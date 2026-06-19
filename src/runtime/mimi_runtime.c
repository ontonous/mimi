#include <stdint.h>
#include <stddef.h>
#include "mimi_runtime.h"

/* ====================================================================
 * MIMI_NO_STD (freestanding) support
 *
 * When MIMI_NO_STD is defined, the runtime provides minimal inline
 * implementations of essential libc functions so the resulting binary
 * can run in freestanding environments (no libc, no OS syscalls).
 *
 * Features that unconditionally require OS support (sockets, threads,
 * file I/O, signals) are either stub-returned or omitted.
 * ==================================================================== */

#ifdef MIMI_NO_STD

/* Minimal memory: simple bump allocator over a static 128KB pool */
#define BUMP_POOL_SIZE (128 * 1024)
static char bump_pool[BUMP_POOL_SIZE];
static size_t bump_offset = 0;

void* malloc(size_t size) {
    /* Align to 8 bytes */
    size = (size + 7) & ~7;
    if (bump_offset + size > BUMP_POOL_SIZE) return (void*)0;
    void* ptr = (void*)&bump_pool[bump_offset];
    bump_offset += size;
    return ptr;
}

void free(void* ptr) {
    (void)ptr;
    /* Bump allocator: free is a no-op (memory persists for process lifetime) */
}

void* realloc(void* ptr, size_t new_size) {
    (void)ptr;
    /* Simplified realloc: just allocate new block (old data discarded) */
    return malloc(new_size);
}

void* calloc(size_t count, size_t size) {
    if (size != 0 && count > SIZE_MAX / size) return (void*)0;
    size_t total = count * size;
    void* ptr = malloc(total);
    if (ptr) {
        char* cp = (char*)ptr;
        for (size_t i = 0; i < total; i++) cp[i] = 0;
    }
    return ptr;
}

/* String helpers */
size_t strlen(const char* s) {
    const char* p = s;
    while (*p) p++;
    return (size_t)(p - s);
}

int strcmp(const char* a, const char* b) {
    while (*a && *a == *b) { a++; b++; }
    return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char* a, const char* b, size_t n) {
    for (size_t i = 0; i < n; i++) {
        if (a[i] != b[i]) return (unsigned char)a[i] - (unsigned char)b[i];
        if (!a[i]) break;
    }
    return 0;
}

char* strcpy(char* dst, const char* src) {
    /* SAFETY: Callers must ensure dst is large enough for src (including NUL).
     * All callers in this file allocate via malloc/strdup with correct sizes. */
    char* p = dst;
    while ((*p++ = *src++));
    return dst;
}

char* strncpy(char* dst, const char* src, size_t n) {
    for (size_t i = 0; i < n; i++) {
        dst[i] = src[i];
        if (!src[i]) break;
    }
    return dst;
}

char* strcat(char* dst, const char* src) {
    /* SAFETY: Callers must ensure dst has room for strlen(dst)+strlen(src)+1.
     * All callers in this file allocate via malloc/strdup with correct sizes. */
    char* p = dst + strlen(dst);
    while ((*p++ = *src++));
    return dst;
}

char* strdup(const char* s) {
    size_t len = strlen(s) + 1;
    char* c = (char*)malloc(len);
    if (c) strcpy(c, s);
    return c;
}

char* strstr(const char* haystack, const char* needle) {
    if (!*needle) return (char*)haystack;
    size_t nlen = strlen(needle);
    while (*haystack) {
        if (strncmp(haystack, needle, nlen) == 0) return (char*)haystack;
        haystack++;
    }
    return (char*)0;
}

void* memset(void* ptr, int value, size_t num) {
    unsigned char* p = (unsigned char*)ptr;
    for (size_t i = 0; i < num; i++) p[i] = (unsigned char)value;
    return ptr;
}

void* memcpy(void* dst, const void* src, size_t num) {
    unsigned char* d = (unsigned char*)dst;
    const unsigned char* s = (const unsigned char*)src;
    for (size_t i = 0; i < num; i++) d[i] = s[i];
    return dst;
}

void* memmove(void* dst, const void* src, size_t num) {
    unsigned char* d = (unsigned char*)dst;
    const unsigned char* s = (const unsigned char*)src;
    if (d < s) {
        for (size_t i = 0; i < num; i++) d[i] = s[i];
    } else if (d > s) {
        for (size_t i = num; i > 0; i--) d[i-1] = s[i-1];
    }
    return dst;
}

/* No-op stubs for OS-dependent functions */
void exit(int code) { (void)code; while (1) {} }
int printf(const char* fmt, ...) { (void)fmt; return 0; }
int fprintf(void* stream, const char* fmt, ...) { (void)stream; (void)fmt; return 0; }
int puts(const char* s) { (void)s; return 0; }
int sprintf(char* buf, const char* fmt, ...) { (void)buf; (void)fmt; return 0; }
int snprintf(char* buf, size_t n, const char* fmt, ...) { (void)buf; (void)n; (void)fmt; return 0; }
int fgets(char* buf, int n, void* stream) { (void)buf; (void)n; (void)stream; return 0; }
int fputs(const char* s, void* stream) { (void)s; (void)stream; return 0; }
int fflush(void* stream) { (void)stream; return 0; }

/* Use an opaque type alias for FILE* to avoid stdio.h */
typedef struct { int dummy; } MimiFile;
MimiFile* fopen(const char* path, const char* mode) { (void)path; (void)mode; return (MimiFile*)0; }
int fclose(MimiFile* f) { (void)f; return -1; }
size_t fread(void* buf, size_t sz, size_t count, MimiFile* f) { (void)buf; (void)sz; (void)count; (void)f; return 0; }
size_t fwrite(const void* buf, size_t sz, size_t count, MimiFile* f) { (void)buf; (void)sz; (void)count; (void)f; return 0; }
int access(const char* path, int mode) { (void)path; (void)mode; return -1; }

