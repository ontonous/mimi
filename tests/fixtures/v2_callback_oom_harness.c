/* Strict native oracle for the real generated AnyPayload V2 callbacks.
 * Only malloc/free references in this object and the generated object are
 * wrapped. The C library and the test driver retain their real allocators. */
#include <assert.h>
#include <limits.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef NDEBUG
#error "The V2 callback memory oracle requires assertions"
#endif

extern int32_t mimi_v2_test_clone(void *destination, const void *source);
extern void mimi_v2_test_drop(void *payload);
extern void *__real_malloc(size_t size);
extern void __real_free(void *allocation);

static size_t allocation_calls;
static size_t fail_at = SIZE_MAX;
static size_t injected_failures;
static ptrdiff_t live_allocations;

void *__wrap_malloc(size_t size) {
    allocation_calls++;
    if (allocation_calls == fail_at) {
        injected_failures++;
        return NULL;
    }
    void *allocation = __real_malloc(size);
    if (allocation != NULL) {
        live_allocations++;
    }
    return allocation;
}

void __wrap_free(void *allocation) {
    if (allocation != NULL) {
        live_allocations--;
    }
    __real_free(allocation);
}

_Noreturn void mimi_runtime_abort(const char *message) {
    fprintf(stderr, "V2 clone callback unexpectedly aborted: %s\n",
            message != NULL ? message : "(null)");
    abort();
}

typedef struct {
    const unsigned char *data;
    int64_t length;
} String;

typedef struct {
    String first;
    String second;
} Pair;

typedef struct {
    uint8_t active;
    String payload;
} Option;

typedef struct {
    uint8_t active;
    String ok;
    int64_t error;
} Result;

typedef union {
    unsigned char bytes[64];
    String string;
    Pair pair;
    Option option;
    Result result;
} Storage;

_Static_assert(sizeof(void *) == 8, "native oracle targets the 64-bit host ABI");
_Static_assert(sizeof(String) == 16, "StringBox ABI");
_Static_assert(sizeof(Pair) == 32, "two-field Record ABI");
_Static_assert(sizeof(Option) == 24 && offsetof(Option, payload) == 8, "Option ABI");
_Static_assert(sizeof(Result) == 32 && offsetof(Result, error) == 24, "Result ABI");

static int mode_is(const char *mode, const char *expected) {
    return strcmp(mode, expected) == 0;
}

static void assert_string(String value, String borrowed) {
    assert(value.length == borrowed.length);
    assert(value.data != NULL && value.data != borrowed.data);
    assert(memcmp(value.data, borrowed.data, (size_t)value.length) == 0);
    assert(value.data[value.length] == 0);
}

static void assert_value(const char *mode, const Storage *value, String text,
                         const String *borrowed_header) {
    if (mode_is(mode, "string-first")) {
        assert_string(value->string, text);
    } else if (mode_is(mode, "string-null-empty")) {
        assert(value->string.data == NULL && value->string.length == 0);
    } else if (mode_is(mode, "record-second")) {
        assert_string(value->pair.first, text);
        assert_string(value->pair.second, text);
        assert(value->pair.first.data != value->pair.second.data);
    } else if (mode_is(mode, "option-some")) {
        assert(value->option.active == 1);
        assert_string(value->option.payload, text);
    } else if (mode_is(mode, "option-none-poison")) {
        assert(value->option.active == 0);
        assert(value->option.payload.data == NULL && value->option.payload.length == 0);
    } else if (mode_is(mode, "result-ok-poison")) {
        assert(value->result.active == 1 && value->result.error == 0);
        assert_string(value->result.ok, text);
    } else {
        assert(value->result.active == 0);
        assert(value->result.ok.data == NULL && value->result.ok.length == 0);
        if (mode_is(mode, "result-err-empty")) {
            assert(value->result.error == 0);
        } else {
            const String *header = (const String *)(intptr_t)value->result.error;
            assert(header != NULL && header != borrowed_header);
            assert_string(*header, text);
        }
    }
}

