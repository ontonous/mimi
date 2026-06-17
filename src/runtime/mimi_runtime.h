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

#endif
