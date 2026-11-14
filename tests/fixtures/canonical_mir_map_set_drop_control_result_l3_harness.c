#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>

struct mimi_map_set_result_i32_boundary {
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

struct mimi_map_set_result_i32_observation {
    struct mimi_map_set_result_i32_boundary new_boundary;
    struct mimi_map_set_result_i32_boundary set_boundary;
    struct mimi_map_set_result_i32_boundary get_boundary;
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

_Static_assert(sizeof(struct mimi_map_set_result_i32_boundary) == 120,
               "feature-only Result New boundary ABI changed");
_Static_assert(sizeof(struct mimi_map_set_result_i32_observation) == 432,
               "feature-only Result New observation ABI changed");

struct mimi_map_set_drop_control_trace {
    uint64_t generated_new_calls;
    int32_t generated_new_status;
    int32_t generated_new_output_zero;
    uint64_t generated_set_calls;
    int32_t generated_set_status;
    int32_t generated_set_output_zero;
    int32_t generated_set_source_before_status;
    int32_t generated_set_source_before_size;
    int32_t generated_set_source_after_status;
    int32_t generated_set_source_after_size;
};

static struct mimi_map_set_drop_control_trace trace = {
    .generated_new_status = INT32_MIN,
    .generated_new_output_zero = -1,
    .generated_set_status = INT32_MIN,
    .generated_set_output_zero = -1,
    .generated_set_source_before_status = INT32_MIN,
    .generated_set_source_before_size = INT32_MIN,
    .generated_set_source_after_status = INT32_MIN,
    .generated_set_source_after_size = INT32_MIN,
};

extern int32_t mimi_l3_generated_main(void);
extern int32_t mimi_ordinary_map_set_result_l3_install_capacity_failure(void);
extern int32_t mimi_ordinary_map_set_get_any_result_i32_l3_reset(void);
extern int32_t mimi_ordinary_map_set_get_any_result_i32_l3_read_observation(
    struct mimi_map_set_result_i32_observation *out_observation);

extern int32_t mimi_ordinary_map_owned_any_v2_new(int64_t *out_root);
extern int32_t mimi_ordinary_map_owned_any_v2_drop(int64_t root);
extern int32_t mimi_ordinary_map_owned_any_v2_size(int64_t root, int32_t *out_size);
extern int32_t mimi_ordinary_map_owned_any_v2_set(
    int64_t source_root,
    const uint8_t *key,
    int64_t key_len,
    int64_t payload_owner,
    const uint8_t *selection_bytes,
    int64_t selection_len,
    const uint8_t *selection_hash32,
    int64_t *out_root);

extern int32_t __real_mimi_ordinary_map_owned_any_v2_new(int64_t *out_root);
extern int32_t __real_mimi_ordinary_map_owned_any_v2_set(
    int64_t source_root,
    const uint8_t *key,
    int64_t key_len,
    int64_t payload_owner,
    const uint8_t *selection_bytes,
    int64_t selection_len,
    const uint8_t *selection_hash32,
    int64_t *out_root);

int32_t __wrap_mimi_ordinary_map_owned_any_v2_new(int64_t *out_root) {
    trace.generated_new_calls++;
    const int32_t status = __real_mimi_ordinary_map_owned_any_v2_new(out_root);
    trace.generated_new_status = status;
    trace.generated_new_output_zero = out_root == NULL ? -1 : (*out_root == 0);
    return status;
}

int32_t __wrap_mimi_ordinary_map_owned_any_v2_set(
    int64_t source_root,
    const uint8_t *key,
    int64_t key_len,
    int64_t payload_owner,
    const uint8_t *selection_bytes,
    int64_t selection_len,
    const uint8_t *selection_hash32,
    int64_t *out_root) {
    int32_t source_size = INT32_MIN;
    trace.generated_set_calls++;
    trace.generated_set_source_before_status =
        mimi_ordinary_map_owned_any_v2_size(source_root, &source_size);
    trace.generated_set_source_before_size = source_size;

    const int32_t status = __real_mimi_ordinary_map_owned_any_v2_set(
        source_root, key, key_len, payload_owner, selection_bytes, selection_len,
        selection_hash32, out_root);
    trace.generated_set_status = status;
    trace.generated_set_output_zero = out_root == NULL ? -1 : (*out_root == 0);

    source_size = INT32_MIN;
    trace.generated_set_source_after_status =
        mimi_ordinary_map_owned_any_v2_size(source_root, &source_size);
    trace.generated_set_source_after_size = source_size;
    return status;
}

static int final_domains_are_zero(
    const struct mimi_map_set_result_i32_observation *observation) {
    return observation->final_roots == 0 &&
           observation->final_ordinary_map_handles == 0 &&
           observation->final_owned_get_handles == 0 &&
           observation->final_runtime_map_handles == 0 &&
           observation->final_managed_any == 0 &&
           observation->final_owned_strings == 0 &&
           observation->final_any_owner_tokens == 0 &&
           observation->final_lists == 0;
}

static int boundary_roots_match(
    const struct mimi_map_set_result_i32_boundary *boundary,
    uint64_t roots,
    uint64_t map_handles,
    uint64_t runtime_map_handles,
    uint64_t root_publish_attempts) {
    return boundary->roots == roots &&
           boundary->ordinary_map_handles == map_handles &&
           boundary->owned_get_handles == 0 &&
           boundary->runtime_map_handles == runtime_map_handles &&
           boundary->managed_any == 0 &&
           boundary->any_owner_tokens == 0 &&
           boundary->lists == 0 &&
           boundary->root_publish_attempts == root_publish_attempts;
}

int main(void) {
#if MIMI_L3_NEW_ERR
    const char *mode = "new-err";
    const int32_t expected_generated_status = 3;
    int64_t warmup_root = 0;
    int32_t warmup_new_status = INT32_MIN;
    int32_t warmup_drop_status = INT32_MIN;
#else
    const char *mode = "set-err";
    const int32_t expected_generated_status = 2;
#endif

    if (mimi_ordinary_map_set_get_any_result_i32_l3_reset() != 0 ||
        mimi_ordinary_map_set_result_l3_install_capacity_failure() != 0) {
        fputs("MapSetDropControl Result L3 failpoint installation failed\n", stderr);
        return 90;
    }

#if MIMI_L3_NEW_ERR
    /*
     * Feature-only harness preconditioning: the existing standalone L3
     * installer fails after one successful root publication. Consume that
     * publication through the production runtime New and Drop ABIs, then call
     * generated main so its own MapNew is Err(3). No production runtime
     * behavior or failpoint API is changed for this precondition.
     */
    warmup_new_status = __real_mimi_ordinary_map_owned_any_v2_new(&warmup_root);
    if (warmup_new_status != 0 || warmup_root == 0) {
        fputs("MapSetDropControl NewErr harness warm-up New failed\n", stderr);
        return 91;
    }
    warmup_drop_status = mimi_ordinary_map_owned_any_v2_drop(warmup_root);
    if (warmup_drop_status != 0) {
        fputs("MapSetDropControl NewErr harness warm-up Drop failed\n", stderr);
        return 92;
    }
#endif

    const int32_t generated_status = mimi_l3_generated_main();
    struct mimi_map_set_result_i32_observation observation = {0};
    const int32_t observation_status =
        mimi_ordinary_map_set_get_any_result_i32_l3_read_observation(&observation);

    const int expected_new_calls = MIMI_L3_NEW_ERR ? 2 : 1;
    const int32_t expected_new_status = MIMI_L3_NEW_ERR ? 3 : 0;
    const int expected_new_output_zero = MIMI_L3_NEW_ERR ? 1 : 0;
    const uint64_t expected_new_root_publish_calls = MIMI_L3_NEW_ERR ? 2 : 1;
    const uint64_t expected_new_roots = MIMI_L3_NEW_ERR ? 0 : 1;
    const uint64_t expected_new_map_handles = MIMI_L3_NEW_ERR ? 0 : 1;
    const uint64_t expected_new_runtime_map_handles = MIMI_L3_NEW_ERR ? 0 : 1;
    const uint64_t expected_generated_new_calls = 1;
    const int32_t expected_generated_new_status = MIMI_L3_NEW_ERR ? 3 : 0;
    const int expected_generated_new_output_zero = MIMI_L3_NEW_ERR ? 1 : 0;
    const uint64_t expected_generated_set_calls = MIMI_L3_NEW_ERR ? 0 : 1;
    const int32_t expected_generated_set_status = MIMI_L3_NEW_ERR ? INT32_MIN : 2;
    const int expected_generated_set_output_zero = MIMI_L3_NEW_ERR ? -1 : 1;
    const int32_t expected_set_source_status = MIMI_L3_NEW_ERR ? INT32_MIN : 0;
    const int32_t expected_set_source_size = MIMI_L3_NEW_ERR ? INT32_MIN : 0;

    const int observation_matches =
        observation_status == 0 &&
        observation.new.calls == (uint64_t)expected_new_calls &&
        observation.new.status == expected_new_status &&
        observation.new.output_zero == expected_new_output_zero &&
        boundary_roots_match(
            &observation.new,
            expected_new_roots,
            expected_new_map_handles,
            expected_new_runtime_map_handles,
            expected_new_root_publish_calls) &&
        observation.set.calls == 0 && observation.get.calls == 0 &&
        observation.shutdown_registered == 1 && final_domains_are_zero(&observation);
    const int trace_matches =
        trace.generated_new_calls == expected_generated_new_calls &&
        trace.generated_new_status == expected_generated_new_status &&
        trace.generated_new_output_zero == expected_generated_new_output_zero &&
        trace.generated_set_calls == expected_generated_set_calls &&
        trace.generated_set_status == expected_generated_set_status &&
        trace.generated_set_output_zero == expected_generated_set_output_zero &&
        trace.generated_set_source_before_status == expected_set_source_status &&
        trace.generated_set_source_before_size == expected_set_source_size &&
        trace.generated_set_source_after_status == expected_set_source_status &&
        trace.generated_set_source_after_size == expected_set_source_size;

    printf("native-map-set-drop-control-result mode=%s generated_status=%" PRId32,
           mode, generated_status);
#if MIMI_L3_NEW_ERR
    printf(" warmup_new_status=%" PRId32 " warmup_root_nonzero=%d"
           " warmup_drop_status=%" PRId32,
           warmup_new_status, warmup_root != 0, warmup_drop_status);
#endif
    printf(" fixture_new_calls=%" PRIu64 " fixture_new_status=%" PRId32
           " fixture_new_output_zero=%d fixture_set_calls=%" PRIu64
           " fixture_set_status=%" PRId32 " fixture_set_output_zero=%d"
           " fixture_set_source_before=%" PRId32 "/%" PRId32
           " fixture_set_source_after=%" PRId32 "/%" PRId32
           " new_observer_calls=%" PRIu64 " new_observer_status=%" PRId32
           " new_observer_output_zero=%d new_roots_after_new=%" PRIu64
           " new_publish_checkpoints=%" PRIu64 " shutdown_registered=%" PRId32
           " final_roots=%" PRIu64 " final_map_handles=%" PRIu64
           " final_owned_get_handles=%" PRIu64 " final_runtime_map_handles=%" PRIu64
           " final_managed_any=%" PRIu64 " final_owned_strings=%" PRIu64
           " final_any_owner_tokens=%" PRIu64 " final_lists=%" PRIu64 "\n",
           trace.generated_new_calls, trace.generated_new_status,
           trace.generated_new_output_zero, trace.generated_set_calls,
           trace.generated_set_status, trace.generated_set_output_zero,
           trace.generated_set_source_before_status, trace.generated_set_source_before_size,
           trace.generated_set_source_after_status, trace.generated_set_source_after_size,
           observation.new.calls, observation.new.status, observation.new.output_zero,
           observation.new.roots, observation.new.root_publish_attempts,
           observation.shutdown_registered, observation.final_roots,
           observation.final_ordinary_map_handles, observation.final_owned_get_handles,
           observation.final_runtime_map_handles, observation.final_managed_any,
           observation.final_owned_strings, observation.final_any_owner_tokens,
           observation.final_lists);

    if (generated_status != expected_generated_status || !observation_matches ||
        !trace_matches) {
        fprintf(stderr,
                "MapSetDropControl Result L3 invariant failure: observation_status=%" PRId32
                " generated_status=%" PRId32 " expected=%" PRId32 "\n",
                observation_status, generated_status, expected_generated_status);
        return 93;
    }
    return 0;
}