int main(int argc, char **argv) {
    assert(argc == 2);
    const char *mode = argv[1];
    const String text = {(const unsigned char *)"payload", 7};
    const String poison = {(const unsigned char *)(uintptr_t)1, -1};
    const String error_header = text;
    Storage source;
    Storage destination;
    memset(&source, 0, sizeof(source));
    memset(&destination, 0xa5, sizeof(destination));

    if (mode_is(mode, "record-second")) {
        source.pair.first = text;
        source.pair.second = text;
    } else if (mode_is(mode, "option-some") || mode_is(mode, "option-none-poison")) {
        source.option.active = (uint8_t)mode_is(mode, "option-some");
        source.option.payload = source.option.active ? text : poison;
    } else if (mode_is(mode, "result-ok-poison") || mode_is(mode, "result-err-poison") ||
               mode_is(mode, "result-err-header") || mode_is(mode, "result-err-empty")) {
        source.result.active = (uint8_t)mode_is(mode, "result-ok-poison");
        source.result.ok = source.result.active ? text : poison;
        source.result.error = source.result.active ? 1 :
            (mode_is(mode, "result-err-empty") ? 0 : (int64_t)(intptr_t)&error_header);
    } else {
        source.string = text;
        if (mode_is(mode, "string-null-empty")) {
            source.string.data = NULL;
            source.string.length = 0;
        } else if (mode_is(mode, "string-negative")) {
            source.string.length = -1;
        } else if (mode_is(mode, "string-max-length")) {
            source.string.length = INT64_MAX;
        } else if (mode_is(mode, "string-near-max-length")) {
            source.string.length = INT64_MAX - 7;
        } else {
            assert(mode_is(mode, "string-first"));
        }
    }
    unsigned char original_source[sizeof(source)];
    memcpy(original_source, &source, sizeof(source));

    const int expected_failure = mode_is(mode, "string-first") ||
        mode_is(mode, "string-negative") || mode_is(mode, "string-max-length") ||
        mode_is(mode, "string-near-max-length") || mode_is(mode, "record-second") ||
        mode_is(mode, "result-err-header");
    const int retry = mode_is(mode, "string-first") || mode_is(mode, "record-second") ||
        mode_is(mode, "result-err-header");
    size_t expected_calls = 0;
    if (mode_is(mode, "string-first") || mode_is(mode, "option-some") ||
        mode_is(mode, "result-ok-poison")) {
        expected_calls = 1;
    } else if (mode_is(mode, "record-second") || mode_is(mode, "result-err-poison") ||
               mode_is(mode, "result-err-header")) {
        expected_calls = 2;
    }
    if (expected_failure) {
        fail_at = mode_is(mode, "record-second") || mode_is(mode, "result-err-header") ? 2 : 1;
    }

    const int32_t status = mimi_v2_test_clone(&destination, &source);
    assert((status != 0) == expected_failure);
    assert(allocation_calls == expected_calls);
    assert(injected_failures == (size_t)retry);
    assert(memcmp(original_source, &source, sizeof(source)) == 0);
    if (expected_failure) {
        for (size_t index = 0; index < sizeof(destination); index++) {
            assert(destination.bytes[index] == 0xa5);
        }
    } else {
        assert_value(mode, &destination, text, &error_header);
        mimi_v2_test_drop(&destination);
    }
    assert(live_allocations == 0);

    if (retry) {
        allocation_calls = 0;
        fail_at = SIZE_MAX;
        assert(mimi_v2_test_clone(&destination, &source) == 0);
        assert_value(mode, &destination, text, &error_header);
        mimi_v2_test_drop(&destination);
        assert(allocation_calls == (mode_is(mode, "string-first") ? 1u : 2u));
        assert(live_allocations == 0);
        assert(memcmp(original_source, &source, sizeof(source)) == 0);
    }
    printf("v2-callback-native-ok mode=%s status=%d allocations=%zu live=0\n",
           mode, status, expected_calls);
    return 0;
}
