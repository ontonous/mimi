//! Runtime support for the closed Canonical MIR MapRoot profile.
//!
//! MapRoot uses the production Map storage while retaining the closed MIR
//! profile: static UTF-8 keys, i32 values, and a Checker receipt that prevents
//! handle escape or aliasing.

fn abort(message: &'static [u8]) -> ! {
    // SAFETY: every caller passes a static, NUL-terminated diagnostic.
    unsafe { super::mimi_runtime_abort(message.as_ptr().cast()) }
}

fn checked_size_i32(len: u128) -> Result<i32, &'static str> {
    i32::try_from(len).map_err(|_| "E0802: canonical MIR MapRoot.size result overflows i32")
}

fn checked_size_i32_or_abort(len: u128) -> i32 {
    checked_size_i32(len)
        .unwrap_or_else(|_| abort(b"[E0802] canonical MIR MapRoot.size result overflows i32\0"))
}

/// Allocate one empty production Map for the Canonical MIR MapRoot ABI.
#[no_mangle]
pub extern "C" fn mimi_mir_map_root_new() -> i64 {
    super::mimi_map_new()
}

/// Insert a static UTF-8 key into one exclusively owned MapRoot.
///
/// The MapRoot MIR receipt proves this operation consumes the source root and
/// returns the same physical handle as its new root. This in-place update is
/// observationally equivalent to the reference/bytecode persistent copy only
/// within that no-alias, no-escape profile.
///
/// # Safety
/// For `key_len > 0`, `key` must point to `key_len` readable bytes for this
/// call. The bytes must be UTF-8. The handle must be a live MapRoot created by
/// `mimi_mir_map_root_new`; the MIR contract supplies exclusive ownership.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_map_root_set(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: i32,
) -> i64 {
    if handle == 0 || key.is_null() || key_len < 0 {
        abort(b"[E0800] canonical MIR MapRoot.set received an invalid operand\0");
    }
    let Ok(key_len) = usize::try_from(key_len) else {
        abort(b"[E0800] canonical MIR MapRoot.set key length is invalid\0");
    };
    // SAFETY: the ABI precondition requires the byte range to remain readable
    // for this call; null and negative lengths were rejected above.
    let key_bytes = unsafe { std::slice::from_raw_parts(key, key_len) };
    let Ok(key) = std::str::from_utf8(key_bytes) else {
        abort(b"[E0800] canonical MIR MapRoot.set key is not valid UTF-8\0");
    };
    if key.contains('\0') {
        abort(b"[E0800] canonical MIR MapRoot.set key contains NUL\0");
    }
    // The checker-only path validates and mutates under the registry lock;
    // it does not create a thread-id lease (which is unnecessary here).
    super::handle::map_root_set(handle, key.to_owned(), i64::from(value))
        .unwrap_or_else(|_| abort(b"[E0800] canonical MIR MapRoot handle is busy or not live\0"));
    handle
}

/// Clone and insert one typed Mimi String as a map-owned Any payload. The
/// payload uses exact byte length and is reclaimed on overwrite or when the
/// final Map clone is destroyed.
///
/// # Safety
/// For `key_len > 0` and `value_len > 0`, the corresponding pointers must
/// reference readable byte ranges for this call. The key must be UTF-8 and
/// NUL-free. The handle must be a live, exclusively owned MapRoot.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_map_root_set_string(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: *const std::ffi::c_char,
    value_len: i64,
) -> i64 {
    if handle == 0
        || key.is_null()
        || key_len < 0
        || value.is_null() && value_len != 0
        || value_len < 0
    {
        abort(b"[E0800] canonical MIR MapRoot String Set received an invalid operand\0");
    }
    let (Ok(key_len), Ok(value_len)) = (usize::try_from(key_len), usize::try_from(value_len))
    else {
        abort(b"[E0800] canonical MIR MapRoot String Set length is invalid\0");
    };
    // SAFETY: the ABI contract requires readable key/value byte ranges for
    // the checked explicit lengths; empty String accepts a null data pointer.
    let key_bytes = unsafe { std::slice::from_raw_parts(key, key_len) };
    let Ok(key) = std::str::from_utf8(key_bytes) else {
        abort(b"[E0800] canonical MIR MapRoot String Set key is not valid UTF-8\0");
    };
    if key.contains('\0') {
        abort(b"[E0800] canonical MIR MapRoot String Set key contains NUL\0");
    }
    let tagged = unsafe { super::mimi_any_string_clone(value, value_len as i64) };
    if tagged == 0 {
        abort(b"[E0800] canonical MIR MapRoot String clone failed\0");
    }
    match super::handle::map_root_set_owned_any_string(handle, key.to_owned(), tagged) {
        Ok(()) => handle,
        Err(_) => {
            let Ok(tagged) = usize::try_from(tagged) else {
                std::process::abort();
            };
            // SAFETY: `tagged` is the fresh allocation just returned by
            // `mimi_any_string_clone`; the failed map operation did not take
            // ownership of it.
            super::mimi_free(((tagged & !1) as *mut u8).cast());
            abort(b"[E0800] canonical MIR MapRoot String handle is busy or not live\0");
        }
    }
}

