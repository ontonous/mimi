#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <time.h>
#include "mimi_runtime.h"

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
    /* Try to interpret payload as a string pointer */
    const char* s = (const char*)payload;
    /* Heuristic: if the pointer looks like a valid userspace address and
       the first few bytes look like printable ASCII, treat it as a string */
    if (payload > 0x1000 && payload < 0x7fffffffffff) {
        /* Check first byte for printable ASCII */
        if (s[0] >= 0x20 && s[0] < 0x7f) {
            fprintf(stderr, "Error: %s\n", s);
            exit(1);
        }
    }
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

int64_t mimi_cap_register(const char* name) {
    if (cap_count >= MAX_CAPS) return -1;
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
    return id;
}

int mimi_cap_check(int64_t cap, const char* name) {
    for (int i = 0; i < cap_count; i++) {
        if (cap_table[i].id == cap && !cap_table[i].consumed) {
            if (!name || !name[0]) return 1;
            if (strcmp(cap_table[i].name, name) == 0) return 1;
            return 0;
        }
    }
    return 0;
}

int mimi_cap_consume(int64_t cap, const char* name) {
    for (int i = 0; i < cap_count; i++) {
        if (cap_table[i].id == cap && !cap_table[i].consumed) {
            if (!name || !name[0] || strcmp(cap_table[i].name, name) == 0) {
                cap_table[i].consumed = 1;
                return 1;
            }
            return 0;
        }
    }
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

    /* Calculate result length */
    size_t s_len = strlen(s);
    size_t result_len = s_len + count * (to_len - from_len);
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

#include <pthread.h>

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
static int pool_task_head = 0;
static int pool_task_tail = 0;
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
    if (pool_threads_initialized) return;
    int ncpu = sysconf(_SC_NPROCESSORS_ONLN);
    if (ncpu < 1) ncpu = 4;
    if (ncpu > POOL_MAX_THREADS) ncpu = POOL_MAX_THREADS;
    pool_thread_count = ncpu;
    for (int i = 0; i < ncpu; i++) {
        pthread_create(&pool_threads[i], NULL, pool_worker, NULL);
    }
    pool_threads_initialized = 1;
}

void mimi_pool_submit(void* fn_ptr, void* arg) {
    if (!fn_ptr) return;
    pool_ensure_init();
    void* (*func)(void*) = (void* (*)(void*))fn_ptr;
    pthread_mutex_lock(&pool_mutex);
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

void* mimi_from_json(const char* json_str) {
    (void)json_str;
    /* Stub: returns NULL (error) */
    return NULL;
}
