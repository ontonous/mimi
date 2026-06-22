#include <stdint.h>
#include <stddef.h>
#include "mimi_runtime.h"

/* Integer power: base^exp with overflow detection.
   Returns 0 on overflow (matching safe_arith::checked_pow semantics). */
int64_t __mimi_pow_i64(int64_t base, int64_t exp) {
    if (exp < 0) return 0;
    if (exp == 0) return 1;
    int64_t result = 1;
    int64_t b = base;
    int64_t e = exp;
    while (e > 0) {
        if (e & 1) {
            if (b != 0 && result > (INT64_MAX / b)) return 0;
            result *= b;
        }
        e >>= 1;
        if (e > 0) {
            if (b != 0 && b > (INT64_MAX / b)) return 0;
            b *= b;
        }
    }
    return result;
}

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
    /* Print the error payload as a numeric value. */
    (void)payload;
    while (1) {}
}

/* mimi_try_exit_str without stdio */
void mimi_try_exit_str(const char* str, int64_t len) {
    (void)str;
    (void)len;
    while (1) {}
}

/* MIMI_NO_STD stub: refcounted allocation uses bump allocator (no atomics) */
typedef struct { int64_t strong; int64_t weak; } MimiRcHeader;

void* mimi_rc_alloc(int64_t size) {
    size_t total = sizeof(MimiRcHeader) + (size_t)size;
    MimiRcHeader* hdr = (MimiRcHeader*)malloc(total);
    if (!hdr) return (void*)0;
    hdr->strong = 1;
    hdr->weak = 0;
    return (void*)(hdr + 1);
}
void mimi_rc_retain(void* ptr) {
    if (!ptr) return;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    hdr->strong++;
}
void mimi_rc_release(void* ptr) {
    if (!ptr) return;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    hdr->strong--;
    if (hdr->strong <= 0 && hdr->weak <= 0) free(hdr);
}
void mimi_rc_weak_retain(void* ptr) {
    if (!ptr) return;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    hdr->weak++;
}
void mimi_rc_weak_release(void* ptr) {
    if (!ptr) return;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    hdr->weak--;
    if (hdr->strong <= 0 && hdr->weak <= 0) free(hdr);
}
void* mimi_rc_upgrade(void* ptr) {
    if (!ptr) return (void*)0;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    if (hdr->strong > 0) {
        hdr->strong++;
        return ptr;
    }
    return (void*)0;
}

#else /* !MIMI_NO_STD — normal libc build */

#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <stdatomic.h>

/* ─── Platform-specific includes ─── */
#ifdef _WIN32
  #define WIN32_LEAN_AND_MEAN 1
  #include <windows.h>
  #include <winsock2.h>
  #include <ws2tcpip.h>
  #include <io.h>
  #include <process.h>

  /* POSIX compat macros */
  #define strdup _strdup
  #define close _close           /* file close only; use closesocket() for sockets */
  #define ssize_t int
  #define socklen_t int

  /* strndup — not available on MSVC */
  static inline char* win32_strndup(const char* s, size_t n) {
      size_t len = 0;
      while (len < n && s[len]) len++;
      char* p = (char*)malloc(len + 1);
      if (p) { memcpy(p, s, len); p[len] = '\0'; }
      return p;
  }
  #define strndup win32_strndup

  /* ─── pthread compat layer for Win32 ───
     Maps the pthread subset used by Mimi runtime to Win32 primitives.
     Uses SRWLOCK (zero-initializable, no cleanup needed) + CONDITION_VARIABLE. */
  typedef SRWLOCK pthread_mutex_t;
  typedef CONDITION_VARIABLE pthread_cond_t;
  #define PTHREAD_MUTEX_INITIALIZER SRWLOCK_INIT
  #define PTHREAD_COND_INITIALIZER CONDITION_VARIABLE_INIT

  static inline int win32_mutex_lock(pthread_mutex_t* m) {
      AcquireSRWLockExclusive(m); return 0;
  }
  static inline int win32_mutex_unlock(pthread_mutex_t* m) {
      ReleaseSRWLockExclusive(m); return 0;
  }
  static inline int win32_cond_wait(pthread_cond_t* cv, pthread_mutex_t* m) {
      return SleepConditionVariableSRW(cv, m, INFINITE, 0) ? 0 : -1;
  }
  static inline int win32_cond_signal(pthread_cond_t* cv) {
      WakeConditionVariable(cv); return 0;
  }
  static inline int win32_cond_broadcast(pthread_cond_t* cv) {
      WakeAllConditionVariable(cv); return 0;
  }
  #define pthread_mutex_lock(m)    win32_mutex_lock(m)
  #define pthread_mutex_unlock(m)  win32_mutex_unlock(m)
  #define pthread_cond_wait(c,m)   win32_cond_wait(c,m)
  #define pthread_cond_signal(c)   win32_cond_signal(c)
  #define pthread_cond_broadcast(c) win32_cond_broadcast(c)

  typedef HANDLE pthread_t;
  static inline int win32_pthread_create(pthread_t* t, void* attr,
                                         void* (*fn)(void*), void* arg) {
      (void)attr;
      *t = CreateThread(NULL, 0, (LPTHREAD_START_ROUTINE)fn, arg, 0, NULL);
      return *t != NULL ? 0 : -1;
  }
  static inline int win32_pthread_join(pthread_t t, void** ret) {
      (void)ret;
      DWORD r = WaitForSingleObject(t, INFINITE);
      if (r == WAIT_OBJECT_0) { CloseHandle(t); return 0; }
      return -1;
  }
  #define pthread_create(t,a,f,a2) win32_pthread_create(t,a,f,a2)
  #define pthread_join(t,r)        win32_pthread_join(t,r)

  /* Thread-local storage */
  #define THREAD_LOCAL __declspec(thread)

  /* Winsock must be initialized before any socket call */
  static int win32_wsa_init(void) {
      WSADATA wsa;
      return WSAStartup(MAKEWORD(2,2), &wsa) == 0 ? 0 : -1;
  }
  static void win32_wsa_cleanup(void) { WSACleanup(); }
