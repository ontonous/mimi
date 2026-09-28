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
}