/* Stubs for networking (return errors) */
int64_t mimi_socket(int64_t d, int64_t t, int64_t p) { (void)d; (void)t; (void)p; return -1; }
int64_t mimi_connect(int64_t fd, const char* h, int64_t p) { (void)fd; (void)h; (void)p; return -1; }
int64_t mimi_bind(int64_t fd, int64_t p) { (void)fd; (void)p; return -1; }
int64_t mimi_listen(int64_t fd, int64_t b) { (void)fd; (void)b; return -1; }
int64_t mimi_accept(int64_t fd) { (void)fd; return -1; }
int64_t mimi_send(int64_t fd, const char* d, int64_t l) { (void)fd; (void)d; (void)l; return -1; }
char* mimi_recv(int64_t fd, int64_t bs, int64_t* ol) { (void)fd; (void)bs; (void)ol; return (char*)0; }
int64_t mimi_close(int64_t fd) { (void)fd; return -1; }
char* mimi_http_get(const char* u) { (void)u; return (char*)0; }
char* mimi_http_post(const char* u, const char* b) { (void)u; (void)b; return (char*)0; }
const char* json_get_string(const char* j, const char* k) { (void)j; (void)k; return (const char*)0; }
int64_t json_get_int(const char* j, const char* k) { (void)j; (void)k; return 0; }
const char* json_get_element(const char* j, int64_t i) { (void)j; (void)i; return (const char*)0; }

/* Stubs for pthread (parasteps won't work in freestanding) */
int pthread_create(void* tid, void* attr, void* (*fn)(void*), void* arg) {
    (void)tid; (void)attr; (void)fn; (void)arg; return -1;
}
int pthread_join(void* tid, void** retval) { (void)tid; (void)retval; return -1; }
int pthread_mutex_lock(void* m) { (void)m; return 0; }
int pthread_mutex_unlock(void* m) { (void)m; return 0; }
int pthread_cond_wait(void* c, void* m) { (void)c; (void)m; return 0; }
int pthread_cond_signal(void* c) { (void)c; return 0; }
int pthread_cond_broadcast(void* c) { (void)c; return 0; }

/* Stubs for time functions */
int64_t mimi_now(void) { return 0; }
int64_t mimi_now_ms(void) { return 0; }
void mimi_sleep(int64_t ms) { (void)ms; }

/* Stubs for environment */
const char* mimi_getenv(const char* name) { (void)name; return (const char*)0; }
int64_t mimi_args_count(void) { return 0; }
const char* mimi_args_get(int64_t i) { (void)i; return (const char*)0; }

/* Thread pool stubs */
void mimi_pool_submit(void* fn, void* arg) { (void)fn; (void)arg; }
void mimi_pool_join_all(void) {}

/* mimi_try_exit without stdio */
void mimi_try_exit(int64_t payload) {
    (void)payload;
    while (1) {}
}

/* MIMI_NO_STD stub: refcounted allocation uses bump allocator (no atomics) */
void* mimi_rc_alloc(int64_t size) {
    size_t total = sizeof(int64_t) + (size_t)size;
    int64_t* hdr = (int64_t*)malloc(total);
    if (!hdr) return (void*)0;
    *hdr = 1;  /* refcount = 1 */
    return (void*)(hdr + 1);
}
void mimi_rc_retain(void* ptr) {
    if (!ptr) return;
    int64_t* hdr = (int64_t*)ptr - 1;
    (*hdr)++;
}
void mimi_rc_release(void* ptr) {
    if (!ptr) return;
    int64_t* hdr = (int64_t*)ptr - 1;
    (*hdr)--;
    if (*hdr <= 0) free(hdr);
}

#else /* !MIMI_NO_STD — normal libc build */

#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <time.h>
#include <pthread.h>
#include <stdatomic.h>

/* Reference-counted heap allocation.
   Layout: [ int64_t refcount | user data ... ]
   Returns pointer to user data (right after the refcount header). */
void* mimi_rc_alloc(int64_t size) {
    size_t total = sizeof(atomic_int_least64_t) + (size_t)size;
    atomic_int_least64_t* hdr = (atomic_int_least64_t*)malloc(total);
    if (!hdr) return (void*)0;
    atomic_init(hdr, 1);
    return (void*)(hdr + 1);
}

void mimi_rc_retain(void* ptr) {
    if (!ptr) return;
    atomic_int_least64_t* hdr = (atomic_int_least64_t*)ptr - 1;
    atomic_fetch_add(hdr, 1);
}

void mimi_rc_release(void* ptr) {
    if (!ptr) return;
    atomic_int_least64_t* hdr = (atomic_int_least64_t*)ptr - 1;
    int64_t prev = atomic_fetch_sub(hdr, 1);
    if (prev <= 1) {
        free(hdr);
    }
}

#define INITIAL_CAPACITY 16
#define LOAD_FACTOR 0.75

typedef enum {
    ENTRY_EMPTY = 0,
    ENTRY_USED = 1,
    ENTRY_DELETED = 2
} EntryState;

typedef struct Entry {
    char* key;
    ValueHandle value;
    EntryState state;
} Entry;

typedef struct Map {
    Entry* entries;
    int64_t capacity;
    int64_t size;
    int64_t deleted_count;
} Map;

static uint64_t fnv_hash(const char* key) {
    uint64_t hash = 14695981039346656037ULL;
    for (const char* p = key; *p; p++) {
        hash ^= (uint64_t)(unsigned char)(*p);
        hash *= 1099511628211ULL;
    }
    return hash;
}

static int map_grow(Map* map) {
    int64_t new_capacity = map->capacity * 2;
    Entry* new_entries = calloc(new_capacity, sizeof(Entry));
    if (!new_entries) return -1;

    for (int64_t i = 0; i < map->capacity; i++) {
        if (map->entries[i].state == ENTRY_USED) {
            int64_t idx = fnv_hash(map->entries[i].key) % new_capacity;
            while (new_entries[idx % new_capacity].state == ENTRY_USED) {
                idx++;
            }
            new_entries[idx % new_capacity] = map->entries[i];
        }
    }

    free(map->entries);
    map->entries = new_entries;
    map->capacity = new_capacity;
    map->deleted_count = 0;
    return 0;
}

MapHandle mimi_map_new(void) {
    Map* map = calloc(1, sizeof(Map));
    if (!map) return 0;
    map->entries = calloc(INITIAL_CAPACITY, sizeof(Entry));
    if (!map->entries) {
        free(map);
        return 0;
    }
    map->capacity = INITIAL_CAPACITY;
    map->size = 0;
    map->deleted_count = 0;
    return (MapHandle)map;
}

void mimi_map_destroy(MapHandle handle) {
    if (!handle) return;
    Map* map = (Map*)handle;
    for (int64_t i = 0; i < map->capacity; i++) {
        if (map->entries[i].state == ENTRY_USED && map->entries[i].key) {
            free(map->entries[i].key);
        }
    }
    free(map->entries);
    free(map);
}