/// Remove one static key from a live MapRoot and reclaim its detached String
/// payload when this is the final table/clone owner. Missing keys are a no-op.
///
/// # Safety
/// `key` must be a non-null pointer to `key_len` readable bytes (including
/// when the length is zero). The bytes must be UTF-8 and NUL-free. The handle
/// must be a live MapRoot whose Checker receipt excludes aliases and value
/// escapes.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_map_root_remove(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i64 {
    if handle == 0 || key.is_null() || key_len < 0 {
        abort(b"[E0800] canonical MIR MapRoot.remove received an invalid operand\0");
    }
    let Ok(key_len) = usize::try_from(key_len) else {
        abort(b"[E0800] canonical MIR MapRoot.remove key length is invalid\0");
    };
    // SAFETY: the ABI precondition requires this exact key range to be
    // readable for the call; null and negative lengths were rejected above.
    let key_bytes = unsafe { std::slice::from_raw_parts(key, key_len) };
    let Ok(key) = std::str::from_utf8(key_bytes) else {
        abort(b"[E0800] canonical MIR MapRoot.remove key is not valid UTF-8\0");
    };
    if key.contains('\0') {
        abort(b"[E0800] canonical MIR MapRoot.remove key contains NUL\0");
    }
    super::handle::map_root_remove(handle, key)
        .unwrap_or_else(|_| abort(b"[E0800] canonical MIR MapRoot handle is busy or not live\0"));
    handle
}

/// Return whether one live MapRoot contains a static UTF-8 key. This reads
/// only the key index; the stored `ValueHandle` is never inspected.
///
/// # Safety
/// For `key_len > 0`, `key` must point to `key_len` readable bytes for this
/// call. The bytes must be UTF-8 and NUL-free. The handle must be a live
/// MapRoot whose Checker receipt excludes aliases and value escapes.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_map_root_contains(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i32 {
    if handle == 0 || key.is_null() || key_len < 0 {
        abort(b"[E0800] canonical MIR MapRoot.contains received an invalid operand\0");
    }
    let Ok(key_len) = usize::try_from(key_len) else {
        abort(b"[E0800] canonical MIR MapRoot.contains key length is invalid\0");
    };
    // SAFETY: the ABI precondition requires this exact key range to be
    // readable for the call; null and negative lengths were rejected above.
    let key_bytes = unsafe { std::slice::from_raw_parts(key, key_len) };
    let Ok(key) = std::str::from_utf8(key_bytes) else {
        abort(b"[E0800] canonical MIR MapRoot.contains key is not valid UTF-8\0");
    };
    if key.contains('\0') {
        abort(b"[E0800] canonical MIR MapRoot.contains key contains NUL\0");
    }
    super::handle::map_root_contains(handle, key)
        .map(i32::from)
        .unwrap_or_else(|_| abort(b"[E0800] canonical MIR MapRoot handle is busy or not live\0"))
}

/// Return the checked i32 size of one live MapRoot.
#[no_mangle]
pub extern "C" fn mimi_mir_map_root_size(handle: i64) -> i32 {
    if handle == 0 {
        abort(b"[E0800] canonical MIR MapRoot.size received an invalid handle\0");
    }
    checked_size_i32_or_abort(live_map_size(handle) as u128)
}