#else
  #include <unistd.h>
  #include <signal.h>
  #include <setjmp.h>
  #include <pthread.h>
  #include <regex.h>
  #include <sys/socket.h>
  #include <netinet/in.h>
  #include <netinet/tcp.h>
  #include <netdb.h>
  #include <arpa/inet.h>
  #include <fcntl.h>

  #define THREAD_LOCAL __thread

  static int win32_wsa_init(void) { return 0; }
  static void win32_wsa_cleanup(void) {}
#endif

/* Reference-counted heap allocation.
   Layout: [ strong_count | weak_count | user data ... ]
   Returns pointer to user data (right after the refcount header). */
typedef struct { atomic_int_least64_t strong; atomic_int_least64_t weak; } MimiRcHeader;

void* mimi_rc_alloc(int64_t size) {
    size_t total = sizeof(MimiRcHeader) + (size_t)size;
    MimiRcHeader* hdr = (MimiRcHeader*)malloc(total);
    if (!hdr) return (void*)0;
    atomic_init(&hdr->strong, 1);
    atomic_init(&hdr->weak, 0);
    return (void*)(hdr + 1);
}

void mimi_rc_retain(void* ptr) {
    if (!ptr) return;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    atomic_fetch_add(&hdr->strong, 1);
}

void mimi_rc_release(void* ptr) {
    if (!ptr) return;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    // Use fetch_sub return value to atomically detect last-strong-drop.
    // This eliminates the TOCTOU race between fetch_sub and a subsequent
    // separate load where another thread could retain between the two.
    if (atomic_fetch_sub(&hdr->strong, 1) == 1) {
        // We were the last strong reference. If no weak refs remain, free now.
        // Otherwise weak_release will free when it drops the last weak ref.
        if (atomic_load(&hdr->weak) == 0) {
            free(hdr);
        }
    }
}

void mimi_rc_weak_retain(void* ptr) {
    if (!ptr) return;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    atomic_fetch_add(&hdr->weak, 1);
}

void mimi_rc_weak_release(void* ptr) {
    if (!ptr) return;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    // Use fetch_sub return value to atomically detect last-weak-drop,
    // avoiding the TOCTOU race between separate fetch_sub and load.
    if (atomic_fetch_sub(&hdr->weak, 1) == 1) {
        // We were the last weak reference. Free only if strong refs are also
        // gone (they must be, because strong_release defers to us when weak>0).
        if (atomic_load(&hdr->strong) <= 0) {
            free(hdr);
        }
    }
}