int64_t mimi_map_size(MapHandle handle) {
    if (!handle) return 0;
    Map* map = (Map*)handle;
    return map->size;
}

int mimi_map_has_key(MapHandle handle, const char* key) {
    if (!handle || !key) return 0;
    Map* map = (Map*)handle;
    int64_t idx = fnv_hash(key) % map->capacity;
    int64_t start_idx = idx;

    do {
        if (map->entries[idx].state == ENTRY_EMPTY) return 0;
        if (map->entries[idx].state == ENTRY_USED &&
            strcmp(map->entries[idx].key, key) == 0) {
            return 1;
        }
        idx = (idx + 1) % map->capacity;
    } while (idx != start_idx);

    return 0;
}

ValueHandle mimi_map_get(MapHandle handle, const char* key) {
    if (!handle || !key) return 0;
    Map* map = (Map*)handle;
    int64_t idx = fnv_hash(key) % map->capacity;
    int64_t start_idx = idx;

    do {
        if (map->entries[idx].state == ENTRY_EMPTY) return 0;
        if (map->entries[idx].state == ENTRY_USED &&
            strcmp(map->entries[idx].key, key) == 0) {
            return map->entries[idx].value;
        }
        idx = (idx + 1) % map->capacity;
    } while (idx != start_idx);

    return 0;
}

void mimi_map_set(MapHandle handle, const char* key, ValueHandle value) {
    if (!handle || !key) return;
    Map* map = (Map*)handle;

    if ((map->size + map->deleted_count) >= (int64_t)(map->capacity * LOAD_FACTOR)) {
        map_grow(map);
    }

    int64_t idx = fnv_hash(key) % map->capacity;
    int64_t first_deleted = -1;
    int64_t start_idx = idx;

    do {
        if (map->entries[idx].state == ENTRY_USED &&
            strcmp(map->entries[idx].key, key) == 0) {
            map->entries[idx].value = value;
            return;
        }
        if (map->entries[idx].state == ENTRY_DELETED && first_deleted == -1) {
            first_deleted = idx;
        }
        if (map->entries[idx].state == ENTRY_EMPTY) {
            int64_t target_idx = (first_deleted != -1) ? first_deleted : idx;
            char* key_copy = malloc(strlen(key) + 1);
            if (!key_copy) return;
            strcpy(key_copy, key);
            map->entries[target_idx].key = key_copy;
            map->entries[target_idx].value = value;
            map->entries[target_idx].state = ENTRY_USED;
            map->size++;
            return;
        }
        idx = (idx + 1) % map->capacity;
    } while (idx != start_idx);

    if (first_deleted != -1) {
        char* key_copy = malloc(strlen(key) + 1);
        if (!key_copy) return;
        strcpy(key_copy, key);
        map->entries[first_deleted].key = key_copy;
        map->entries[first_deleted].value = value;
        map->entries[first_deleted].state = ENTRY_USED;
        map->size++;
    }
}

int mimi_map_remove(MapHandle handle, const char* key) {
    if (!handle || !key) return 0;
    Map* map = (Map*)handle;
    int64_t idx = fnv_hash(key) % map->capacity;
    int64_t start_idx = idx;

    do {
        if (map->entries[idx].state == ENTRY_EMPTY) return 0;
        if (map->entries[idx].state == ENTRY_USED &&
            strcmp(map->entries[idx].key, key) == 0) {
            free(map->entries[idx].key);
            map->entries[idx].key = NULL;
            map->entries[idx].value = 0;
            map->entries[idx].state = ENTRY_DELETED;
            map->size--;
            map->deleted_count++;
            return 1;
        }
        idx = (idx + 1) % map->capacity;
    } while (idx != start_idx);

    return 0;
}

MapHandle mimi_map_from_list(ValueHandle* keys, ValueHandle* values, int64_t n) {
    MapHandle handle = mimi_map_new();
    if (!handle || !keys || !values) return handle;

    for (int64_t i = 0; i < n; i++) {
        ValueHandle key_handle = keys[i];
        ValueHandle val_handle = values[i];
        const char* key_str = (const char*)key_handle;
        if (key_str) {
            mimi_map_set(handle, key_str, val_handle);
        }
    }
    return handle;
}

const char* mimi_value_type_name(ValueHandle handle) {
    (void)handle;
    return "unknown";
}

/* ========== String functions ========== */

MimiList* mimi_str_split(const char* s, const char* delim) {
    MimiList* result = (MimiList*)calloc(1, sizeof(MimiList));
    if (!result || !s || !delim) return result;

    /* Count parts first */
    int64_t count = 0;
    size_t delim_len = strlen(delim);
    if (delim_len == 0) {
        /* Empty delimiter: split into individual characters */
        for (const char* p = s; *p; p++) count++;
        if (count == 0) count = 1;
    } else {
        const char* p = s;
        count = 1;
        while ((p = strstr(p, delim)) != NULL) {
            count++;
            p += delim_len;
        }
    }

    result->data = (const char**)calloc(count, sizeof(const char*));
    if (!result->data) { free(result); return NULL; }
    result->len = count;

    if (delim_len == 0) {
        /* Empty delimiter: each character is a part */
        int64_t i = 0;
        for (const char* p = s; *p; p++) {
            char* part = (char*)malloc(2);
            part[0] = *p;
            part[1] = '\0';
            result->data[i++] = part;
        }
    } else {
        int64_t i = 0;
        const char* start = s;
        const char* found;
        while ((found = strstr(start, delim)) != NULL) {
            size_t part_len = found - start;
            char* part = (char*)malloc(part_len + 1);
            memcpy(part, start, part_len);
            part[part_len] = '\0';
            result->data[i++] = part;
            start = found + delim_len;
        }
        /* Last part (or the whole string if no delimiter found) */
        result->data[i] = strdup(start);
    }

    return result;
}

const char* mimi_str_join(const MimiList* list, const char* sep) {
    if (!list || !list->data || list->len == 0) return strdup("");
    if (!sep) sep = "";

    /* Calculate total length */
    size_t total = 0;
    size_t sep_len = strlen(sep);
    for (int64_t i = 0; i < list->len; i++) {
        total += strlen(list->data[i] ? list->data[i] : "");
        if (i < list->len - 1) total += sep_len;
    }

    char* result = (char*)malloc(total + 1);
    if (!result) return strdup("");

    char* p = result;
    for (int64_t i = 0; i < list->len; i++) {
        const char* s = list->data[i] ? list->data[i] : "";
        size_t len = strlen(s);
        memcpy(p, s, len);
        p += len;
        if (i < list->len - 1) {
            memcpy(p, sep, sep_len);
            p += sep_len;
        }
    }
    *p = '\0';
    return result;
}