fn live_map_size(handle: i64) -> i64 {
    i64::try_from(
        super::handle::map_root_size(handle).unwrap_or_else(|_| {
            abort(b"[E0800] canonical MIR MapRoot handle is busy or not live\0")
        }),
    )
    .unwrap_or_else(|_| abort(b"[E0802] canonical MIR MapRoot size exceeds i64\0"))
}

/// Consume and reclaim one live MapRoot. Duplicate/stale drops fail closed.
#[no_mangle]
pub extern "C" fn mimi_mir_map_root_drop(handle: i64) {
    if handle == 0 {
        abort(b"[E0800] canonical MIR MapRoot.drop received an invalid handle\0");
    }
    super::handle::map_root_drop(handle)
        .unwrap_or_else(|_| abort(b"[E0800] canonical MIR MapRoot handle is busy or not live\0"));
}

/// Test-runtime probe used to prove that emitted MapRootDrop reaches the
/// production runtime table. It is absent from non-test runtime archives.
#[cfg(any(test, mimi_test_ub_symbols))]
#[no_mangle]
pub extern "C" fn mimi_test_map_root_live_count() -> i64 {
    super::handle::mimi_test_map_live_count()
}

/// Test-runtime entry into the exact checked-size trap branch used by
/// `mimi_mir_map_root_size`, without allocating billions of keys.
#[cfg(mimi_test_ub_symbols)]
#[no_mangle]
pub extern "C" fn mimi_test_map_root_size_from_len(len: i64) -> i32 {
    if len < 0 {
        abort(b"[E0800] canonical MIR MapRoot test size received a negative length\0");
    }
    checked_size_i32_or_abort(len as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_size_i32_preserves_the_e0802_boundary() {
        assert_eq!(checked_size_i32(i32::MAX as u128), Ok(i32::MAX));
        assert_eq!(
            checked_size_i32(i32::MAX as u128 + 1),
            Err("E0802: canonical MIR MapRoot.size result overflows i32")
        );
    }

    #[test]
    fn runtime_map_root_set_overwrite_unicode_and_drop() {
        let root = mimi_mir_map_root_new();
        assert!(super::super::handle::map_generation(root).is_ok());
        let answer = b"answer";
        let snow = "雪".as_bytes();
        // SAFETY: the byte slices stay live and immutable for each call; this
        // test uses the live root returned by the matching constructor.
        let root = unsafe { mimi_mir_map_root_set(root, answer.as_ptr(), answer.len() as i64, -7) };
        // SAFETY: same live root and key slice preconditions as above.
        let root = unsafe { mimi_mir_map_root_set(root, answer.as_ptr(), answer.len() as i64, 42) };
        // SAFETY: same live root and key slice preconditions as above.
        let root = unsafe { mimi_mir_map_root_set(root, snow.as_ptr(), snow.len() as i64, 9) };
        assert_eq!(mimi_mir_map_root_size(root), 2);
        let answer_key = std::ffi::CString::new("answer").expect("static key has no NUL");
        let snow_key = std::ffi::CString::new("雪").expect("Unicode key has no NUL");
        // SAFETY: both keys are live C strings and `root` is still live.
        assert_eq!(
            unsafe { super::super::mimi_map_get(root, answer_key.as_ptr()) },
            42,
            "MapRoot Set must persist the checked scalar in production MimiMap storage"
        );
        // SAFETY: same live Map and C-string conditions as above.
        assert_eq!(
            unsafe { super::super::mimi_map_get(root, snow_key.as_ptr()) },
            9
        );
        mimi_mir_map_root_drop(root);
        assert!(super::super::handle::map_generation(root).is_err());
    }

    #[test]
    fn runtime_map_root_contains_is_payload_blind_and_non_consuming() {
        let scalar_root = mimi_mir_map_root_new();
        let scalar_key = b"scalar";
        let missing_key = b"missing";
        // SAFETY: the key bytes remain readable and the root is live.
        let scalar_root = unsafe {
            mimi_mir_map_root_set(
                scalar_root,
                scalar_key.as_ptr(),
                scalar_key.len() as i64,
                73,
            )
        };
        // SAFETY: each exact key range is readable and scalar_root remains live.
        assert_eq!(
            unsafe {
                mimi_mir_map_root_contains(
                    scalar_root,
                    scalar_key.as_ptr(),
                    scalar_key.len() as i64,
                )
            },
            1
        );
        // SAFETY: same live root and readable missing-key range.
        assert_eq!(
            unsafe {
                mimi_mir_map_root_contains(
                    scalar_root,
                    missing_key.as_ptr(),
                    missing_key.len() as i64,
                )
            },
            0
        );
        // SAFETY: contains is observational; repeated queries leave the root live.
        assert_eq!(
            unsafe {
                mimi_mir_map_root_contains(
                    scalar_root,
                    scalar_key.as_ptr(),
                    scalar_key.len() as i64,
                )
            },
            1
        );
        assert_eq!(mimi_mir_map_root_size(scalar_root), 1);
        mimi_mir_map_root_drop(scalar_root);

        let string_root = mimi_mir_map_root_new();
        let string_key = b"string";
        let payload = b"opaque payload";
        // SAFETY: root and exact key/value byte ranges are valid for this call.
        let string_root = unsafe {
            mimi_mir_map_root_set_string(
                string_root,
                string_key.as_ptr(),
                string_key.len() as i64,
                payload.as_ptr().cast(),
                payload.len() as i64,
            )
        };
        // SAFETY: both key ranges remain readable and string_root remains live.
        assert_eq!(
            unsafe {
                mimi_mir_map_root_contains(
                    string_root,
                    string_key.as_ptr(),
                    string_key.len() as i64,
                )
            },
            1
        );
        // SAFETY: same live root and readable missing-key range.
        assert_eq!(
            unsafe {
                mimi_mir_map_root_contains(
                    string_root,
                    missing_key.as_ptr(),
                    missing_key.len() as i64,
                )
            },
            0
        );
        // SAFETY: repeated membership checks do not consume or inspect payloads.
        assert_eq!(
            unsafe {
                mimi_mir_map_root_contains(
                    string_root,
                    string_key.as_ptr(),
                    string_key.len() as i64,
                )
            },
            1
        );
        assert_eq!(mimi_mir_map_root_size(string_root), 1);
        mimi_mir_map_root_drop(string_root);
    }

    #[test]
    fn owned_string_payload_survives_shallow_clone_until_last_map_owner() {
        let root = mimi_mir_map_root_new();
        let key = b"payload";
        let value = "a\0b雪".as_bytes();
        // SAFETY: the root is live and both byte slices remain readable for
        // the exact explicit lengths passed to the String Set helper.
        let root = unsafe {
            mimi_mir_map_root_set_string(
                root,
                key.as_ptr(),
                key.len() as i64,
                value.as_ptr().cast(),
                value.len() as i64,
            )
        };
        let c_key = std::ffi::CString::new("payload").expect("static key has no NUL");
        // SAFETY: root is a live Map handle and c_key is a live C string.
        let tagged = unsafe { super::super::mimi_map_get(root, c_key.as_ptr()) };
        let address = (tagged as usize) & !1;
        assert_ne!(
            tagged & 1,
            0,
            "String payload must carry its explicit Any tag"
        );

        // SAFETY: root is live; mimi_map_clone retains the owner Arc for the
        // shallow clone and returns an independent live handle.
        let clone = unsafe { super::super::mimi_map_clone(root) };
        assert_ne!(clone, 0);
        let registered = || {
            super::super::any_value_strings()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key(&address)
        };
        assert!(registered());
        assert_eq!(
            super::super::copy_registered_any_string(tagged),
            Some(value.to_vec())
        );

        mimi_mir_map_root_drop(root);
        assert!(registered(), "the clone must keep the String payload alive");
        // SAFETY: clone remains live after its source root is destroyed.
        let cloned_tagged = unsafe { super::super::mimi_map_get(clone, c_key.as_ptr()) };
        assert_eq!(cloned_tagged, tagged);
        assert_eq!(
            super::super::copy_registered_any_string(cloned_tagged),
            Some(value.to_vec())
        );

        // SAFETY: clone is the final remaining owner and is destroyed once.
        unsafe { super::super::mimi_map_destroy(clone) };
        assert!(
            !registered(),
            "the final clone drop must free the payload once"
        );
    }

    #[test]
    fn remove_reclaims_string_only_after_last_shallow_map_owner() {
        let root = mimi_mir_map_root_new();
        let key = b"payload";
        let value = "a\0b雪".as_bytes();
        // SAFETY: the root is live and both byte slices remain readable for
        // the exact explicit lengths passed to the String Set helper.
        let root = unsafe {
            mimi_mir_map_root_set_string(
                root,
                key.as_ptr(),
                key.len() as i64,
                value.as_ptr().cast(),
                value.len() as i64,
            )
        };
        let c_key = std::ffi::CString::new("payload").expect("static key has no NUL");
        // SAFETY: root is live and c_key is a live C string.
        let tagged = unsafe { super::super::mimi_map_get(root, c_key.as_ptr()) };
        let address = (tagged as usize) & !1;
        // SAFETY: root is live; mimi_map_clone retains the owner Arc.
        let clone = unsafe { super::super::mimi_map_clone(root) };
        let registered = || {
            super::super::any_value_strings()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key(&address)
        };
        assert!(registered());

        // SAFETY: both handles are live and the static key range is readable.
        assert_eq!(
            unsafe { mimi_mir_map_root_remove(root, key.as_ptr(), key.len() as i64) },
            root
        );
        assert!(registered(), "the shallow clone still owns the payload");
        // SAFETY: clone and c_key are live; the entry remains in the clone.
        let cloned_tagged = unsafe { super::super::mimi_map_get(clone, c_key.as_ptr()) };
        assert_eq!(cloned_tagged, tagged);
        assert_eq!(
            super::super::copy_registered_any_string(cloned_tagged),
            Some(value.to_vec())
        );

        // SAFETY: clone is live and the same exact key range remains readable.
        assert_eq!(
            unsafe { mimi_mir_map_root_remove(clone, key.as_ptr(), key.len() as i64) },
            clone
        );
        assert!(!registered(), "removing the final owner frees the payload");
        mimi_mir_map_root_drop(root);
        mimi_mir_map_root_drop(clone);
    }

    #[test]
    fn remove_keeps_payload_until_last_key_in_one_map_is_removed() {
        let root = mimi_mir_map_root_new();
        let key = b"first";
        let value = b"shared";
        // SAFETY: root and both explicit byte ranges stay live for each call.
        let root = unsafe {
            mimi_mir_map_root_set_string(
                root,
                key.as_ptr(),
                key.len() as i64,
                value.as_ptr().cast(),
                value.len() as i64,
            )
        };
        let c_key = std::ffi::CString::new("first").expect("static key");
        // SAFETY: root and key are live.
        let tagged = unsafe { super::super::mimi_map_get(root, c_key.as_ptr()) };
        let address = (tagged as usize) & !1;
        let registered = || {
            super::super::any_value_strings()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key(&address)
        };
        assert!(registered());

        // This low-level duplicate models a shared opaque value handle held
        // by two entries in the same table. Production MapRoot Set clones a
        // fresh String per call, but the runtime must still retain the owner
        // until the table's final reference disappears.
        super::super::handle::map_root_set(root, "second".into(), tagged)
            .expect("insert duplicate tagged handle for the runtime invariant");
        // SAFETY: root and the exact key range are live.
        assert_eq!(
            unsafe { mimi_mir_map_root_remove(root, key.as_ptr(), key.len() as i64) },
            root
        );
        assert!(registered(), "the second key still names the payload");
        assert_eq!(
            super::super::copy_registered_any_string(tagged),
            Some(value.to_vec())
        );

        let last_key = b"second";
        // SAFETY: root and the exact final key range are live.
        assert_eq!(
            unsafe { mimi_mir_map_root_remove(root, last_key.as_ptr(), last_key.len() as i64) },
            root
        );
        assert!(!registered(), "the final key removal releases the payload");
        mimi_mir_map_root_drop(root);
    }
}
