//! Runtime support for the closed Canonical MIR MapRoot profile.
//!
//! This is deliberately separate from the public legacy Map ABI. MapRoot can
//! retain only static UTF-8 keys and i32 values, and its Checker receipt
//! prevents handle escape or aliasing. The runtime table owns each root until
//! the dedicated MapRootDrop operation removes it.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

static NEXT_MAP_ROOT_HANDLE: AtomicI64 = AtomicI64::new(1);
static MAP_ROOTS: OnceLock<Mutex<HashMap<i64, BTreeSet<String>>>> = OnceLock::new();

fn roots() -> &'static Mutex<HashMap<i64, BTreeSet<String>>> {
    MAP_ROOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

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

/// Allocate one empty root for the dedicated Canonical MIR MapRoot ABI.
#[no_mangle]
pub extern "C" fn mimi_mir_map_root_new() -> i64 {
    let handle = NEXT_MAP_ROOT_HANDLE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .unwrap_or_else(|_| abort(b"[E0800] canonical MIR MapRoot handle space exhausted\0"));
    let previous = roots()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(handle, BTreeSet::new());
    if previous.is_some() {
        abort(b"[E0800] canonical MIR MapRoot handle was reused\0");
    }
    handle
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
    _value: i32,
) -> i64 {
    if handle <= 0 || key.is_null() || key_len < 0 {
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
    let mut roots = roots().lock().unwrap_or_else(|error| error.into_inner());
    let Some(root) = roots.get_mut(&handle) else {
        abort(b"[E0800] canonical MIR MapRoot.set handle is not live\0");
    };
    root.insert(key.to_owned());
    handle
}

/// Return the checked i32 size of one live MapRoot.
#[no_mangle]
pub extern "C" fn mimi_mir_map_root_size(handle: i64) -> i32 {
    if handle <= 0 {
        abort(b"[E0800] canonical MIR MapRoot.size received an invalid handle\0");
    }
    let roots = roots().lock().unwrap_or_else(|error| error.into_inner());
    let Some(root) = roots.get(&handle) else {
        abort(b"[E0800] canonical MIR MapRoot.size handle is not live\0");
    };
    checked_size_i32_or_abort(root.len() as u128)
}

/// Consume and reclaim one live MapRoot. Duplicate/stale drops fail closed.
#[no_mangle]
pub extern "C" fn mimi_mir_map_root_drop(handle: i64) {
    if handle <= 0 {
        abort(b"[E0800] canonical MIR MapRoot.drop received an invalid handle\0");
    }
    let removed = roots()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&handle);
    if removed.is_none() {
        abort(b"[E0800] canonical MIR MapRoot.drop handle is not live\0");
    }
}

/// Test-runtime probe used to prove that emitted MapRootDrop reaches the
/// production runtime table. It is absent from non-test runtime archives.
#[cfg(mimi_test_ub_symbols)]
#[no_mangle]
pub extern "C" fn mimi_test_map_root_live_count() -> i64 {
    i64::try_from(
        roots()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
    )
    .unwrap_or_else(|_| abort(b"[E0802] canonical MIR MapRoot test count overflows i64\0"))
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
        mimi_mir_map_root_drop(root);
        assert!(!roots()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&root));
    }
}