void mimi_try_exit(int64_t payload) {
    /* Print the error payload as a numeric value.
     * We do NOT dereference payload as a pointer because:
     * 1. It may not be a valid pointer (could be an integer error code)
     * 2. Even if it looks like a pointer, dereferencing unmapped memory causes segfault
     * 3. The printable-ASCII heuristic can be tricked into leaking arbitrary memory
     * The caller should use fprintf(stderr, ...) directly if a string message is needed. */
    fprintf(stderr, "Error: Result::Err(%ld)\n", (long)payload);
    exit(1);
}

/* ========== Capability runtime ========== */

#define MAX_CAPS 256
typedef struct {
    int64_t id;
    char name[64];
    int consumed;
} CapEntry;

static CapEntry cap_table[MAX_CAPS];
static int64_t cap_next_id = 1;
static int cap_count = 0;
static pthread_mutex_t cap_mutex = PTHREAD_MUTEX_INITIALIZER;

int64_t mimi_cap_register(const char* name) {
    pthread_mutex_lock(&cap_mutex);
    if (cap_count >= MAX_CAPS) { pthread_mutex_unlock(&cap_mutex); return -1; }
    int64_t id = cap_next_id++;
    cap_table[cap_count].id = id;
    cap_table[cap_count].consumed = 0;
    if (name) {
        strncpy(cap_table[cap_count].name, name, 63);
        cap_table[cap_count].name[63] = '\0';
    } else {
        cap_table[cap_count].name[0] = '\0';
    }
    cap_count++;
    pthread_mutex_unlock(&cap_mutex);
    return id;
}

int mimi_cap_check(int64_t cap, const char* name) {
    pthread_mutex_lock(&cap_mutex);
    for (int i = 0; i < cap_count; i++) {
        if (cap_table[i].id == cap && !cap_table[i].consumed) {
            if (!name || !name[0]) { pthread_mutex_unlock(&cap_mutex); return 1; }
            if (strcmp(cap_table[i].name, name) == 0) { pthread_mutex_unlock(&cap_mutex); return 1; }
            pthread_mutex_unlock(&cap_mutex);
            return 0;
        }
    }
    pthread_mutex_unlock(&cap_mutex);
    return 0;
}

int mimi_cap_consume(int64_t cap, const char* name) {
    pthread_mutex_lock(&cap_mutex);
    for (int i = 0; i < cap_count; i++) {
        if (cap_table[i].id == cap && !cap_table[i].consumed) {
            if (!name || !name[0] || strcmp(cap_table[i].name, name) == 0) {
                cap_table[i].consumed = 1;
                pthread_mutex_unlock(&cap_mutex);
                return 1;
            }
            pthread_mutex_unlock(&cap_mutex);
            return 0;
        }
    }
    pthread_mutex_unlock(&cap_mutex);
    return 0;
}

const char* mimi_str_replace(const char* s, const char* from, const char* to) {
    if (!s) return strdup("");
    if (!from || from[0] == '\0') return strdup(s);
    if (!to) to = "";

    /* Count occurrences */
    size_t from_len = strlen(from);
    size_t to_len = strlen(to);
    int64_t count = 0;
    const char* p = s;
    while ((p = strstr(p, from)) != NULL) {
        count++;
        p += from_len;
    }

    if (count == 0) return strdup(s);

    /* Calculate result length using signed arithmetic to avoid unsigned wraparound
     * when to_len < from_len (shortening replacement). The result is always >= 0
     * because count only reflects actual occurrences found in s. */
    size_t s_len = strlen(s);
    int64_t delta = (int64_t)to_len - (int64_t)from_len;
    size_t result_len = s_len + (size_t)(count * delta);
    char* result = (char*)malloc(result_len + 1);
    if (!result) return strdup(s);

    /* Build result */
    char* out = result;
    const char* scan = s;
    const char* found;
    while ((found = strstr(scan, from)) != NULL) {
        size_t prefix_len = found - scan;
        memcpy(out, scan, prefix_len);
        out += prefix_len;
        memcpy(out, to, to_len);
        out += to_len;
        scan = found + from_len;
    }
    /* Copy remainder */
    strcpy(out, scan);
    return result;
}

/* ========== Thread pool for parasteps ========== */

#define POOL_MAX_THREADS 64
#define POOL_MAX_TASKS 1024

typedef struct {
    void* (*func)(void*);
    void* arg;
} PoolTask;

static pthread_mutex_t pool_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t pool_cond = PTHREAD_COND_INITIALIZER;
static PoolTask pool_tasks[POOL_MAX_TASKS];
static int pool_task_count = 0;
static size_t pool_task_head = 0;
static size_t pool_task_tail = 0;
static int pool_shutdown = 0;
static int pool_active_threads = 0;
static int pool_threads_initialized = 0;
static pthread_t pool_threads[POOL_MAX_THREADS];
static int pool_thread_count = 0;

static void* pool_worker(void* arg) {
    (void)arg;
    while (1) {
        PoolTask task;
        int got_task = 0;

        pthread_mutex_lock(&pool_mutex);
        while (pool_task_head == pool_task_tail && !pool_shutdown) {
            pthread_cond_wait(&pool_cond, &pool_mutex);
        }
        if (pool_shutdown && pool_task_head == pool_task_tail) {
            pthread_mutex_unlock(&pool_mutex);
            break;
        }
        task = pool_tasks[pool_task_head % POOL_MAX_TASKS];
        pool_task_head++;
        pool_active_threads++;
        pthread_mutex_unlock(&pool_mutex);

        task.func(task.arg);

        pthread_mutex_lock(&pool_mutex);
        pool_active_threads--;
        if (pool_task_head == pool_task_tail && pool_active_threads == 0) {
            pthread_cond_broadcast(&pool_cond);
        }
        pthread_mutex_unlock(&pool_mutex);
    }
    return NULL;
}