void* mimi_rc_upgrade(void* ptr) {
    if (!ptr) return (void*)0;
    MimiRcHeader* hdr = (MimiRcHeader*)ptr - 1;
    int64_t s;
    do {
        s = atomic_load(&hdr->strong);
        if (s == 0) return (void*)0;
    } while (!atomic_compare_exchange_weak(&hdr->strong, &s, s + 1));
    return ptr;
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

/* mimi_map_keys: returns a MimiList (MimiList*) with all keys from the map */
/* MimiList layout = { int64_t len; const char** data; } where data is an i64* array of i8* key pointers */
MimiList* mimi_map_keys(MapHandle handle) {
    MimiList* result = (MimiList*)calloc(1, sizeof(MimiList));
    if (!result || !handle) return result;
    Map* map = (Map*)handle;
    result->len = map->size;
    if (map->size == 0) {
        result->data = NULL;
        return result;
    }
    const char** keys = (const char**)malloc(map->size * sizeof(const char*));
    if (!keys) { result->len = 0; return result; }
    int64_t idx = 0;
    for (int64_t i = 0; i < map->capacity; i++) {
        if (map->entries[i].state == ENTRY_USED && map->entries[i].key) {
            keys[idx++] = map->entries[i].key;
        }
    }
    result->data = keys;
    return result;
}

/* mimi_map_values: returns a MimiList (MimiList*) with all values from the map */
/* Layout = { int64_t len; int64_t* data; } where data is an i64* array of ValueHandle values */
MimiList* mimi_map_values(MapHandle handle) {
    MimiList* result = (MimiList*)calloc(1, sizeof(MimiList));
    if (!result || !handle) return result;
    Map* map = (Map*)handle;
    result->len = map->size;
    if (map->size == 0) {
        result->data = NULL;
        return result;
    }
    int64_t* values = (int64_t*)malloc(map->size * sizeof(int64_t));
    if (!values) { result->len = 0; return result; }
    int64_t idx = 0;
    for (int64_t i = 0; i < map->capacity; i++) {
        if (map->entries[i].state == ENTRY_USED) {
            values[idx++] = (int64_t)map->entries[i].value;
        }
    }
    result->data = (const char**)values;
    return result;
}

const char* mimi_value_type_name(ValueHandle handle) {
    (void)handle;
    return "unknown";
}

/* ========== String functions ========== */

const char* mimi_str_concat(const char* a, const char* b) {
    size_t alen = strlen(a);
    size_t blen = strlen(b);
    char* result = (char*)malloc(alen + blen + 1);
    if (!result) return NULL;
    strcpy(result, a);
    strcat(result, b);
    return result;
}

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

/* mimi_try_exit_str with stdio: print actual string content, not a number. */
void mimi_try_exit_str(const char* str, int64_t len) {
    if (str && len > 0) {
        fprintf(stderr, "Error: Result::Err(\"%.*s\")\n", (int)len, str);
    } else {
        fprintf(stderr, "Error: Result::Err(\"\")\n");
    }
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
    int ncpu;
#ifdef _WIN32
    SYSTEM_INFO sysinfo;
    GetSystemInfo(&sysinfo);
    ncpu = (int)sysinfo.dwNumberOfProcessors;
#else
    ncpu = (int)sysconf(_SC_NPROCESSORS_ONLN);
#endif
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

#ifdef _WIN32
int64_t mimi_now(void) {
    FILETIME ft;
    GetSystemTimeAsFileTime(&ft);
    ULARGE_INTEGER li;
    li.LowPart = ft.dwLowDateTime;
    li.HighPart = ft.dwHighDateTime;
    /* FILETIME is 100-ns intervals since 1601-01-01. Convert to Unix epoch. */
    return (int64_t)((li.QuadPart - 116444736000000000ULL) / 10000000);
}

int64_t mimi_now_ms(void) {
    FILETIME ft;
    GetSystemTimeAsFileTime(&ft);
    ULARGE_INTEGER li;
    li.LowPart = ft.dwLowDateTime;
    li.HighPart = ft.dwHighDateTime;
    return (int64_t)((li.QuadPart - 116444736000000000ULL) / 10000);
}

void mimi_sleep(int64_t ms) {
    if (ms > 0) Sleep((DWORD)ms);
}
#else
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
#endif

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

/* ========== Regex functions ========== */

#ifndef MIMI_NO_STD

/* ─── Platform-specific POSIX regex API ─── */
#ifdef _WIN32

/* Minimal POSIX regex shim for Win32.
   Recursive backtracking NFA matcher supporting: . * + \d \w \s
   [a-z] [^...] ^ $ and literal characters. */
#include <ctype.h>

typedef struct { char* pattern; } regex_t;
typedef struct { regoff_t rm_so, rm_eo; } regmatch_t;
#define REG_EXTENDED 1

static int regcomp(regex_t* preg, const char* pattern, int cflags) {
    (void)cflags;
    preg->pattern = _strdup(pattern);
    return preg->pattern ? 0 : -1;
}

/* Advance past one pattern element (class, escape, literal). */
static const char* skip_elem(const char* p) {
    if (p[0] == '\\' && p[1]) return p + 2;
    if (p[0] == '[') {
        const char* e = p + 1;
        if (*e == '^') e++;
        while (*e && *e != ']') {
            if (*e == '\\' && *(e+1)) e += 2; else e++;
        }
        return *e == ']' ? e + 1 : e;
    }
    return p + 1;
}

/* Does pattern element at *pp match character c?  Advances *pp past element. */
static int elem_match(const char** pp, char c) {
    const char* p = *pp;
    if (p[0] == '\\') {
        int m = 0;
        switch (p[1]) {
            case 'd': m = isdigit((unsigned char)c); break;
            case 'D': m = !isdigit((unsigned char)c); break;
            case 'w': m = isalnum((unsigned char)c) || c == '_'; break;
            case 'W': m = !(isalnum((unsigned char)c) || c == '_'); break;
            case 's': m = isspace((unsigned char)c); break;
            case 'S': m = !isspace((unsigned char)c); break;
            default:  m = (c == p[1]); break;
        }
        *pp = p + 2; return m;
    }
    if (p[0] == '[') {
        const char* end = p + 1;
        int neg = 0;
        if (*end == '^') { neg = 1; end++; }
        int m = 0;
        while (*end && *end != ']') {
            if (end[1] == '-' && end[2] && end[2] != ']') {
                if (c >= end[0] && c <= end[2]) { m = 1; break; }
                end += 3;
            } else {
                if (c == end[0]) { m = 1; break; }
                end++;
            }
        }
        while (*end && *end != ']') {
            if (*end == '\\' && *(end+1)) end += 2; else end++;
        }
        *pp = (*end == ']') ? end + 1 : end;
        return neg ? !m : m;
    }
    if (p[0] == '.') { *pp = p + 1; return c != '\0' && c != '\n'; }
    *pp = p + 1; return c == p[0];
}

/* Recursive match: pattern at p against text at s.
   Returns the total number of text chars consumed on success, -1 on failure. */
static int match_here(const char* p, const char* s) {
    while (*p == '^') p++;
    for (;;) {
        if (!*p) return 0;                           /* done */
        if (p[0] == '$' && p[1] == '\0') return *s ? -1 : 0;
        const char* elem = p;
        const char* next = skip_elem(p);
        int has_star = (next[0] == '*');
        int has_plus = (next[0] == '+');
        if (has_star || has_plus) {
            const char* after_q = next + 1;
            /* Count how many elem matches starting at s */
            int max_n = 0;
            const char* t;
            for (t = s; *t; ) {
                const char* tmp = p;
                if (!elem_match(&tmp, *t)) break;
                t++; max_n++;
            }
            int min_n = has_plus ? 1 : 0;
            /* Try from max_n down to min_n (greedy) */
            for (int n = max_n; n >= min_n; n--) {
                int r = match_here(after_q, s + n);
                if (r >= 0) return n + r;
            }
            return -1;
        }
        if (!*s) return -1;
        const char* tmp = p;
        if (!elem_match(&tmp, *s)) return -1;
        p = tmp; s++;
    }
}

static int regexec(const regex_t* preg, const char* string, size_t nmatch,
                   regmatch_t* pmatch, int eflags) {
    (void)eflags;
    if (!preg->pattern || !string) return 1;
    int anchored = (preg->pattern[0] == '^');
    for (const char* start = string; ; start++) {
        int r = match_here(preg->pattern, start);
        if (r >= 0) {
            if (pmatch && nmatch > 0) {
                pmatch[0].rm_so = (regoff_t)(start - string);
                pmatch[0].rm_eo = (regoff_t)((start - string) + r);
            }
            return 0;
        }
        if (anchored || !*start) break;
    }
    return 1; /* REG_NOMATCH */
}

static void regfree(regex_t* preg) {
    free(preg->pattern);
    preg->pattern = NULL;
}

#else /* POSIX */
  #include <regex.h>
#endif /* _WIN32 / POSIX */

/* ─── Platform-independent Mimi regex wrappers ─── */

/* regex_match(text, pattern) -> int (0 or 1) */
int mimi_regex_match(const char* text, const char* pattern) {
    if (!text || !pattern) return 0;
    regex_t regex;
    int ret = regcomp(&regex, pattern, REG_EXTENDED);
    if (ret != 0) return 0;
    ret = regexec(&regex, text, 0, NULL, 0);
    regfree(&regex);
    return ret == 0 ? 1 : 0;
}

/* regex_find(text, pattern) -> malloc'd first match substring (empty on no match) */
char* mimi_regex_find(const char* text, const char* pattern) {
    if (!text || !pattern) {
        char* empty = (char*)malloc(1);
        if (empty) empty[0] = '\0';
        return empty;
    }
    regex_t regex;
    if (regcomp(&regex, pattern, REG_EXTENDED) != 0) {
        char* empty = (char*)malloc(1);
        if (empty) empty[0] = '\0';
        return empty;
    }
    regmatch_t match;
    int ret = regexec(&regex, text, 1, &match, 0);
    regfree(&regex);
    if (ret != 0) {
        char* empty = (char*)malloc(1);
        if (empty) empty[0] = '\0';
        return empty;
    }
    size_t len = (size_t)(match.rm_eo - match.rm_so);
    char* result = (char*)malloc(len + 1);
    if (result) {
        strncpy(result, text + match.rm_so, len);
        result[len] = '\0';
    }
    return result;
}

/* regex_replace(text, pattern, replacement) -> malloc'd result string */
char* mimi_regex_replace(const char* text, const char* pattern, const char* replacement) {
    if (!text || !pattern || !replacement) return NULL;
    regex_t regex;
    if (regcomp(&regex, pattern, REG_EXTENDED) != 0) {
        char* empty = (char*)malloc(1);
        if (empty) empty[0] = '\0';
        return empty;
    }
    regmatch_t match;
    const char* cursor = text;
    size_t result_cap = strlen(text) * 2 + 16;
    if (result_cap < 64) result_cap = 64;
    char* result = (char*)malloc(result_cap);
    if (!result) { regfree(&regex); return NULL; }
    result[0] = '\0';
    size_t offset = 0;
    while (regexec(&regex, cursor, 1, &match, 0) == 0) {
        size_t prefix_len = (size_t)match.rm_so;
        if (offset + prefix_len + strlen(replacement) + 1 > result_cap) {
            result_cap = (result_cap + prefix_len + strlen(replacement) + 1) * 2;
            char* new_result = (char*)realloc(result, result_cap);
            if (!new_result) { free(result); regfree(&regex); return NULL; }
            result = new_result;
        }
        if (prefix_len > 0) {
            strncpy(result + offset, cursor, prefix_len);
            offset += prefix_len;
        }
        size_t repl_len = strlen(replacement);
        strncpy(result + offset, replacement, repl_len);
        offset += repl_len;
        cursor += (size_t)match.rm_eo;
    }
    size_t remaining = strlen(cursor);
    if (offset + remaining + 1 > result_cap) {
        result_cap = offset + remaining + 1;
        char* new_result = (char*)realloc(result, result_cap);
        if (!new_result) { free(result); regfree(&regex); return NULL; }
        result = new_result;
    }
    strncpy(result + offset, cursor, remaining);
    offset += remaining;
    result[offset] = '\0';
    regfree(&regex);
    return result;
}


/* ========== Network / Socket functions ========== */

/* Socket API is provided by platform headers at the top of the file.
   On Win32, Winsock2 provides mostly-identical function signatures
   but requires closesocket() instead of close() for socket cleanup. */

int64_t mimi_socket(int64_t domain, int64_t type, int64_t protocol) {
#ifdef _WIN32
    SOCKET fd = socket((int)domain, (int)type, (int)protocol);
    if (fd != INVALID_SOCKET) {
        int reuse = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, (const char*)&reuse, sizeof(reuse));
    }
    return (fd == INVALID_SOCKET) ? -1 : (int64_t)fd;
#else
    int fd = socket((int)domain, (int)type, (int)protocol);
    if (fd >= 0) {
        int reuse = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
    }
    return (int64_t)fd;
#endif
}

int64_t mimi_connect(int64_t fd, const char* host, int64_t port) {
    if (!host || fd < 0) return -1;
    struct addrinfo hints, *res;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    char port_str[16];
    snprintf(port_str, sizeof(port_str), "%lld", (long long)port);
    int err = getaddrinfo(host, port_str, &hints, &res);
    if (err != 0 || !res) return -1;
#ifdef _WIN32
    int ret = connect((SOCKET)fd, res->ai_addr, (int)res->ai_addrlen);
#else
    int ret = connect((int)fd, res->ai_addr, res->ai_addrlen);
#endif
    if (ret == 0) {
        int flag = 1;
#ifdef _WIN32
        setsockopt((SOCKET)fd, IPPROTO_TCP, TCP_NODELAY, (const char*)&flag, sizeof(flag));
#else
        setsockopt((int)fd, IPPROTO_TCP, TCP_NODELAY, (const char*)&flag, sizeof(flag));
#endif
    }
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
#ifdef _WIN32
    int ret = bind((SOCKET)fd, (struct sockaddr*)&addr, sizeof(addr));
#else
    int ret = bind((int)fd, (struct sockaddr*)&addr, sizeof(addr));
#endif
    return (int64_t)ret;
}

int64_t mimi_listen(int64_t fd, int64_t backlog) {
    if (fd < 0) return -1;
#ifdef _WIN32
    int ret = listen((SOCKET)fd, (int)backlog);
#else
    int ret = listen((int)fd, (int)backlog);
#endif
    return (int64_t)ret;
}

int64_t mimi_accept(int64_t fd) {
    if (fd < 0) return -1;
    struct sockaddr_in client_addr;
    socklen_t addr_len = (socklen_t)sizeof(client_addr);
#ifdef _WIN32
    SOCKET client_fd = accept((SOCKET)fd, (struct sockaddr*)&client_addr, &addr_len);
    return (client_fd == INVALID_SOCKET) ? -1 : (int64_t)client_fd;
#else
    int client_fd = accept((int)fd, (struct sockaddr*)&client_addr, &addr_len);
    return (int64_t)client_fd;
#endif
}

int64_t mimi_send(int64_t fd, const char* data, int64_t len) {
    if (fd < 0 || !data) return -1;
#ifdef _WIN32
    int sent = send((SOCKET)fd, data, (int)len, 0);
#else
    ssize_t sent = send((int)fd, data, (size_t)len, 0);
#endif
    return (int64_t)sent;
}

char* mimi_recv(int64_t fd, int64_t buf_size, int64_t* out_len) {
    if (fd < 0 || buf_size <= 0) return NULL;
    char* buf = (char*)malloc((size_t)buf_size + 1);
    if (!buf) return NULL;
#ifdef _WIN32
    int n = recv((SOCKET)fd, buf, (int)buf_size, 0);
#else
    ssize_t n = recv((int)fd, buf, (size_t)buf_size, 0);
#endif
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
#ifdef _WIN32
    int ret = closesocket((SOCKET)fd);
#else
    int ret = close((int)fd);
#endif
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

// Struct-by-value: add two int32 fields of a #[repr(C)] struct
typedef struct { int32_t x; int32_t y; } __mimi_TestPoint;
int32_t __mimi_extern_test_struct_by_val(__mimi_TestPoint p) {
    return p.x + p.y;
}

// Struct with mixed types: compute weighted sum
typedef struct { int32_t id; double value; int32_t flag; } __mimi_MixedStruct;
double __mimi_extern_test_mixed_struct(__mimi_MixedStruct s) {
    return (double)s.id + s.value + (double)s.flag;
}

// Nested struct: inner struct + outer field
typedef struct { int32_t a; int32_t b; } __mimi_Inner;
typedef struct { __mimi_Inner inner; int32_t c; } __mimi_Outer;
int32_t __mimi_extern_test_nested_struct(__mimi_Outer o) {
    return o.inner.a + o.inner.b + o.c;
}

// Struct with i64 fields (like stat timestamp)
typedef struct { int64_t sec; int64_t nsec; } __mimi_Timespec;
int64_t __mimi_extern_test_timespec_sum(__mimi_Timespec t) {
    return t.sec + t.nsec;
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

// Codegen JSON FFI: serialize Mimi list {i64 len, i8* data} to JSON string
// data points to i64 array (Mimi's universal element representation).
// elem_type: 0=int, 1=float (bitcast f64 bits), 2=string (i8* ptr).
// Returns malloc'd char* (caller must free).
char* mimi_json_serialize(void* data, int64_t len, int64_t elem_type) {
    if (!data || len <= 0) {
        char* empty = (char*)malloc(3);
        if (!empty) return NULL;
        empty[0] = '['; empty[1] = ']'; empty[2] = '\0';
        return empty;
    }
    size_t buf_size = (size_t)len * 64 + 16;
    char* buf = (char*)malloc(buf_size);
    if (!buf) return NULL;
    char* p = buf;
    *p++ = '[';
    for (int64_t i = 0; i < len; i++) {
        if (i > 0) *p++ = ',';
        int64_t raw = ((int64_t*)data)[i];
        if (elem_type == 1) {
            double val;
            memcpy(&val, &raw, sizeof(val));
            int nd = sprintf(p, "%g", val);
            p += nd;
        } else if (elem_type == 2) {
            char* s = (char*)raw;
            *p++ = '"';
            if (s) {
                while (*s) {
                    if (*s == '"' || *s == '\\') *p++ = '\\';
                    *p++ = *s++;
                }
            }
            *p++ = '"';
        } else {
            char tmp[24];
            int nd = 0;
            int64_t val = raw;
            if (val < 0) { tmp[nd++] = '-'; val = -val; }
            if (val == 0) {
                tmp[nd++] = '0';
            } else {
                char rev[24];
                int nr = 0;
                while (val > 0) { rev[nr++] = (char)('0' + (val % 10)); val /= 10; }
                for (int j = nr - 1; j >= 0; j--) tmp[nd++] = rev[j];
            }
            for (int j = 0; j < nd; j++) *p++ = tmp[j];
        }
    }
    *p++ = ']';
    *p = '\0';
    return buf;
}

// Backward-compatible wrapper for existing codegen i64-only callers
char* mimi_list_serialize(void* data, int64_t len) {
    return mimi_json_serialize(data, len, 0);
}

// Codegen JSON FFI: deserialize JSON string "[e1,e2,...]" to Mimi list data.
// elem_type: 0=int, 1=float, 2=string.
// Returns malloc'd i64 array. Sets *out_len. Caller must free the result.
void* mimi_json_deserialize(const char* json, int64_t* out_len, int64_t elem_type) {
    if (!json) { *out_len = 0; return NULL; }
    const char* p = json;
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
    if (*p != '[') { *out_len = 0; return NULL; }
    p++;
    int64_t count = 0;
    while (*p) {
        while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r' || *p == ',') { p++; if (*p == '\0') break; }
        if (*p == ']') break;
        if (elem_type == 2 && *p == '"') {
            count++;
            p++;
            while (*p && *p != '"') { if (*p == '\\') p++; p++; }
            if (*p == '"') p++;
        } else if (*p >= '0' && *p <= '9') { count++; while (*p >= '0' && *p <= '9') p++; if (*p == '.') { p++; while (*p >= '0' && *p <= '9') p++; } }
        else if (*p == '-') { count++; p++; while (*p >= '0' && *p <= '9') p++; if (*p == '.') { p++; while (*p >= '0' && *p <= '9') p++; } }
        else p++;
    }
    int64_t* data = (int64_t*)malloc((count + 1) * sizeof(int64_t));
    if (!data) { *out_len = 0; return NULL; }
    p = json;
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
    if (*p == '[') p++;
    int64_t idx = 0;
    while (*p && idx < count) {
        while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r' || *p == ',') { p++; if (*p == '\0') break; }
        if (*p == ']') break;
        if (elem_type == 1) {
            // Float: parse as double, store bits as i64
            char* end = NULL;
            double fval = strtod(p, &end);
            if (end != p) {
                int64_t bits;
                memcpy(&bits, &fval, sizeof(bits));
                data[idx++] = bits;
                p = end;
            } else {
                data[idx++] = 0; p++;
            }
        } else if (elem_type == 2) {
            // String: parse quoted string, store pointer to malloc'd copy
            if (*p == '"') p++;
            const char* start = p;
            while (*p && *p != '"') { if (*p == '\\') p++; p++; }
            int64_t slen = p - start;
            char* s = (char*)malloc(slen + 1);
            if (s) {
                strncpy(s, start, slen);
                s[slen] = '\0';
                data[idx++] = (int64_t)(intptr_t)s;
            } else {
                data[idx++] = 0;
            }
            if (*p == '"') p++;
        } else {
            int neg = 0;
            if (*p == '-') { neg = 1; p++; }
            int64_t val = 0;
            while (*p >= '0' && *p <= '9') { val = val * 10 + (*p - '0'); p++; }
            data[idx++] = neg ? -val : val;
        }
    }
    *out_len = count;
    return (void*)data;
}

// Backward-compatible wrapper for existing codegen i64-only callers
void* mimi_list_deserialize(const char* json, int64_t* out_len) {
    return mimi_json_deserialize(json, out_len, 0);
}

// F7: Tuple serialization for codegen FFI — serialize heterogeneous tuple to JSON array.
// values: array of i64 (one per element, bitcast to i64).
// count: number of elements.
// elem_types: array of i64 tags, one per element (0=int, 1=float, 2=string).
// Returns malloc'd char* JSON array. Caller must free.
char* mimi_tuple_serialize(int64_t* values, int64_t count, int64_t* elem_types) {
    if (!values || count <= 0) {
        char* empty = (char*)malloc(3);
        if (!empty) return NULL;
        empty[0] = '['; empty[1] = ']'; empty[2] = '\0';
        return empty;
    }
    size_t buf_size = (size_t)count * 64 + 16;
    char* buf = (char*)malloc(buf_size);
    if (!buf) return NULL;
    char* p = buf;
    *p++ = '[';
    for (int64_t i = 0; i < count; i++) {
        if (i > 0) *p++ = ',';
        int64_t raw = values[i];
        int64_t tag = elem_types ? elem_types[i] : 0;
        if (tag == 1) {
            double val;
            memcpy(&val, &raw, sizeof(val));
            int nd = sprintf(p, "%g", val);
            p += nd;
        } else if (tag == 2) {
            char* s = (char*)raw;
            *p++ = '"';
            if (s) {
                while (*s) {
                    if (*s == '"' || *s == '\\') *p++ = '\\';
                    *p++ = *s++;
                }
            }
            *p++ = '"';
        } else {
            char tmp[24];
            int nd = 0;
            int64_t val = raw;
            if (val < 0) { tmp[nd++] = '-'; val = -val; }
            if (val == 0) {
                tmp[nd++] = '0';
            } else {
                char rev[24];
                int nr = 0;
                while (val > 0) { rev[nr++] = (char)('0' + (val % 10)); val /= 10; }
                for (int j = nr - 1; j >= 0; j--) tmp[nd++] = rev[j];
            }
            for (int j = 0; j < nd; j++) *p++ = tmp[j];
        }
    }
    *p++ = ']';
    *p = '\0';
    return buf;
}

// F7: Tuple deserialization for codegen FFI — parse JSON array back to i64 values.
// json: JSON array string (e.g. "[1,2.5,\"hello\"]").
// count: number of elements expected.
// elem_types: array of i64 tags, one per element (0=int, 1=float, 2=string).
// out_values: pre-allocated array of count i64s (caller provides buffer).
// Returns the number of elements actually parsed, or -1 on error.
// String elements produce heap-allocated C strings stored as pointers in out_values;
// caller must free them using libc::free for each string element.
int64_t mimi_tuple_deserialize(const char* json, int64_t count, int64_t* elem_types, int64_t* out_values) {
    if (!json || !out_values || count <= 0) return -1;
    const char* p = json;
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
    if (*p != '[') return -1;
    p++;
    int64_t idx = 0;
    while (*p && idx < count) {
        while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r' || *p == ',') { p++; if (*p == '\0') break; }
        if (*p == ']') break;
        int64_t tag = elem_types ? elem_types[idx] : 0;
        if (tag == 1) {
            // Float
            char* end = NULL;
            double fval = strtod(p, &end);
            if (end != p) {
                int64_t bits;
                memcpy(&bits, &fval, sizeof(bits));
                out_values[idx++] = bits;
                p = end;
            } else {
                out_values[idx++] = 0; p++;
            }
        } else if (tag == 2) {
            // String
            if (*p == '"') p++;
            const char* start = p;
            while (*p && *p != '"') { if (*p == '\\') p++; p++; }
            int64_t slen = p - start;
            char* s = (char*)malloc(slen + 1);
            if (s) {
                strncpy(s, start, slen);
                s[slen] = '\0';
                out_values[idx++] = (int64_t)(intptr_t)s;
            } else {
                out_values[idx++] = 0;
            }
            if (*p == '"') p++;
        } else {
            // Int
            int neg = 0;
            if (*p == '-') { neg = 1; p++; }
            int64_t val = 0;
            while (*p >= '0' && *p <= '9') { val = val * 10 + (*p - '0'); p++; }
            out_values[idx++] = neg ? -val : val;
        }
    }
    return idx;
}

// G8: Segmentation fault — for fork isolation testing
void __mimi_extern_test_segfault(void) {
    volatile int* p = NULL;
    *p = 42;  // deliberate null dereference
}

// G9: Abort — for #[no_panic] signal handler testing
void __mimi_extern_test_abort(void) {
    abort();
}

// === Wrappers for interpreter FFI path (no __mimi_extern_ prefix) ===
// The interpreter looks up symbols by the Mimi extern name directly.
double test_float_identity(double x) { return __mimi_extern_test_float_identity(x); }
int    test_strlen(const char* s) { return __mimi_extern_test_strlen(s); }
void   test_nop(void) { __mimi_extern_test_nop(); }
int    test_parse_int(const char* json) { return __mimi_extern_test_parse_int(json); }
int    test_json_sum(const char* json) { return __mimi_extern_test_json_sum(json); }
void   test_segfault(void) { __mimi_extern_test_segfault(); }
void   test_abort(void) { __mimi_extern_test_abort(); }

int    test_struct_by_val(__mimi_TestPoint p) { return __mimi_extern_test_struct_by_val(p); }
double test_mixed_struct(__mimi_MixedStruct s) { return __mimi_extern_test_mixed_struct(s); }
int    test_nested_struct(__mimi_Outer o) { return __mimi_extern_test_nested_struct(o); }
int64_t test_timespec_sum(__mimi_Timespec t) { return __mimi_extern_test_timespec_sum(t); }
// greet uses a static buffer to avoid allocation issues across .so boundary
char* test_greet(int x) { return __mimi_extern_test_greet(x); }
// callback wrapper
int    test_callback(int x, int (*cb)(int)) { return __mimi_extern_test_callback(x, cb); }

// Thread-local jump buffer for #[no_panic] C crash recovery.
// When a signal handler fires, it siglongjmps here to escape the crashing C function.
// NULL means no recovery is in progress (signal = crash).
static THREAD_LOCAL sigjmp_buf* mimi_no_panic_jump_buf = NULL;

// Saved signal handlers for #[no_panic] restoration.
static THREAD_LOCAL struct sigaction mimi_old_sigsegv;
static THREAD_LOCAL struct sigaction mimi_old_sigabrt;
static THREAD_LOCAL struct sigaction mimi_old_sigbus;
static THREAD_LOCAL struct sigaction mimi_old_sigill;
static THREAD_LOCAL struct sigaction mimi_old_sigfpe;

/// Signal handler for #[no_panic] C crash recovery.
/// Restores SIG_DFL (so a second crash terminates), then longjmps.
static void mimi_no_panic_handler(int sig) {
    signal(SIGSEGV, SIG_DFL);
    signal(SIGABRT, SIG_DFL);
    signal(SIGBUS, SIG_DFL);
    signal(SIGILL, SIG_DFL);
    signal(SIGFPE, SIG_DFL);
    if (mimi_no_panic_jump_buf) {
        siglongjmp(*mimi_no_panic_jump_buf, sig);
    }
}

/// Install crash-recovery signal handlers for #[no_panic] FFI calls.
/// Must be paired with mimi_restore_no_panic_handlers().
void mimi_install_no_panic_handlers(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_NODEFER;
    sa.sa_handler = mimi_no_panic_handler;
    sigaction(SIGSEGV, &sa, &mimi_old_sigsegv);
    sigaction(SIGABRT, &sa, &mimi_old_sigabrt);
    sigaction(SIGBUS, &sa, &mimi_old_sigbus);
    sigaction(SIGILL, &sa, &mimi_old_sigill);
    sigaction(SIGFPE, &sa, &mimi_old_sigfpe);
}

/// Restore previously saved signal handlers after #[no_panic] FFI call.
void mimi_restore_no_panic_handlers(void) {
    sigaction(SIGSEGV, &mimi_old_sigsegv, NULL);
    sigaction(SIGABRT, &mimi_old_sigabrt, NULL);
    sigaction(SIGBUS, &mimi_old_sigbus, NULL);
    sigaction(SIGILL, &mimi_old_sigill, NULL);
    sigaction(SIGFPE, &mimi_old_sigfpe, NULL);
    mimi_no_panic_jump_buf = NULL;
}

// Thread-local error handler for contract violations.
// When set (by pybind11 wrappers), mimi_runtime_abort calls the handler instead of abort().
// The handler may throw a C++ exception or longjmp to a recovery point.
static THREAD_LOCAL void (*mimi_runtime_error_handler)(const char*) = NULL;

void mimi_runtime_set_error_handler(void (*handler)(const char*)) {
    mimi_runtime_error_handler = handler;
}

void mimi_runtime_abort(const char* msg) {
    if (msg) {
        fprintf(stderr, "[FFI contract violation] %s\n", msg);
    } else {
        fprintf(stderr, "[FFI contract violation] (no details)\n");
    }
    if (mimi_runtime_error_handler) {
        void (*handler)(const char*) = mimi_runtime_error_handler;
        mimi_runtime_error_handler = NULL; // prevent re-entry
        handler(msg);
        return;
    }
    fprintf(stderr, "Hint: use --skip-verify-ffi to disable contract checking.\n");
    abort();
}

#endif /* MIMI_NO_STD (inner: network code) */
#endif /* MIMI_NO_STD (outer: freestanding vs libc build) */
