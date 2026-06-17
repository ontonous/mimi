#include <stdlib.h>
#include <string.h>
#include <stdint.h>
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
