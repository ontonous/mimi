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

#endif