static void pool_ensure_init(void) {
    pthread_mutex_lock(&pool_mutex);
    if (pool_threads_initialized) {
        pthread_mutex_unlock(&pool_mutex);
        return;
    }
    int ncpu = sysconf(_SC_NPROCESSORS_ONLN);
    if (ncpu < 1) ncpu = 4;
    if (ncpu > POOL_MAX_THREADS) ncpu = POOL_MAX_THREADS;
    pool_thread_count = ncpu;
    for (int i = 0; i < ncpu; i++) {
        pthread_create(&pool_threads[i], NULL, pool_worker, NULL);
    }
    pool_threads_initialized = 1;
    pthread_mutex_unlock(&pool_mutex);
}

void mimi_pool_submit(void* fn_ptr, void* arg) {
    if (!fn_ptr) return;
    pool_ensure_init();
    void* (*func)(void*) = (void* (*)(void*))fn_ptr;
    pthread_mutex_lock(&pool_mutex);
    while (pool_task_tail - pool_task_head >= POOL_MAX_TASKS) {
        pthread_cond_wait(&pool_cond, &pool_mutex);
    }
    pool_tasks[pool_task_tail % POOL_MAX_TASKS].func = func;
    pool_tasks[pool_task_tail % POOL_MAX_TASKS].arg = arg;
    pool_task_tail++;
    pthread_cond_signal(&pool_cond);
    pthread_mutex_unlock(&pool_mutex);
}

void mimi_pool_join_all(void) {
    if (!pool_threads_initialized) return;
    pthread_mutex_lock(&pool_mutex);
    while (pool_task_head < pool_task_tail || pool_active_threads > 0) {
        pthread_cond_wait(&pool_cond, &pool_mutex);
    }
    pthread_mutex_unlock(&pool_mutex);
}

/* ========== Time functions ========== */

int64_t mimi_now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec;
}

int64_t mimi_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + (int64_t)ts.tv_nsec / 1000000;
}

void mimi_sleep(int64_t ms) {
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (ms % 1000) * 1000000;
    nanosleep(&ts, NULL);
}

/* ========== Environment/CLI functions ========== */

static int stored_argc = 0;
static char** stored_argv = NULL;

void mimi_args_init(int argc, char** argv) {
    stored_argc = argc;
    stored_argv = argv;
}

const char* mimi_getenv(const char* name) {
    if (!name) return NULL;
    return getenv(name);
}

int64_t mimi_args_count(void) {
    // Return argc - 1 (skip program name)
    if (stored_argc <= 1) return 0;
    return (int64_t)(stored_argc - 1);
}

const char* mimi_args_get(int64_t i) {
    if (!stored_argv || i < 0 || i >= stored_argc - 1) return NULL;
    return stored_argv[(int)i + 1]; // +1 to skip program name
}

/* ========== JSON stubs (actual implementation in Rust runtime) ========== */

const char* mimi_to_json(void* value_ptr) {
    (void)value_ptr;
    /* Stub: returns empty JSON object */
    char* result = strdup("{}");
    return result;
}

/* ========== JSON parser (recursive descent, no external dependencies) ========== */

/* Parser state */
typedef struct {
    const char* p;
    const char* start;
    const char* err_pos;
} JsonParser;

static void json_skip_ws(JsonParser* jp) {
    while (*jp->p == ' ' || *jp->p == '\t' || *jp->p == '\n' || *jp->p == '\r') jp->p++;
}

static int json_parse_value(JsonParser* jp, char** out, size_t* out_len);

static int json_parse_string(JsonParser* jp, char** out, size_t* out_len) {
    json_skip_ws(jp);
    if (*jp->p != '"') { jp->err_pos = jp->p; return 0; }
    jp->p++; /* skip opening quote */
    const char* start = jp->p;
    size_t len = 0;
    int esc = 0;
    while (*jp->p) {
        if (esc) { esc = 0; len++; jp->p++; continue; }
        if (*jp->p == '\\') { esc = 1; len++; jp->p++; continue; }
        if (*jp->p == '"') { jp->p++; break; }
        len++; jp->p++;
    }
    if (*(jp->p - 1) != '"') { jp->err_pos = start; return 0; }
    if (out) {
        *out = (char*)malloc(len + 1);
        if (*out) {
            memcpy(*out, start, len);
            (*out)[len] = '\0';
            /* Unescape JSON escapes */
            char* w = *out;
            for (char* r = *out; *r; r++) {
                if (*r == '\\') {
                    r++;
                    switch (*r) {
                        case '"': *w++ = '"'; break;
                        case '\\': *w++ = '\\'; break;
                        case '/': *w++ = '/'; break;
                        case 'b': *w++ = '\b'; break;
                        case 'f': *w++ = '\f'; break;
                        case 'n': *w++ = '\n'; break;
                        case 'r': *w++ = '\r'; break;
                        case 't': *w++ = '\t'; break;
                        case 'u': {
                            r++;
                            uint32_t cp = 0;
                            for (int i = 0; i < 4; i++) {
                                char c = *r;
                                if (c >= '0' && c <= '9') cp = (cp << 4) | (unsigned)(c - '0');
                                else if (c >= 'a' && c <= 'f') cp = (cp << 4) | (unsigned)(c - 'a' + 10);
                                else if (c >= 'A' && c <= 'F') cp = (cp << 4) | (unsigned)(c - 'A' + 10);
                                else { r--; break; }
                                r++;
                            }
                            if (cp <= 0x7F) {
                                *w++ = (char)cp;
                            } else if (cp <= 0x7FF) {
                                *w++ = (char)(0xC0 | (cp >> 6));
                                *w++ = (char)(0x80 | (cp & 0x3F));
                            } else if (cp <= 0xFFFF) {
                                *w++ = (char)(0xE0 | (cp >> 12));
                                *w++ = (char)(0x80 | ((cp >> 6) & 0x3F));
                                *w++ = (char)(0x80 | (cp & 0x3F));
                            } else if (cp <= 0x10FFFF) {
                                *w++ = (char)(0xF0 | (cp >> 18));
                                *w++ = (char)(0x80 | ((cp >> 12) & 0x3F));
                                *w++ = (char)(0x80 | ((cp >> 6) & 0x3F));
                                *w++ = (char)(0x80 | (cp & 0x3F));
                            } else {
                                *w++ = '?';
                            }
                            break;
                        }
                        default: *w++ = *r; break;
                    }
                } else {
                    *w++ = *r;
                }
            }
            *w = '\0';
        }
    }
    if (out_len) *out_len = len;
    return 1;
}

