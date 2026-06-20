/**
 * Mimi FFI Runtime Library Header
 * 
 * This header declares the C ABI functions provided by libmimi_ffi_rt.
 * These functions are used by Mimi-generated code to manage shared handles,
 * capabilities, and string conversions across the FFI boundary.
 * 
 * Version: 0.1.0
 * Auto-generated from src/ffi/runtime.rs
 */

#ifndef MIMI_FFI_RT_H
#define MIMI_FFI_RT_H

#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

// ---------------------------------------------------------------------------
// Shared Handle Management
// ---------------------------------------------------------------------------

/**
 * Retain a shared handle (increment reference count).
 * 
 * @param handle The shared handle ID
 * @return The same handle ID (for chaining)
 */
int64_t mimi_shared_retain(int64_t handle);

/**
 * Release a shared handle (decrement reference count).
 * If the reference count reaches zero, the handle is removed from the table.
 * 
 * @param handle The shared handle ID
 */
void mimi_shared_release(int64_t handle);

/**
 * Get a raw pointer to the inner value of a shared handle.
 * The pointer is valid only while the handle is alive (before release).
 * 
 * @param handle The shared handle ID
 * @return Pointer to the inner value, or NULL if handle is invalid
 */
const void* mimi_shared_get_ptr(int64_t handle);

// ---------------------------------------------------------------------------
// Capability Management
// ---------------------------------------------------------------------------

/**
 * Check whether a capability is valid and matches the expected name.
 * Does NOT consume the capability.
 * 
 * @param cap The capability ID
 * @param name The expected capability name (null-terminated C string)
 * @return true if the capability is valid and matches, false otherwise
 */
bool mimi_cap_check(int64_t cap, const char* name);

/**
 * Consume a capability (mark as used).
 * Returns true if the capability was valid, matched the name, and was not
 * already consumed.
 * 
 * @param cap The capability ID
 * @param name The expected capability name (null-terminated C string)
 * @return true if the capability was successfully consumed
 */
bool mimi_cap_consume(int64_t cap, const char* name);

// ---------------------------------------------------------------------------
// String Conversion
// ---------------------------------------------------------------------------

/**
 * Get a C string pointer from a Mimi string (borrow semantics).
 * The caller must NOT free the returned pointer - Mimi retains ownership.
 * The pointer is valid until the Mimi string is garbage collected.
 * 
 * @param handle The shared handle ID containing a Mimi string
 * @return Null-terminated C string, or NULL if handle is invalid or not a string
 */
const char* mimi_string_as_c_str(int64_t handle);

/**
 * Convert a Mimi string to a raw C string (transfer ownership to C).
 * The caller is responsible for calling mimi_string_free_raw() on the result.
 * The Mimi string is cleared after conversion.
 * 
 * @param handle The shared handle ID containing a Mimi string
 * @return Owned C string that must be freed with mimi_string_free_raw(), or NULL on error
 */
char* mimi_string_into_raw(int64_t handle);

/**
 * Convert a raw C string back to a Mimi string.
 * The caller should NOT free the original C string after this call.
 * 
 * @param c_str The C string to convert (ownership transferred)
 * @return A new shared handle containing the Mimi string, or 0 on error
 */
int64_t mimi_string_from_raw(char* c_str);

/**
 * Free a raw string that was obtained via mimi_string_into_raw().
 * 
 * @param c_str The C string to free
 */
void mimi_string_free_raw(char* c_str);

#ifdef __cplusplus
}
#endif

#endif /* MIMI_FFI_RT_H */