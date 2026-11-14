//! Standalone strict Memcheck gate for the exact New/Set/Get-String Native route.
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
    materialize_ordinary_map_set_get_string_route, ORDINARY_MAP_SET_GET_STRING_EXECUTION_PROFILE,
};
use mimi::lexer::Lexer;

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

const EVIDENCE_ENV: &str = "MIMI_ORDINARY_MAP_SET_GET_STRING_L3_EVIDENCE";
const PROFILE: &str = ORDINARY_MAP_SET_GET_STRING_EXECUTION_PROFILE;
const RUNTIME_FEATURE: &str = "ordinary-map-set-result-l3";
const GENERATED_ENTRY: &str = "mimi_l3_generated_main";
const FIXTURE: &str =
    include_str!("fixtures/mir_ordinary_map_owned_any_v2_set_get_string_lineage.mimi");
const TYPE_ID_A: &[u8; 35] = b"rt:00000000000000000000000000000001";
const TYPE_ID_B: &[u8; 35] = b"rt:00000000000000000000000000000002";
const DESCRIPTOR_HASH_A: [u8; 32] = [0xa5; 32];
const DESCRIPTOR_HASH_B: [u8; 32] = [0x5a; 32];
const DRIVER_SOURCE: &str = r#"
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct mimi_set_get_string_l3_observation {
    uint64_t set_calls;
    uint64_t get_calls;
    int32_t set_source_before_status;
    int32_t set_source_before_size;
    int32_t set_source_after_status;
    int32_t set_source_after_size;
    int32_t set_status;
    int32_t set_output_root_zero;
    uint64_t set_roots_after;
    uint64_t set_ordinary_map_handles_after;
    uint64_t set_owned_get_handles_after;
    uint64_t set_managed_any_after;
    uint64_t set_any_owner_tokens_after;
    uint64_t set_owned_strings_after;
    uint64_t set_runtime_map_handles_after;
    uint64_t set_lists_after;
    uint64_t set_root_publish_attempts;
    int32_t get_status;
    int32_t get_outputs_zero;
    int64_t get_output_length;
    uint64_t get_roots_after;
    uint64_t get_ordinary_map_handles_after;
    uint64_t get_owned_get_handles_after;
    uint64_t get_managed_any_after;
    uint64_t get_any_owner_tokens_after;
    uint64_t get_owned_strings_after;
    uint64_t get_runtime_map_handles_after;
    uint64_t get_lists_after;
    uint64_t get_copy_clone_attempts;
    uint64_t get_copy_publish_attempts;
    uint64_t get_registry_registration_attempts;
    uint64_t get_registry_capacity_failures;
    uint64_t get_temporary_string_drops;
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
_Static_assert(sizeof(struct mimi_set_get_string_l3_observation) == 304,
               "Rust/C String L3 observation ABI changed");

struct string_box {
    char *data;
    int64_t length;
};

static const uint8_t type_id_a[35] = { @TYPE_ID_A@ };
static const uint8_t type_id_b[35] = { @TYPE_ID_B@ };
static const uint8_t descriptor_hash_a[32] = { @HASH_A@ };
static const uint8_t descriptor_hash_b[32] = { @HASH_B@ };

extern int32_t mimi_l3_generated_main(void);
extern int32_t mimi_ordinary_map_set_get_string_l3_reset(void);
extern int32_t mimi_ordinary_map_set_get_string_l3_install_set_capacity_failure(void);
extern int32_t mimi_ordinary_map_set_get_string_l3_install_get_capacity_failure(void);
extern int32_t mimi_ordinary_map_set_get_string_l3_read_observation(
    struct mimi_set_get_string_l3_observation *out_observation);
extern int32_t mimi_ordinary_map_owned_any_v2_new(int64_t *out_root);
extern int32_t mimi_ordinary_map_owned_any_v2_size(int64_t handle, int32_t *out_size);
extern int32_t mimi_ordinary_map_owned_any_v2_drop(int64_t handle);
extern int32_t mimi_ordinary_map_owned_any_v2_set_string_result(
    int64_t handle, const uint8_t *key, int64_t key_len,
    const char *value_data, int64_t value_len,
    const uint8_t *type_id, int64_t type_id_len,
    const uint8_t *descriptor_hash,
    int32_t (*clone_fn)(uint8_t *destination, const uint8_t *source),
    void (*drop_fn)(uint8_t *payload), int64_t *out_root);
extern int32_t mimi_ordinary_map_owned_any_v2_get_string_result(
    int64_t handle, const uint8_t *key, int64_t key_len,
    const uint8_t *type_id, int64_t type_id_len,
    const uint8_t *descriptor_hash, char **out_data, int64_t *out_len);
extern void mimi_string_free(char *pointer);

static int32_t clone_string_box(uint8_t *destination, const uint8_t *source) {
    struct string_box input;
    struct string_box output = {0};
    memcpy(&input, source, sizeof(input));
    if (input.length < 0 || input.length > 4096 ||
        (input.length > 0 && input.data == NULL)) {
        return 1;
    }
    if (input.length > 0) {
        output.data = (char *)malloc((size_t)input.length);
        if (output.data == NULL) {
            return 1;
        }
        memcpy(output.data, input.data, (size_t)input.length);
    }
    output.length = input.length;
    memcpy(destination, &output, sizeof(output));
    return 0;
}

static void drop_string_box(uint8_t *payload) {
    struct string_box value;
    memcpy(&value, payload, sizeof(value));
    free(value.data);
}

static int observation_read(struct mimi_set_get_string_l3_observation *observation) {
    return mimi_ordinary_map_set_get_string_l3_read_observation(observation) == 0;
}

static int final_owner_domains_zero(
    struct mimi_set_get_string_l3_observation *observation) {
    return observation_read(observation) && observation->final_roots == 0 &&
           observation->final_ordinary_map_handles == 0 &&
           observation->final_owned_get_handles == 0 &&
           observation->final_runtime_map_handles == 0 &&
           observation->final_managed_any == 0 &&
           observation->final_owned_strings == 0 &&
           observation->final_any_owner_tokens == 0 && observation->final_lists == 0;
}

static void print_final_owner_domains(
    const struct mimi_set_get_string_l3_observation *observation) {
    printf("final-domain-counts roots=%" PRIu64 " ordinary_map_handles=%" PRIu64
           " owned_get_handles=%" PRIu64 " runtime_map_handles=%" PRIu64
           " managed_any=%" PRIu64 " owned_strings=%" PRIu64
           " any_owner_tokens=%" PRIu64 " lists=%" PRIu64 "\n",
           observation->final_roots, observation->final_ordinary_map_handles,
           observation->final_owned_get_handles, observation->final_runtime_map_handles,
           observation->final_managed_any, observation->final_owned_strings,
           observation->final_any_owner_tokens, observation->final_lists);
}

static void print_boundary_observations(
    const struct mimi_set_get_string_l3_observation *observation) {
    if (observation->set_calls != 0) {
        printf("set-boundary source_before=%" PRId32 "/%" PRId32
               " source_after=%" PRId32 "/%" PRId32
               " status=%" PRId32 " output_root_zero=%" PRId32
               " roots=%" PRIu64 " map_handles=%" PRIu64
               " owned_get=%" PRIu64 " managed_any=%" PRIu64
               " any_owners=%" PRIu64 " owned_strings=%" PRIu64
               " runtime_maps=%" PRIu64 " lists=%" PRIu64
               " root_publish_attempts=%" PRIu64 "\n",
               observation->set_source_before_status, observation->set_source_before_size,
               observation->set_source_after_status, observation->set_source_after_size,
               observation->set_status, observation->set_output_root_zero,
               observation->set_roots_after, observation->set_ordinary_map_handles_after,
               observation->set_owned_get_handles_after, observation->set_managed_any_after,
               observation->set_any_owner_tokens_after, observation->set_owned_strings_after,
               observation->set_runtime_map_handles_after, observation->set_lists_after,
               observation->set_root_publish_attempts);
    }
    if (observation->get_calls != 0) {
        printf("get-boundary status=%" PRId32 " outputs_zero=%" PRId32
               " length=%" PRId64 " roots=%" PRIu64 " map_handles=%" PRIu64
               " owned_get=%" PRIu64 " managed_any=%" PRIu64
               " any_owners=%" PRIu64 " owned_strings=%" PRIu64
               " runtime_maps=%" PRIu64 " lists=%" PRIu64
               " copy_clone_attempts=%" PRIu64 " copy_publish_attempts=%" PRIu64
               " registry_attempts=%" PRIu64 " registry_capacity_failures=%" PRIu64
               " temporary_string_drops=%" PRIu64 "\n",
               observation->get_status, observation->get_outputs_zero,
               observation->get_output_length, observation->get_roots_after,
               observation->get_ordinary_map_handles_after,
               observation->get_owned_get_handles_after, observation->get_managed_any_after,
               observation->get_any_owner_tokens_after, observation->get_owned_strings_after,
               observation->get_runtime_map_handles_after, observation->get_lists_after,
               observation->get_copy_clone_attempts, observation->get_copy_publish_attempts,
               observation->get_registry_registration_attempts,
               observation->get_registry_capacity_failures,
               observation->get_temporary_string_drops);
    }
}

static int prepare_string_root(int64_t *source_root, int64_t *updated_root) {
    static const char payload[] = "payload";
    if (mimi_ordinary_map_owned_any_v2_new(source_root) != 0 || *source_root == 0) {
        return 0;
    }
    const int32_t status = mimi_ordinary_map_owned_any_v2_set_string_result(
        *source_root, (const uint8_t *)"answer", 6, payload, 7,
        type_id_a, 35, descriptor_hash_a, clone_string_box, drop_string_box,
        updated_root);
    return status == 0 && *updated_root != 0;
}

static int size_is(int64_t root, int32_t expected) {
    int32_t size = -1;
    return mimi_ordinary_map_owned_any_v2_size(root, &size) == 0 && size == expected;
}

static int run_hit(void) {
    struct mimi_set_get_string_l3_observation observation = {0};
    if (mimi_ordinary_map_set_get_string_l3_reset() != 0) {
        return 81;
    }
    // These snapshots are taken inside the runtime ABI calls, before native
    // code returns to settle its borrowed key and source/result String values.
    const int32_t generated_status = mimi_l3_generated_main();
    if (!observation_read(&observation) || generated_status != 42 ||
        observation.set_calls != 1 || observation.get_calls != 1 ||
        observation.set_source_before_status != 0 || observation.set_source_before_size != 0 ||
        observation.set_source_after_status != 0 || observation.set_source_after_size != 0 ||
        observation.set_status != 0 || observation.set_output_root_zero != 0 ||
        observation.set_roots_after != 2 || observation.set_ordinary_map_handles_after != 2 ||
        observation.set_owned_get_handles_after != 0 ||
        observation.set_any_owner_tokens_after != 1 ||
        observation.set_managed_any_after != 0 || observation.set_owned_strings_after != 2 ||
        observation.set_runtime_map_handles_after != 2 || observation.set_lists_after != 0 ||
        observation.set_root_publish_attempts != 0 ||
        observation.get_status != 0 || observation.get_outputs_zero != 0 ||
        observation.get_output_length != 7 || observation.get_roots_after != 1 ||
        observation.get_ordinary_map_handles_after != 1 ||
        observation.get_owned_get_handles_after != 0 ||
        observation.get_any_owner_tokens_after != 1 || observation.get_owned_strings_after != 2 ||
        observation.get_managed_any_after != 0 || observation.get_runtime_map_handles_after != 1 ||
        observation.get_lists_after != 0 ||
        observation.get_copy_clone_attempts != 1 || observation.get_copy_publish_attempts != 1 ||
        observation.get_registry_registration_attempts != 1 ||
        observation.get_registry_capacity_failures != 0 ||
        observation.get_temporary_string_drops != 1 ||
        observation.shutdown_registered != 1 || !final_owner_domains_zero(&observation)) {
        printf("hit-diagnostic generated_status=%" PRId32 " shutdown_registered=%" PRId32 "\n",
               generated_status, observation.shutdown_registered);
        print_boundary_observations(&observation);
        print_final_owner_domains(&observation);
        fputs("Set/Get-String generated hit or final owner-domain invariant failed\n", stderr);
        return 82;
    }
    printf("mode=hit generated_status=%" PRId32 " set_calls=%" PRIu64
           " set_status=%" PRId32 " set_roots_after=%" PRIu64
           " set_any_owners_after=%" PRIu64 " get_calls=%" PRIu64
           " get_status=%" PRId32 " get_outputs_zero=%" PRId32
           " get_length=%" PRId64 " get_temp_drops=%" PRIu64
           " final_roots=%" PRIu64 " final_owned_strings=%" PRIu64
           " final_any_owner_tokens=%" PRIu64 " final_lists=%" PRIu64 "\n",
           generated_status, observation.set_calls, observation.set_status,
           observation.set_roots_after, observation.set_any_owner_tokens_after,
           observation.get_calls, observation.get_status, observation.get_outputs_zero,
           observation.get_output_length, observation.get_temporary_string_drops,
           observation.final_roots, observation.final_owned_strings,
           observation.final_any_owner_tokens, observation.final_lists);
    print_boundary_observations(&observation);
    print_final_owner_domains(&observation);
    return 0;
}

static int run_set_capacity(void) {
    struct mimi_set_get_string_l3_observation observation = {0};
    static const char payload[] = "payload";
    int64_t source_root = 0;
    int64_t output_root = INT64_C(0x7fffffffffffffff);
    if (mimi_ordinary_map_set_get_string_l3_install_set_capacity_failure() != 0 ||
        mimi_ordinary_map_owned_any_v2_new(&source_root) != 0 || source_root == 0) {
        return 83;
    }
    const int32_t status = mimi_ordinary_map_owned_any_v2_set_string_result(
        source_root, (const uint8_t *)"answer", 6, payload, 7,
        type_id_a, 35, descriptor_hash_a, clone_string_box, drop_string_box,
        &output_root);
    if (status != 2 || output_root != 0 || !size_is(source_root, 0) ||
        !observation_read(&observation) || observation.set_calls != 1 ||
        observation.set_source_before_status != 0 || observation.set_source_before_size != 0 ||
        observation.set_source_after_status != 0 || observation.set_source_after_size != 0 ||
        observation.set_status != 2 || observation.set_output_root_zero != 1 ||
        observation.set_roots_after != 1 || observation.set_ordinary_map_handles_after != 1 ||
        observation.set_owned_get_handles_after != 0 ||
        observation.set_any_owner_tokens_after != 0 ||
        observation.set_managed_any_after != 0 || observation.set_owned_strings_after != 0 ||
        observation.set_runtime_map_handles_after != 1 || observation.set_lists_after != 0 ||
        observation.set_root_publish_attempts != 2) {
        fputs("Set/Get-String Set capacity failure changed its source or published output\n", stderr);
        return 84;
    }
    if (mimi_ordinary_map_owned_any_v2_drop(source_root) != 0 ||
        !final_owner_domains_zero(&observation)) {
        fputs("Set/Get-String Set capacity source settlement failed\n", stderr);
        return 85;
    }
    printf("mode=set-capacity status=%" PRId32 " output_root_zero=1"
           " source_before_size=%" PRId32 " source_after_size=%" PRId32
           " root_publish_attempts=%" PRIu64 " final_roots=%" PRIu64
           " final_any_owner_tokens=%" PRIu64 " final_lists=%" PRIu64 "\n",
           status, observation.set_source_before_size, observation.set_source_after_size,
           observation.set_root_publish_attempts, observation.final_roots,
           observation.final_any_owner_tokens, observation.final_lists);
    print_boundary_observations(&observation);
    print_final_owner_domains(&observation);
    return 0;
}

static int run_get_capacity(void) {
    struct mimi_set_get_string_l3_observation observation = {0};
    int64_t source_root = 0;
    int64_t updated_root = 0;
    char *output = (char *)(uintptr_t)1;
    int64_t output_length = -1;
    if (!prepare_string_root(&source_root, &updated_root) ||
        mimi_ordinary_map_owned_any_v2_drop(source_root) != 0 ||
        mimi_ordinary_map_set_get_string_l3_install_get_capacity_failure() != 0) {
        return 86;
    }
    const int32_t status = mimi_ordinary_map_owned_any_v2_get_string_result(
        updated_root, (const uint8_t *)"answer", 6,
        type_id_a, 35, descriptor_hash_a, &output, &output_length);
    if (status != 3 || output != NULL || output_length != 0 ||
        !observation_read(&observation) || observation.set_calls != 0 ||
        observation.get_calls != 1 || observation.get_status != 3 ||
        observation.get_outputs_zero != 1 || observation.get_output_length != 0 ||
        observation.get_roots_after != 1 || observation.get_ordinary_map_handles_after != 1 ||
        observation.get_owned_get_handles_after != 0 ||
        observation.get_any_owner_tokens_after != 1 ||
        observation.get_managed_any_after != 0 || observation.get_runtime_map_handles_after != 1 ||
        observation.get_lists_after != 0 || observation.get_owned_strings_after != 0 ||
        observation.get_copy_clone_attempts != 1 || observation.get_copy_publish_attempts != 1 ||
        observation.get_registry_registration_attempts != 1 ||
        observation.get_registry_capacity_failures != 1 ||
        observation.get_temporary_string_drops != 1 || !size_is(updated_root, 1)) {
        fputs("Set/Get-String fresh String registration Capacity leaked or changed an owner\n", stderr);
        return 87;
    }
    if (mimi_ordinary_map_owned_any_v2_drop(updated_root) != 0 ||
        !final_owner_domains_zero(&observation)) {
        fputs("Set/Get-String Get capacity root settlement failed\n", stderr);
        return 88;
    }
    printf("mode=get-capacity status=%" PRId32 " outputs_zero=1"
           " copy_clone_attempts=%" PRIu64 " copy_publish_attempts=%" PRIu64
           " temporary_string_drops=%" PRIu64 " final_roots=%" PRIu64
           " final_owned_strings=%" PRIu64 " final_any_owner_tokens=%" PRIu64
           " final_lists=%" PRIu64 "\n",
           status, observation.get_copy_clone_attempts, observation.get_copy_publish_attempts,
           observation.get_temporary_string_drops, observation.final_roots,
           observation.final_owned_strings, observation.final_any_owner_tokens,
           observation.final_lists);
    print_boundary_observations(&observation);
    print_final_owner_domains(&observation);
    return 0;
}

static int run_missing(void) {
    struct mimi_set_get_string_l3_observation observation = {0};
    int64_t root = 0;
    char *output = (char *)(uintptr_t)1;
    int64_t output_length = -1;
    if (mimi_ordinary_map_set_get_string_l3_reset() != 0 ||
        mimi_ordinary_map_owned_any_v2_new(&root) != 0 || root == 0) {
        return 89;
    }
    const int32_t status = mimi_ordinary_map_owned_any_v2_get_string_result(
        root, (const uint8_t *)"answer", 6,
        type_id_a, 35, descriptor_hash_a, &output, &output_length);
    if (status != 1 || output != NULL || output_length != 0 ||
        !observation_read(&observation) || observation.get_calls != 1 ||
        observation.get_status != 1 || observation.get_outputs_zero != 1 ||
        observation.get_roots_after != 1 || observation.get_ordinary_map_handles_after != 1 ||
        observation.get_owned_get_handles_after != 0 ||
        observation.get_runtime_map_handles_after != 1 || observation.get_lists_after != 0 ||
        observation.get_copy_clone_attempts != 0 || observation.get_copy_publish_attempts != 0 ||
        observation.get_registry_registration_attempts != 0 ||
        observation.get_registry_capacity_failures != 0 ||
        observation.get_temporary_string_drops != 0 || !size_is(root, 0) ||
        mimi_ordinary_map_owned_any_v2_drop(root) != 0 ||
        !final_owner_domains_zero(&observation)) {
        fputs("Set/Get-String missing lookup changed its source owner or output\n", stderr);
        return 90;
    }
    printf("mode=missing status=%" PRId32 " outputs_zero=1 final_roots=%" PRIu64
           " final_any_owner_tokens=%" PRIu64 " final_lists=%" PRIu64 "\n",
           status, observation.final_roots, observation.final_any_owner_tokens,
           observation.final_lists);
    print_boundary_observations(&observation);
    print_final_owner_domains(&observation);
    return 0;
}

static int run_descriptor_mismatch(void) {
    struct mimi_set_get_string_l3_observation observation = {0};
    int64_t source_root = 0;
    int64_t updated_root = 0;
    char *output = (char *)(uintptr_t)1;
    int64_t output_length = -1;
    if (mimi_ordinary_map_set_get_string_l3_reset() != 0 ||
        !prepare_string_root(&source_root, &updated_root) ||
        !size_is(source_root, 0) || mimi_ordinary_map_owned_any_v2_drop(source_root) != 0) {
        return 91;
    }
    const int32_t status = mimi_ordinary_map_owned_any_v2_get_string_result(
        updated_root, (const uint8_t *)"answer", 6,
        type_id_b, 35, descriptor_hash_b, &output, &output_length);
    if (status != 2 || output != NULL || output_length != 0 ||
        !observation_read(&observation) || observation.get_calls != 1 ||
        observation.get_status != 2 || observation.get_outputs_zero != 1 ||
        observation.get_roots_after != 1 || observation.get_ordinary_map_handles_after != 1 ||
        observation.get_owned_get_handles_after != 0 ||
        observation.get_runtime_map_handles_after != 1 || observation.get_lists_after != 0 ||
        observation.get_any_owner_tokens_after != 1 ||
        observation.get_copy_clone_attempts != 0 || observation.get_copy_publish_attempts != 0 ||
        observation.get_registry_registration_attempts != 0 ||
        observation.get_registry_capacity_failures != 0 ||
        observation.get_temporary_string_drops != 0 || !size_is(updated_root, 1) ||
        mimi_ordinary_map_owned_any_v2_drop(updated_root) != 0 ||
        !final_owner_domains_zero(&observation)) {
        fputs("Set/Get-String descriptor mismatch changed its source owner or output\n", stderr);
        return 92;
    }
    printf("mode=descriptor-mismatch status=%" PRId32 " outputs_zero=1"
           " get_any_owner_tokens_after=%" PRIu64 " final_roots=%" PRIu64
           " final_any_owner_tokens=%" PRIu64 " final_lists=%" PRIu64 "\n",
           status, observation.get_any_owner_tokens_after, observation.final_roots,
           observation.final_any_owner_tokens, observation.final_lists);
    print_boundary_observations(&observation);
    print_final_owner_domains(&observation);
    return 0;
}

static int run_detached(void) {
    struct mimi_set_get_string_l3_observation observation = {0};
    static const char payload[] = "payload";
    int64_t source_root = 0;
    int64_t updated_root = 0;
    char *output = NULL;
    int64_t output_length = 0;
    if (mimi_ordinary_map_set_get_string_l3_reset() != 0 ||
        !prepare_string_root(&source_root, &updated_root)) {
        return 93;
    }
    const int32_t status = mimi_ordinary_map_owned_any_v2_get_string_result(
        updated_root, (const uint8_t *)"answer", 6,
        type_id_a, 35, descriptor_hash_a, &output, &output_length);
    if (status != 0 || output == NULL || output == payload || output_length != 7 ||
        memcmp(output, payload, 7) != 0 ||
        mimi_ordinary_map_owned_any_v2_drop(source_root) != 0 ||
        mimi_ordinary_map_owned_any_v2_drop(updated_root) != 0 ||
        !observation_read(&observation) || observation.get_status != 0 ||
        observation.get_outputs_zero != 0 || observation.get_temporary_string_drops != 1 ||
        observation.get_copy_clone_attempts != 1 || observation.get_copy_publish_attempts != 1 ||
        observation.get_registry_registration_attempts != 1 ||
        observation.get_registry_capacity_failures != 0 ||
        observation.final_roots != 0 || observation.final_any_owner_tokens != 0 ||
        observation.final_owned_strings != 1 || memcmp(output, payload, 7) != 0) {
        fputs("Set/Get-String returned copy did not survive all Map root drops\n", stderr);
        return 94;
    }
    const uint64_t roots_after_map_drop = observation.final_roots;
    const uint64_t strings_before_free = observation.final_owned_strings;
    mimi_string_free(output);
    if (!final_owner_domains_zero(&observation)) {
        fputs("Set/Get-String detached String owner did not settle\n", stderr);
        return 95;
    }
    printf("mode=detached status=%" PRId32 " copied_value_survived=true"
           " roots_after_map_drop=%" PRIu64 " strings_before_free=%" PRIu64
           " temporary_string_drops=%" PRIu64 " final_roots=%" PRIu64
           " final_owned_strings=%" PRIu64 " final_any_owner_tokens=%" PRIu64
           " final_lists=%" PRIu64 "\n",
           status, roots_after_map_drop, strings_before_free,
           observation.get_temporary_string_drops, observation.final_roots,
           observation.final_owned_strings, observation.final_any_owner_tokens,
           observation.final_lists);
    print_boundary_observations(&observation);
    print_final_owner_domains(&observation);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fputs("expected one L3 child mode\n", stderr);
        return 80;
    }
    if (strcmp(argv[1], "hit") == 0) return run_hit();
    if (strcmp(argv[1], "set-capacity") == 0) return run_set_capacity();
    if (strcmp(argv[1], "get-capacity") == 0) return run_get_capacity();
    if (strcmp(argv[1], "missing") == 0) return run_missing();
    if (strcmp(argv[1], "descriptor-mismatch") == 0) return run_descriptor_mismatch();
    if (strcmp(argv[1], "detached") == 0) return run_detached();
    fputs("unknown L3 child mode\n", stderr);
    return 80;
}
"#;
fn main() {
    if let Err(error) = run_parent() {
        eprintln!("ordinary-map-set-get-string-l3: {error}");
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
    let repo_canonical = repo_root
        .canonicalize()
        .map_err(|error| format!("canonicalize repository root: {error}"))?;
    let evidence_canonical = evidence_root
        .canonicalize()
        .map_err(|error| format!("canonicalize evidence root: {error}"))?;
    if evidence_canonical.starts_with(&repo_canonical) {
        return Err(format!(
            "String L3 evidence must be outside the repository: {}",
            evidence_canonical.display()
        ));
    }
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "CARGO_TARGET_DIR must point to external scratch storage".to_owned())?;
    let target_canonical = target_dir
        .canonicalize()
        .map_err(|error| format!("canonicalize external CARGO_TARGET_DIR: {error}"))?;
    if target_canonical.starts_with(&repo_canonical) {
        return Err(format!(
            "String L3 Cargo target must be outside the repository: {}",
            target_canonical.display()
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
    write_text(
        &attempt_root.join("runner-environment.txt"),
        &format!(
            "repository={}\nevidence_root={}\nCARGO_TARGET_DIR={}\nLLVM_SYS_181_PREFIX={}\n",
            repo_canonical.display(),
            evidence_canonical.display(),
            target_canonical.display(),
            std::env::var("LLVM_SYS_181_PREFIX").unwrap_or_else(|_| "<unset>".into())
        ),
    )?;
    let before = source_snapshot(&repo_root)?;
    write_text(&attempt_root.join("source-hashes-before.sha256"), &before)?;
    write_tool_hashes(&attempt_root)?;

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
        "ordinary-map-set-get-string-l3 completed; strict Memcheck evidence: {}",
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
    let fixture_path = attempt_root.join("shared-set-get-string-fixture.mimi");
    let shared_fixture_path =
        repo_root.join("tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_string_lineage.mimi");
    let current_fixture = fs::read_to_string(&shared_fixture_path).map_err(|error| {
        format!(
            "read exact shared Set/Get-String fixture {}: {error}",
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
        .map_err(|error| format!("lex exact shared Set/Get-String fixture: {error}"))?;
    let parser = mimi::loader::parser_for_path(tokens, &shared_fixture_path).map_err(|error| {
        format!("register exact shared Set/Get-String source identity: {error}")
    })?;
    let file = parser
        .parse_file()
        .map_err(|error| format!("parse exact shared Set/Get-String fixture: {error}"))?;
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
    let program = materialize_ordinary_map_set_get_string_route(&checked)
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
            "route_profile={PROFILE}\nmir_digest={mir_digest}\nroute_identity_digest={receipt_identity_digest}\nsource_fixture=tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_string_lineage.mimi\n"
        ),
    )?;

    let object_path = build_root.join("set-get-string-native.o");
    let ir_path = build_root.join("set-get-string-native.ll");
    compile_checked_native_object(&checked, &program, &receipt, &object_path, &ir_path)?;
    verify_default_and_explicit_cli_routes(
        attempt_root,
        command_root,
        &shared_fixture_path,
        &mir_digest,
        &receipt_identity_digest,
    )?;

    let runtime_archive = build_root.join("libmimi_runtime_set_get_string_l3.a");
    compile_runtime_archive(
        &repo_root.join("src/runtime/standalone.rs"),
        &runtime_archive,
        repo_root,
        command_root,
    )?;
    let driver_source_path = build_root.join("set-get-string-l3-driver.c");
    write_text(&driver_source_path, &driver_source())?;
    let driver_object = build_root.join("set-get-string-l3-driver.o");
    compile_c_driver(&driver_source_path, &driver_object, repo_root, command_root)?;
    let native_binary = build_root.join("native-set-get-string-l3");
    link_native_executable(
        &object_path,
        &driver_object,
        &runtime_archive,
        &native_binary,
        repo_root,
        command_root,
    )?;
    for mode in [
        "hit",
        "set-capacity",
        "get-capacity",
        "missing",
        "descriptor-mismatch",
        "detached",
    ] {
        fs::create_dir(attempt_root.join("memcheck").join(mode))
            .map_err(|error| format!("create Memcheck evidence directory {mode}: {error}"))?;
    }
    for mode in [
        "hit",
        "set-capacity",
        "get-capacity",
        "missing",
        "descriptor-mismatch",
        "detached",
    ] {
        run_strict_memcheck(
            &native_binary,
            mode,
            &attempt_root.join("memcheck").join(mode),
            repo_root,
            command_root,
        )?;
    }
    Ok(())
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
        let mut generator = CodeGenerator::new(&context, "native_map_set_get_string_l3_artifact");
        generator
            .compile_checked_mir_native_with_ordinary_map_set_get_string_route(
                checked, program, receipt,
            )
            .map_err(|errors| {
                format!("exact native Set/Get-String route emission failed: {errors:?}")
            })?;
        generator
            .module
            .verify()
            .map_err(|error| format!("verify exact native Set/Get-String module: {error}"))?;
        let main = generator
            .module
            .get_function("main")
            .ok_or_else(|| "exact native Set/Get-String module lacks main".to_owned())?;
        if main.count_params() != 0
            || main.get_type().get_return_type() != Some(context.i32_type().into())
        {
            return Err("exact native Set/Get-String main ABI is not () -> i32".into());
        }
        rename_function(&main, GENERATED_ENTRY);
        generator
            .module
            .verify()
            .map_err(|error| format!("verify renamed native Set/Get-String module: {error}"))?;
        generator
            .module
            .print_to_file(ir_path)
            .map_err(|error| format!("write native Set/Get-String LLVM IR: {error}"))?;
        generator
            .compile_to_object(object_path)
            .map_err(|error| format!("emit native Set/Get-String object: {error}"))
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
    let mut admitted_lines = Vec::new();
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
        admitted_lines.push(format!("{mode}: {route_line}"));
    }
    if admitted_lines.len() != 2 {
        return Err(
            "default and explicit --mir admission identities were not both recorded".into(),
        );
    }
    write_text(
        &attempt_root.join("cli-route-admission-identities.txt"),
        &format!("{}\n{}\n", admitted_lines[0], admitted_lines[1]),
    )?;
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
        "native Set/Get-String executable link",
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
    mode: &str,
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
        OsString::from(mode),
    ];
    let output = run_command(
        &format!("valgrind-{mode}"),
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
    write_text(
        &evidence_directory.join("still-reachable.txt"),
        &format!(
            "{} bytes in {} blocks\n",
            summary.still_reachable_bytes, summary.still_reachable_blocks
        ),
    )?;
    if !output.status.success() {
        return Err(format!(
            "strict native Memcheck failed: stdout={} stderr={} report={report}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let expected_stdout = match mode {
        "hit" => concat!(
            "mode=hit generated_status=42 set_calls=1 set_status=0 set_roots_after=2 ",
            "set_any_owners_after=1 get_calls=1 get_status=0 get_outputs_zero=0 ",
            "get_length=7 get_temp_drops=1 final_roots=0 final_owned_strings=0 ",
            "final_any_owner_tokens=0 final_lists=0\n",
            "set-boundary source_before=0/0 source_after=0/0 status=0 output_root_zero=0 ",
            "roots=2 map_handles=2 owned_get=0 managed_any=0 any_owners=1 ",
            "owned_strings=2 runtime_maps=2 lists=0 root_publish_attempts=0\n",
            "get-boundary status=0 outputs_zero=0 length=7 roots=1 map_handles=1 ",
            "owned_get=0 managed_any=0 any_owners=1 owned_strings=2 runtime_maps=1 ",
            "lists=0 copy_clone_attempts=1 copy_publish_attempts=1 registry_attempts=1 ",
            "registry_capacity_failures=0 temporary_string_drops=1\n",
            "final-domain-counts roots=0 ordinary_map_handles=0 owned_get_handles=0 ",
            "runtime_map_handles=0 managed_any=0 owned_strings=0 any_owner_tokens=0 lists=0\n"
        ),
        "set-capacity" => concat!(
            "mode=set-capacity status=2 output_root_zero=1 source_before_size=0 ",
            "source_after_size=0 root_publish_attempts=2 final_roots=0 ",
            "final_any_owner_tokens=0 final_lists=0\n",
            "set-boundary source_before=0/0 source_after=0/0 status=2 output_root_zero=1 ",
            "roots=1 map_handles=1 owned_get=0 managed_any=0 any_owners=0 ",
            "owned_strings=0 runtime_maps=1 lists=0 root_publish_attempts=2\n",
            "final-domain-counts roots=0 ordinary_map_handles=0 owned_get_handles=0 ",
            "runtime_map_handles=0 managed_any=0 owned_strings=0 any_owner_tokens=0 lists=0\n"
        ),
        "get-capacity" => concat!(
            "mode=get-capacity status=3 outputs_zero=1 copy_clone_attempts=1 ",
            "copy_publish_attempts=1 temporary_string_drops=1 final_roots=0 ",
            "final_owned_strings=0 final_any_owner_tokens=0 final_lists=0\n",
            "get-boundary status=3 outputs_zero=1 length=0 roots=1 map_handles=1 ",
            "owned_get=0 managed_any=0 any_owners=1 owned_strings=0 runtime_maps=1 ",
            "lists=0 copy_clone_attempts=1 copy_publish_attempts=1 registry_attempts=1 ",
            "registry_capacity_failures=1 temporary_string_drops=1\n",
            "final-domain-counts roots=0 ordinary_map_handles=0 owned_get_handles=0 ",
            "runtime_map_handles=0 managed_any=0 owned_strings=0 any_owner_tokens=0 lists=0\n"
        ),
        "missing" => concat!(
            "mode=missing status=1 outputs_zero=1 final_roots=0 ",
            "final_any_owner_tokens=0 final_lists=0\n",
            "get-boundary status=1 outputs_zero=1 length=0 roots=1 map_handles=1 ",
            "owned_get=0 managed_any=0 any_owners=0 owned_strings=0 runtime_maps=1 ",
            "lists=0 copy_clone_attempts=0 copy_publish_attempts=0 registry_attempts=0 ",
            "registry_capacity_failures=0 temporary_string_drops=0\n",
            "final-domain-counts roots=0 ordinary_map_handles=0 owned_get_handles=0 ",
            "runtime_map_handles=0 managed_any=0 owned_strings=0 any_owner_tokens=0 lists=0\n"
        ),
        "descriptor-mismatch" => concat!(
            "mode=descriptor-mismatch status=2 outputs_zero=1 ",
            "get_any_owner_tokens_after=1 final_roots=0 ",
            "final_any_owner_tokens=0 final_lists=0\n",
            "set-boundary source_before=0/0 source_after=0/0 status=0 output_root_zero=0 ",
            "roots=2 map_handles=2 owned_get=0 managed_any=0 any_owners=1 ",
            "owned_strings=0 runtime_maps=2 lists=0 root_publish_attempts=0\n",
            "get-boundary status=2 outputs_zero=1 length=0 roots=1 map_handles=1 ",
            "owned_get=0 managed_any=0 any_owners=1 owned_strings=0 runtime_maps=1 ",
            "lists=0 copy_clone_attempts=0 copy_publish_attempts=0 registry_attempts=0 ",
            "registry_capacity_failures=0 temporary_string_drops=0\n",
            "final-domain-counts roots=0 ordinary_map_handles=0 owned_get_handles=0 ",
            "runtime_map_handles=0 managed_any=0 owned_strings=0 any_owner_tokens=0 lists=0\n"
        ),
        "detached" => concat!(
            "mode=detached status=0 copied_value_survived=true roots_after_map_drop=0 ",
            "strings_before_free=1 temporary_string_drops=1 final_roots=0 ",
            "final_owned_strings=0 final_any_owner_tokens=0 final_lists=0\n",
            "set-boundary source_before=0/0 source_after=0/0 status=0 output_root_zero=0 ",
            "roots=2 map_handles=2 owned_get=0 managed_any=0 any_owners=1 ",
            "owned_strings=0 runtime_maps=2 lists=0 root_publish_attempts=0\n",
            "get-boundary status=0 outputs_zero=0 length=7 roots=2 map_handles=2 ",
            "owned_get=0 managed_any=0 any_owners=1 owned_strings=1 runtime_maps=2 ",
            "lists=0 copy_clone_attempts=1 copy_publish_attempts=1 registry_attempts=1 ",
            "registry_capacity_failures=0 temporary_string_drops=1\n",
            "final-domain-counts roots=0 ordinary_map_handles=0 owned_get_handles=0 ",
            "runtime_map_handles=0 managed_any=0 owned_strings=0 any_owner_tokens=0 lists=0\n"
        ),
        _ => return Err(format!("unknown strict Memcheck mode {mode:?}")),
    };
    if output.stdout.as_slice() != expected_stdout.as_bytes() {
        return Err(format!(
            "native L3 output differs\nexpected: {expected_stdout:?}\nactual: {:?}",
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    Ok(())
}

fn driver_source() -> String {
    DRIVER_SOURCE
        .replace("@TYPE_ID_A@", &c_bytes(TYPE_ID_A))
        .replace("@TYPE_ID_B@", &c_bytes(TYPE_ID_B))
        .replace("@HASH_A@", &c_bytes(&DESCRIPTOR_HASH_A))
        .replace("@HASH_B@", &c_bytes(&DESCRIPTOR_HASH_B))
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
        repo_root.join("tests/ordinary_map_set_get_string_l3.rs"),
        repo_root.join("tests/support/memcheck_summary.rs"),
        repo_root.join("tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_string_lineage.mimi"),
        repo_root.join("scripts/verify-ordinary-map-set-get-string-l3.sh"),
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

fn write_tool_hashes(attempt_root: &Path) -> Result<(), String> {
    let mut tools = Vec::new();
    for name in ["cargo", "rustc", "cc", "valgrind", "sha256sum"] {
        tools.push(resolve_executable(name)?);
    }
    tools.push(
        std::env::current_exe()
            .map_err(|error| format!("resolve L3 runner executable: {error}"))?
            .canonicalize()
            .map_err(|error| format!("canonicalize L3 runner executable: {error}"))?,
    );
    tools.push(
        PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
            .canonicalize()
            .map_err(|error| format!("canonicalize Mimi CLI executable: {error}"))?,
    );
    tools.sort();
    tools.dedup();
    let mut command = String::from("sha256sum --");
    for tool in &tools {
        command.push(' ');
        command.push_str(&shell_quote(&tool.to_string_lossy()));
    }
    command.push('\n');
    write_text(&attempt_root.join("tool-hashes.command.txt"), &command)?;
    let output = Command::new("sha256sum")
        .arg("--")
        .args(&tools)
        .output()
        .map_err(|error| format!("hash tool executables: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tool executable hashing failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    write_bytes(&attempt_root.join("tool-hashes.sha256"), &output.stdout)
}

fn resolve_executable(name: &str) -> Result<PathBuf, String> {
    let path_variable = std::env::var_os("PATH")
        .ok_or_else(|| "PATH is unset while recording tool hashes".to_owned())?;
    for directory in std::env::split_paths(&path_variable) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|error| format!("canonicalize tool {name}: {error}"));
        }
    }
    Err(format!("tool {name} is unavailable on PATH"))
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