static int json_parse_number(JsonParser* jp, int64_t* out_int, double* out_float, int* is_float) {
    json_skip_ws(jp);
    const char* start = jp->p;
    if (*jp->p == '-') jp->p++;
    if (!*jp->p) { jp->err_pos = jp->p; return 0; }
    int has_dot = 0;
    while (*jp->p >= '0' && *jp->p <= '9') jp->p++;
    if (*jp->p == '.') { has_dot = 1; jp->p++; while (*jp->p >= '0' && *jp->p <= '9') jp->p++; }
    if (*jp->p == 'e' || *jp->p == 'E') { has_dot = 1; jp->p++; if (*jp->p == '+' || *jp->p == '-') jp->p++; while (*jp->p >= '0' && *jp->p <= '9') jp->p++; }
    if (jp->p == start || (!has_dot && jp->p == start + 1 && *start == '-')) { jp->err_pos = start; return 0; }
    size_t len = (size_t)(jp->p - start);
    char buf[64];
    if (len >= sizeof(buf)) return 0;
    memcpy(buf, start, len);
    buf[len] = '\0';
    if (has_dot) {
        *is_float = 1;
        *out_float = strtod(buf, NULL);
    } else {
        *is_float = 0;
        *out_int = strtoll(buf, NULL, 10);
    }
    return 1;
}

static int json_parse_value(JsonParser* jp, char** out, size_t* out_len) {
    json_skip_ws(jp);
    if (!*jp->p) return 0;
    if (*jp->p == '"') {
        return json_parse_string(jp, out, out_len);
    } else if (*jp->p == '{') {
        /* Object: record as raw JSON substring */
        const char* start = jp->p;
        int depth = 0;
        while (*jp->p) {
            if (*jp->p == '{') depth++;
            if (*jp->p == '}') { depth--; if (depth == 0) { jp->p++; break; } }
            jp->p++;
        }
        if (depth != 0) { jp->err_pos = start; return 0; }
        if (out) { *out = strndup(start, (size_t)(jp->p - start)); }
        if (out_len) *out_len = (size_t)(jp->p - start);
        return 1;
    } else if (*jp->p == '[') {
        /* Array: record as raw JSON substring */
        const char* start = jp->p;
        int depth = 0;
        while (*jp->p) {
            if (*jp->p == '[') depth++;
            if (*jp->p == ']') { depth--; if (depth == 0) { jp->p++; break; } }
            jp->p++;
        }
        if (depth != 0) { jp->err_pos = start; return 0; }
        if (out) { *out = strndup(start, (size_t)(jp->p - start)); }
        if (out_len) *out_len = (size_t)(jp->p - start);
        return 1;
    } else if (*jp->p == 't' && strncmp(jp->p, "true", 4) == 0) {
        jp->p += 4;
        if (out) { *out = strdup("true"); }
        if (out_len) *out_len = 4;
        return 1;
    } else if (*jp->p == 'f' && strncmp(jp->p, "false", 5) == 0) {
        jp->p += 5;
        if (out) { *out = strdup("false"); }
        if (out_len) *out_len = 5;
        return 1;
    } else if (*jp->p == 'n' && strncmp(jp->p, "null", 4) == 0) {
        jp->p += 4;
        if (out) { *out = strdup("null"); }
        if (out_len) *out_len = 4;
        return 1;
    } else if (*jp->p == '-' || (*jp->p >= '0' && *jp->p <= '9')) {
        int64_t iv; double fv; int is_float;
        if (!json_parse_number(jp, &iv, &fv, &is_float)) return 0;
        if (out) {
            if (is_float) {
                char buf[32];
                int n = snprintf(buf, sizeof(buf), "%f", fv);
                /* Trim trailing zeros */
                while (n > 1 && buf[n-1] == '0') n--;
                if (buf[n-1] == '.') n--;
                buf[n] = '\0';
                *out = strdup(buf);
            } else {
                char buf[32];
                snprintf(buf, sizeof(buf), "%lld", (long long)iv);
                *out = strdup(buf);
            }
        }
        if (out_len) *out_len = strlen(*out ? *out : "");
        return 1;
    }
    jp->err_pos = jp->p;
    return 0;
}

/* mimi_from_json: validate JSON and return validated string, or NULL on error.
 * Codegen wraps the result as a Mimi string. */
void* mimi_from_json(const char* json_str) {
    if (!json_str) return NULL;
    JsonParser jp;
    jp.p = json_str;
    jp.start = json_str;
    jp.err_pos = NULL;
    char* result = NULL;
    size_t result_len = 0;
    if (!json_parse_value(&jp, &result, &result_len)) return NULL;
    /* Make sure there's no trailing garbage */
    json_skip_ws(&jp);
    if (*jp.p) { free(result); return NULL; }
    return result;
}

/* mimi_is_valid_json: returns 1 if json_str is valid JSON, 0 otherwise.
 * Avoids allocation — purely checks syntax. */
int64_t mimi_is_valid_json(const char* json_str) {
    if (!json_str) return 0;
    JsonParser jp;
    jp.p = json_str;
    jp.start = json_str;
    jp.err_pos = NULL;
    char* result = NULL;
    size_t result_len = 0;
    if (!json_parse_value(&jp, &result, &result_len)) return 0;
    json_skip_ws(&jp);
    int valid = (*jp.p == 0) ? 1 : 0;
    if (result) free(result);
    return valid;
}

/* json_get_string: extract a string field from a JSON object.
 * Returns heap-allocated string or NULL if not found/error.
 * json_str is the raw JSON text (object or value). */
const char* json_get_string(const char* json_str, const char* key) {
    if (!json_str || !key) return NULL;
    JsonParser jp;
    jp.p = json_str;
    jp.start = json_str;
    jp.err_pos = NULL;
    json_skip_ws(&jp);
    if (*jp.p != '{') return NULL;
    jp.p++;
    while (*jp.p && *jp.p != '}') {
        json_skip_ws(&jp);
        char* k = NULL;
        if (!json_parse_string(&jp, &k, NULL)) return NULL;
        json_skip_ws(&jp);
        if (*jp.p != ':') { free(k); return NULL; }
        jp.p++;
        if (strcmp(k, key) == 0) {
            free(k);
            char* val = NULL;
            if (!json_parse_value(&jp, &val, NULL)) return NULL;
            return val;
        }
        free(k);
        if (!json_parse_value(&jp, NULL, NULL)) return NULL;
        json_skip_ws(&jp);
        if (*jp.p == ',') jp.p++;
    }
    return NULL;
}

