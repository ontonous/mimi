#ifndef MIMI_RUNTIME_H
#define MIMI_RUNTIME_H

#include <stdint.h>

typedef uintptr_t ValueHandle;
typedef uintptr_t MapHandle;

MapHandle mimi_map_new(void);
void mimi_map_destroy(MapHandle map);
int64_t mimi_map_size(MapHandle map);
int mimi_map_has_key(MapHandle map, const char* key);
ValueHandle mimi_map_get(MapHandle map, const char* key);
void mimi_map_set(MapHandle map, const char* key, ValueHandle value);
int mimi_map_remove(MapHandle map, const char* key);
MapHandle mimi_map_from_list(ValueHandle* keys, ValueHandle* values, int64_t n);
const char* mimi_value_type_name(ValueHandle handle);

/* String functions: str_split, str_join, str_replace.
   Lists are represented as {i64 len, i64* data} where data[i] is a (char*).
   The list struct is heap-allocated; caller owns the memory. */
typedef struct { int64_t len; const char** data; } MimiList;

/* str_split(s, delim) → heap-allocated MimiList* of substrings */
MimiList* mimi_str_split(const char* s, const char* delim);

/* str_concat(a, b) → heap-allocated concatenated string (a + b) */
const char* mimi_str_concat(const char* a, const char* b);

/* str_join(list_ptr, sep) → heap-allocated joined string.
   list_ptr points to a MimiList where each data[i] is a const char*. */
const char* mimi_str_join(const MimiList* list, const char* sep);

/* str_replace(s, from, to) → heap-allocated result string */
const char* mimi_str_replace(const char* s, const char* from, const char* to);

/* mimi_try_exit(payload): print error message from ? operator and exit(1).
   payload is an i64 — if it looks like a valid string pointer, print it;
   otherwise print as integer. */
void mimi_try_exit(int64_t payload);

/* mimi_try_exit_str(str, len): print error message from ? operator
   when the error type is string. Prints the actual string content. */
void mimi_try_exit_str(const char* str, int64_t len);

/* Cap runtime functions.
   Cap IDs are int64_t. Each cap has a name and a consumed flag. */
int64_t mimi_cap_register(const char* name);
int mimi_cap_check(int64_t cap, const char* name);
int mimi_cap_consume(int64_t cap, const char* name);

/* Thread pool functions for parasteps.
   mimi_pool_submit submits a function to be run in a worker thread.
   fn_ptr is a void* that will be cast to a function pointer.
   mimi_pool_join_all waits for all submitted tasks to complete. */
void mimi_pool_submit(void* fn_ptr, void* arg);
void mimi_pool_join_all(void);

/* Time functions.
   mimi_now() returns current unix timestamp in seconds.
   mimi_now_ms() returns current unix timestamp in milliseconds.
   mimi_sleep(ms) sleeps for the given number of milliseconds. */
int64_t mimi_now(void);
int64_t mimi_now_ms(void);
void mimi_sleep(int64_t ms);

/* Environment/CLI functions.
   mimi_getenv(name) returns a pointer to the env var value, or NULL.
   mimi_args_init(argc, argv) stores CLI args for later access.
   mimi_args_count() returns the number of CLI args (excluding program name).
   mimi_args_get(i) returns the i-th CLI arg as a string, or NULL. */
const char* mimi_getenv(const char* name);
void mimi_args_init(int argc, char** argv);
int64_t mimi_args_count(void);
const char* mimi_args_get(int64_t i);

/* JSON functions.
    mimi_from_json(json_str) -> heap-allocated validated JSON string, or NULL on error.
    json_get_string(json_str, key) -> heap-allocated string field value, or NULL.
    json_get_int(json_str, key, out) -> 1 on success, 0 on failure.
    json_get_element(json_str, index) -> heap-allocated array element JSON, or NULL. */
void* mimi_from_json(const char* json_str);
const char* json_get_string(const char* json_str, const char* key);
int64_t json_get_int(const char* json_str, const char* key);
const char* json_get_element(const char* json_str, int64_t index);
int64_t mimi_is_valid_json(const char* json_str);

/* ========== Network / Socket functions ========== */

/* Create a socket: domain=AF_INET(2), type=SOCK_STREAM(1), protocol=0 -> fd or -1 */
int64_t mimi_socket(int64_t domain, int64_t type, int64_t protocol);

/* Connect socket to host:port -> 0 on success, -1 on error.
   host is a C string; port is in host byte order. */
int64_t mimi_connect(int64_t fd, const char* host, int64_t port);

/* Bind socket to local port (INADDR_ANY) -> 0 on success, -1 on error */
int64_t mimi_bind(int64_t fd, int64_t port);

/* Listen on socket with given backlog -> 0 on success, -1 on error */
int64_t mimi_listen(int64_t fd, int64_t backlog);

/* Accept a connection -> client fd, or -1 on error */
int64_t mimi_accept(int64_t fd);

/* Send data on socket -> bytes sent, or -1 on error.
   data is a raw C string (NOT a Mimi {ptr,len} struct). */
int64_t mimi_send(int64_t fd, const char* data, int64_t len);

/* Receive data from socket -> heap-allocated buffer (caller frees via free()),
   or NULL on error. *out_len receives the number of bytes read. */
char* mimi_recv(int64_t fd, int64_t buf_size, int64_t* out_len);

/* Close a file descriptor -> 0 on success, -1 on error */
int64_t mimi_close(int64_t fd);

/* HTTP convenience: GET a URL and return the response body as a heap-allocated string.
   Returns NULL on error. */
char* mimi_http_get(const char* url);

/* HTTP convenience: POST to a URL with a body and return the response body.
   Returns NULL on error. */
char* mimi_http_post(const char* url, const char* body);

/* Contract violation: print message and abort */
void mimi_runtime_abort(const char* msg);

/* Set a custom error handler for contract violations.
   When set, mimi_runtime_abort calls this handler instead of abort().
   Pass NULL to clear. Useful for pybind11 wrappers that throw C++ exceptions. */
void mimi_runtime_set_error_handler(void (*handler)(const char*));

/* Refcounted heap allocation for shared values.
   Layout: [ strong_count | weak_count | user data ... ]
   mimi_rc_alloc(size) initializes strong=1, weak=0.
   mimi_rc_retain/release manage the strong refcount.
   mimi_rc_weak_retain/release manage the weak refcount.
   The allocation is freed only when both strong and weak reach 0.
   mimi_rc_upgrade(ptr) returns ptr (and increments strong) if the object is
   still alive, or NULL if the strong refcount has already reached 0. */
void* mimi_rc_alloc(int64_t size);
void mimi_rc_retain(void* ptr);
void mimi_rc_release(void* ptr);
void mimi_rc_weak_retain(void* ptr);
void mimi_rc_weak_release(void* ptr);
void* mimi_rc_upgrade(void* ptr);

/* Regex functions (POSIX regex.h) */
int mimi_regex_match(const char* text, const char* pattern);
char* mimi_regex_find(const char* text, const char* pattern);
char* mimi_regex_replace(const char* text, const char* pattern, const char* replacement);

/* Integer power: __mimi_pow_i64(base, exp) -> base^exp (i64).
   Returns 0 on overflow (use safe_arith::checked_pow semantics). */
int64_t __mimi_pow_i64(int64_t base, int64_t exp);

#endif
