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

/* str_join(list_ptr, sep) → heap-allocated joined string.
   list_ptr points to a MimiList where each data[i] is a const char*. */
const char* mimi_str_join(const MimiList* list, const char* sep);

/* str_replace(s, from, to) → heap-allocated result string */
const char* mimi_str_replace(const char* s, const char* from, const char* to);

/* mimi_try_exit(payload): print error message from ? operator and exit(1).
   payload is an i64 — if it looks like a valid string pointer, print it;
   otherwise print as integer. */
void mimi_try_exit(int64_t payload);

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

/* JSON functions (stubs for codegen linking; actual impl in Rust runtime).
   mimi_to_json(value_ptr) -> heap-allocated JSON string.
   mimi_from_json(json_str) -> heap-allocated Value pointer (or NULL on error). */
const char* mimi_to_json(void* value_ptr);
void* mimi_from_json(const char* json_str);

#endif