/* json_get_int: extract an integer field from a JSON object.
 * Returns the value, or 0 if not found/error. */
int64_t json_get_int(const char* json_str, const char* key) {
    char* val = (char*)json_get_string(json_str, key);
    if (!val) return 0;
    char* end = NULL;
    int64_t result = strtoll(val, &end, 10);
    int ok = (end && *end == '\0');
    free(val);
    return ok ? result : 0;
}

/* json_get_element: extract an element from a JSON array by index.
 * Returns heap-allocated JSON substring or NULL. */
const char* json_get_element(const char* json_str, int64_t index) {
    if (!json_str) return NULL;
    JsonParser jp;
    jp.p = json_str;
    jp.start = json_str;
    jp.err_pos = NULL;
    json_skip_ws(&jp);
    if (*jp.p != '[') return NULL;
    jp.p++;
    int64_t i = 0;
    while (*jp.p && *jp.p != ']') {
        if (i == index) {
            char* val = NULL;
            if (!json_parse_value(&jp, &val, NULL)) return NULL;
            return val;
        }
        if (!json_parse_value(&jp, NULL, NULL)) return NULL;
        json_skip_ws(&jp);
        if (*jp.p == ',') jp.p++;
        i++;
    }
    return NULL;
}

/* ========== Network / Socket functions (POSIX only) ========== */

#ifndef MIMI_NO_STD

#include <sys/socket.h>
#include <netinet/in.h>
#include <netdb.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <fcntl.h>

int64_t mimi_socket(int64_t domain, int64_t type, int64_t protocol) {
    int fd = socket((int)domain, (int)type, (int)protocol);
    return (int64_t)fd;
}

int64_t mimi_connect(int64_t fd, const char* host, int64_t port) {
    if (!host || fd < 0) return -1;
    struct addrinfo hints, *res;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    char port_str[16];
    snprintf(port_str, sizeof(port_str), "%ld", (long)port);
    int err = getaddrinfo(host, port_str, &hints, &res);
    if (err != 0 || !res) return -1;
    int ret = connect((int)fd, res->ai_addr, res->ai_addrlen);
    freeaddrinfo(res);
    return (int64_t)ret;
}

int64_t mimi_bind(int64_t fd, int64_t port) {
    if (fd < 0) return -1;
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);
    addr.sin_addr.s_addr = INADDR_ANY;
    int ret = bind((int)fd, (struct sockaddr*)&addr, sizeof(addr));
    return (int64_t)ret;
}

int64_t mimi_listen(int64_t fd, int64_t backlog) {
    if (fd < 0) return -1;
    int ret = listen((int)fd, (int)backlog);
    return (int64_t)ret;
}

int64_t mimi_accept(int64_t fd) {
    if (fd < 0) return -1;
    struct sockaddr_in client_addr;
    socklen_t addr_len = sizeof(client_addr);
    int client_fd = accept((int)fd, (struct sockaddr*)&client_addr, &addr_len);
    return (int64_t)client_fd;
}

int64_t mimi_send(int64_t fd, const char* data, int64_t len) {
    if (fd < 0 || !data) return -1;
    ssize_t sent = send((int)fd, data, (size_t)len, 0);
    return (int64_t)sent;
}

char* mimi_recv(int64_t fd, int64_t buf_size, int64_t* out_len) {
    if (fd < 0 || buf_size <= 0) return NULL;
    char* buf = (char*)malloc((size_t)buf_size + 1);
    if (!buf) return NULL;
    ssize_t n = recv((int)fd, buf, (size_t)buf_size, 0);
    if (n <= 0) {
        free(buf);
        if (out_len) *out_len = 0;
        return NULL;
    }
    buf[n] = '\0';
    if (out_len) *out_len = (int64_t)n;
    return buf;
}

int64_t mimi_close(int64_t fd) {
    if (fd < 0) return -1;
    int ret = close((int)fd);
    return (int64_t)ret;
}

/* ---- Simple HTTP client (non-chunked, no SSL) ---- */

/* Parse URL into host, port, path. Returns 0 on success.
   Format: http://host[:port][/path] */
static int parse_http_url(const char* url, char* host, int hostlen,
                          int* port, char* path, int pathlen) {
    if (!url) return -1;
    const char* p = url;
    /* Skip http:// */
    if (strncmp(p, "http://", 7) == 0) {
        p += 7;
    } else if (strncmp(p, "https://", 8) == 0) {
        /* We don't support TLS, but parse for error message */
        return -1;
    }
    /* Extract host */
    const char* colon = strchr(p, ':');
    const char* slash = strchr(p, '/');
    const char* host_end;
    if (colon && (!slash || colon < slash)) {
        /* host:port */
        size_t hlen = (size_t)(colon - p);
        if (hlen >= (size_t)hostlen) hlen = (size_t)hostlen - 1;
        memcpy(host, p, hlen);
        host[hlen] = '\0';
        p = colon + 1;
        *port = atoi(p);
        /* Skip to path */
        p = strchr(p, '/');
        if (!p) p = "/";
    } else if (slash) {
        size_t hlen = (size_t)(slash - p);
        if (hlen >= (size_t)hostlen) hlen = (size_t)hostlen - 1;
        memcpy(host, p, hlen);
        host[hlen] = '\0';
        *port = 80;
        p = slash;
    } else {
        /* Just host, no path */
        size_t hlen = strlen(p);
        if (hlen >= (size_t)hostlen) hlen = (size_t)hostlen - 1;
        memcpy(host, p, hlen);
        host[hlen] = '\0';
        *port = 80;
        p = "/";
    }
    strncpy(path, p, (size_t)pathlen - 1);
    path[pathlen - 1] = '\0';
    return 0;
}

