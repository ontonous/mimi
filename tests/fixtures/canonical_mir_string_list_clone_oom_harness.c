/*
 * Test-only linker oracle for the production canonical MIR String List clone.
 * The generated MIR object is linked with --wrap for clone/drop/abort and the
 * C allocator. The clone wrapper delegates to the real runtime; malloc fails
 * once, on the second element's 32-byte payload, after one element is owned by
 * the partial clone. No runtime or production ABI hook is added.
 */
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

#if UINTPTR_MAX != UINT64_MAX
#error "This native List ABI oracle is pinned to the 64-bit host layout"
#endif

typedef struct {
    uint32_t magic;
    uint32_t pad;
    void *payload;
    int64_t length;
} MimiStringBoxLayout;

_Static_assert(sizeof(MimiStringBoxLayout) == 24, "MimiStr ABI-v3 box layout");

enum {
    LIST_STORAGE_ALLOCATION_BYTES = 8 + 4 * (int)sizeof(void *),
};

#define FIRST_PAYLOAD_ALLOCATION_BYTES (sizeof("first"))
#define FAIL_PAYLOAD_ALLOCATION_BYTES (sizeof("abcdefghijklmnopqrstuvwxyz12345"))

extern void *__real_malloc(size_t size);
extern void __real_free(void *allocation);
extern void *__real_mimi_mir_list_clone_string(const void *source);
extern void __real_mimi_mir_list_drop_string(void *list);
extern void __real_mimi_runtime_abort(const char *message) __attribute__((noreturn));

static const void *clone_source;
static int clone_active;
static int clone_calls;
static int clone_returned_null;
static int injected_failures;
static int first_payload_seen;
static int string_box_seen;
static int list_storage_seen;
static int allocation_ledger_overflow;
static int allocation_ledger_double_free;
static int source_drop_calls;
static size_t successful_allocations_before_failure;
static void *clone_allocations[64];
static unsigned char clone_allocation_freed[64];
static size_t clone_allocation_count;

static int clone_allocations_settled(void) {
    if (allocation_ledger_overflow || allocation_ledger_double_free) {
        return 0;
    }
    for (size_t index = 0; index < clone_allocation_count; index++) {
        if (!clone_allocation_freed[index]) {
            return 0;
        }
    }
    return 1;
}

static void raw_stderr(const char *bytes, size_t length) {
    (void)write(STDERR_FILENO, bytes, length);
}

void *__wrap_malloc(size_t size) {
    if (!clone_active) {
        return __real_malloc(size);
    }

    if (size == FIRST_PAYLOAD_ALLOCATION_BYTES) {
        first_payload_seen = 1;
    }
    /* The MimiList header is also 24 bytes; require the payload to have been
     * allocated first so this specifically witnesses alloc_mimi_str's box. */
    if (size == sizeof(MimiStringBoxLayout) && first_payload_seen) {
        string_box_seen = 1;
    }
    if (size == LIST_STORAGE_ALLOCATION_BYTES) {
        list_storage_seen = 1;
    }
    if (size == FAIL_PAYLOAD_ALLOCATION_BYTES) {
        injected_failures++;
        successful_allocations_before_failure = clone_allocation_count;
        return NULL;
    }

    void *allocation = __real_malloc(size);
    if (allocation != NULL) {
        if (clone_allocation_count == sizeof(clone_allocations) / sizeof(clone_allocations[0])) {
            allocation_ledger_overflow = 1;
        } else {
            clone_allocations[clone_allocation_count] = allocation;
            clone_allocation_freed[clone_allocation_count] = 0;
            clone_allocation_count++;
        }
    }
    return allocation;
}

void __wrap_free(void *allocation) {
    if (clone_active && allocation != NULL) {
        for (size_t index = 0; index < clone_allocation_count; index++) {
            if (clone_allocations[index] == allocation) {
                if (clone_allocation_freed[index]) {
                    allocation_ledger_double_free = 1;
                } else {
                    clone_allocation_freed[index] = 1;
                }
                break;
            }
        }
    }
    __real_free(allocation);
}

void *__wrap_mimi_mir_list_clone_string(const void *source) {
    clone_calls++;
    clone_source = source;
    memset(clone_allocation_freed, 0, sizeof(clone_allocation_freed));
    clone_allocation_count = 0;
    allocation_ledger_overflow = 0;
    allocation_ledger_double_free = 0;
    successful_allocations_before_failure = 0;
    clone_active = 1;
    void *cloned = __real_mimi_mir_list_clone_string(source);
    clone_returned_null = cloned == NULL;
    clone_active = 0;
    return cloned;
}

void __wrap_mimi_mir_list_drop_string(void *list) {
    if (!clone_active && clone_source != NULL && list == clone_source) {
        source_drop_calls++;
    }
    __real_mimi_mir_list_drop_string(list);
}

__attribute__((noreturn)) void __wrap_mimi_runtime_abort(const char *message) {
    const int verified = clone_calls == 1 && clone_returned_null &&
        injected_failures == 1 && first_payload_seen && string_box_seen &&
        list_storage_seen && successful_allocations_before_failure >= 3 &&
        clone_allocations_settled() && source_drop_calls == 1 &&
        message != NULL && strstr(message, "canonical MIR List clone failed") != NULL;
    if (!verified) {
        static const char failure[] =
            "string-list-clone-oom-native injected-or-cleanup-check-failed\n";
        raw_stderr(failure, sizeof(failure) - 1);
        _exit(97);
    }

    static const char success[] =
        "string-list-clone-oom-native injected=1 rollback=clean "
        "source-drop=1 trap=E0800\n";
    raw_stderr(success, sizeof(success) - 1);
    __real_mimi_runtime_abort(message);
}