/* Build and send an HTTP request, return response body */
static char* http_request(const char* host, int port, const char* request, int* out_len) {
    /* Create socket and connect */
    int64_t fd = mimi_socket(2, 1, 0);  /* AF_INET, SOCK_STREAM */
    if (fd < 0) return NULL;
    if (mimi_connect(fd, host, (int64_t)port) < 0) {
        mimi_close(fd);
        return NULL;
    }
    /* Send request */
    int64_t req_len = (int64_t)strlen(request);
    int64_t sent = mimi_send(fd, request, req_len);
    if (sent != req_len) {
        mimi_close(fd);
        return NULL;
    }
    /* Read response in chunks */
    size_t capacity = 4096;
    size_t total = 0;
    char* response = (char*)malloc(capacity);
    if (!response) { mimi_close(fd); return NULL; }
    while (1) {
        if (total + 4096 > capacity) {
            if (capacity > SIZE_MAX / 2) { free(response); mimi_close(fd); return NULL; }
            capacity *= 2;
            char* new_buf = (char*)realloc(response, capacity);
            if (!new_buf) { free(response); mimi_close(fd); return NULL; }
            response = new_buf;
        }
        int64_t chunk_len = 0;
        char* chunk = mimi_recv(fd, 4096, &chunk_len);
        if (!chunk || chunk_len <= 0) {
            if (chunk) free(chunk);
            break;
        }
        memcpy(response + total, chunk, (size_t)chunk_len);
        total += (size_t)chunk_len;
        free(chunk);
    }
    mimi_close(fd);
    if (total == 0) { free(response); return NULL; }
    response[total] = '\0';

    /* Strip HTTP headers: find \r\n\r\n or \n\n */
    char* body = strstr(response, "\r\n\r\n");
    if (body) {
        body += 4;
        size_t body_len = total - (size_t)(body - response);
        memmove(response, body, body_len);
        response[body_len] = '\0';
        if (out_len) *out_len = (int)body_len;
    } else {
        body = strstr(response, "\n\n");
        if (body) {
            body += 2;
            size_t body_len = total - (size_t)(body - response);
            memmove(response, body, body_len);
            response[body_len] = '\0';
            if (out_len) *out_len = (int)body_len;
        } else if (out_len) {
            *out_len = (int)total;
        }
    }
    return response;
}

char* mimi_http_get(const char* url) {
    char host[256];
    char path[1024];
    int port = 80;
    if (parse_http_url(url, host, sizeof(host), &port, path, sizeof(path)) != 0)
        return NULL;
    /* Build GET request */
    char request[2048];
    int n = snprintf(request, sizeof(request),
        "GET %s HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n",
        path, host);
    if (n < 0 || (size_t)n >= sizeof(request)) return NULL;
    return http_request(host, port, request, NULL);
}

char* mimi_http_post(const char* url, const char* body) {
    if (!body) body = "";
    char host[256];
    char path[1024];
    int port = 80;
    if (parse_http_url(url, host, sizeof(host), &port, path, sizeof(path)) != 0)
        return NULL;
    size_t body_len = strlen(body);
    char request[4096];
    int n = snprintf(request, sizeof(request),
        "POST %s HTTP/1.0\r\nHost: %s\r\nContent-Type: application/octet-stream\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n%s",
        path, host, body_len, body);
    if (n < 0 || (size_t)n >= sizeof(request)) return NULL;
    return http_request(host, port, request, NULL);
}

int __mimi_extern_test_positive(int x) {
    return x;
}

// G1b: Test helper — calls cb(x) and returns the result
int __mimi_extern_test_callback(int x, int (*cb)(int)) {
    return cb(x);
}

// G2: Float identity — takes f64, returns the same f64
double __mimi_extern_test_float_identity(double x) {
    return x;
}

// G3: String length (borrowed string)
int __mimi_extern_test_strlen(const char* s) {
    if (!s) return -1;
    size_t len = 0;
    while (s[len]) len++;
    return (int)len;
}

// G4: Void (no-op)
void __mimi_extern_test_nop(void) {
}

// G5: Parse int from JSON string — expects a JSON number root value
int __mimi_extern_test_parse_int(const char* json) {
    if (!json) return -1;
    const char* p = json;
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
    int neg = 0;
    if (*p == '-') { neg = 1; p++; }
    int val = 0;
    while (*p >= '0' && *p <= '9') {
        val = val * 10 + (*p - '0');
        p++;
    }
    return neg ? -val : val;
}

// G6: Allocate and return a greeting string (owned / raw_string)
// Caller must free the returned pointer.
char* __mimi_extern_test_greet(int x) {
    char buf[64];
    int n = snprintf(buf, sizeof(buf), "Hello %d", x);
    if (n < 0) return NULL;
    char* s = (char*)malloc((size_t)n + 1);
    if (!s) return NULL;
    memcpy(s, buf, (size_t)n + 1);
    return s;
}

// G7: JSON array sum — tests List/Record/Tuple serialized as JSON string
int __mimi_extern_test_json_sum(const char* json) {
    if (!json) return -1;
    const char* p = json;
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
    if (*p == '[') p++; else return -1;
    int sum = 0;
    while (*p) {
        while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r' || *p == ',') p++;
        if (*p == ']' || *p == '\0') break;
        int neg = 0;
        if (*p == '-') { neg = 1; p++; }
        int val = 0;
        while (*p >= '0' && *p <= '9') {
            val = val * 10 + (*p - '0');
            p++;
        }
        sum += neg ? -val : val;
    }
    return sum;
}

// G8: Segmentation fault — for fork isolation testing
void __mimi_extern_test_segfault(void) {
    volatile int* p = NULL;
    *p = 42;  // deliberate null dereference
}

// === Wrappers for interpreter FFI path (no __mimi_extern_ prefix) ===
// The interpreter looks up symbols by the Mimi extern name directly.
double test_float_identity(double x) { return __mimi_extern_test_float_identity(x); }
int    test_strlen(const char* s) { return __mimi_extern_test_strlen(s); }
void   test_nop(void) { __mimi_extern_test_nop(); }
int    test_parse_int(const char* json) { return __mimi_extern_test_parse_int(json); }
int    test_json_sum(const char* json) { return __mimi_extern_test_json_sum(json); }
void   test_segfault(void) { __mimi_extern_test_segfault(); }

// greet uses a static buffer to avoid allocation issues across .so boundary
char* test_greet(int x) { return __mimi_extern_test_greet(x); }
// callback wrapper
int    test_callback(int x, int (*cb)(int)) { return __mimi_extern_test_callback(x, cb); }

void mimi_runtime_abort(const char* msg) {
    if (msg) {
        fprintf(stderr, "Contract violation: %s\n", msg);
    }
    abort();
}

#endif /* MIMI_NO_STD (inner: network code) */
#endif /* MIMI_NO_STD (outer: freestanding vs libc build) */
