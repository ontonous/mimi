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

/// Failure details for the closed MapAnyOwner v1 runtime operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapAnyOwnerRuntimeError {
    InvalidInput,
    Capacity,
    KindMismatch,
    Handle(super::handle::HandleError),
    Owner(super::any_owner::AnyOwnerError),
}

impl From<super::handle::HandleError> for MapAnyOwnerRuntimeError {
    fn from(error: super::handle::HandleError) -> Self {
        Self::Handle(error)
    }
}

impl From<super::any_owner::AnyOwnerError> for MapAnyOwnerRuntimeError {
    fn from(error: super::any_owner::AnyOwnerError) -> Self {
        Self::Owner(error)
    }
}

fn checked_map_any_owner_len(len: i64) -> Result<usize, MapAnyOwnerRuntimeError> {
    if len < 0 {
        return Err(MapAnyOwnerRuntimeError::InvalidInput);
    }
    let len = usize::try_from(len).map_err(|_| MapAnyOwnerRuntimeError::InvalidInput)?;
    // `slice::from_raw_parts` requires every byte of the slice to fit in one
    // allocation no larger than `isize::MAX`. Do not impose a semantic size
    // limit here: Checker, reference, and bytecode admit every valid String.
    if len > isize::MAX as usize {
        return Err(MapAnyOwnerRuntimeError::InvalidInput);
    }
    Ok(len)
}

fn copy_map_any_owner_key(bytes: &[u8]) -> Result<String, MapAnyOwnerRuntimeError> {
    let source = std::str::from_utf8(bytes).map_err(|_| MapAnyOwnerRuntimeError::InvalidInput)?;
    if source.contains('\0') {
        return Err(MapAnyOwnerRuntimeError::InvalidInput);
    }
    let mut key = String::new();
    key.try_reserve_exact(source.len())
        .map_err(|_| MapAnyOwnerRuntimeError::Capacity)?;
    key.push_str(source);
    Ok(key)
}

struct PendingAnyOwner(Option<super::any_owner::AnyOwnerToken>);

impl Drop for PendingAnyOwner {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            // The token was allocated by this call and has not been committed
            // into a Map. Its owner-table release runs outside MAP_TABLE.
            if super::any_owner::release(token).is_err() {
                abort(b"[E0800] MapAnyOwner v1 rollback found an invalid owner token\0");
            }
        }
    }
}

fn insert_map_any_owner_entry(
    mut map: super::handle::MapLease,
    kind: super::any_owner::AnyOwnerKind,
    key: String,
    create_owner: impl FnOnce() -> Result<super::any_owner::AnyOwnerToken, MapAnyOwnerRuntimeError>,
) -> Result<(), MapAnyOwnerRuntimeError> {
    if map
        .any_owner_kind
        .is_some_and(|root_kind| root_kind != kind)
    {
        return Err(MapAnyOwnerRuntimeError::KindMismatch);
    }
    if !map.any_owner_entries.contains_key(&key) {
        map.any_owner_entries
            .try_reserve(1)
            .map_err(|_| MapAnyOwnerRuntimeError::Capacity)?;
    }
    let token = create_owner()?;
    let mut pending = PendingAnyOwner(Some(token));
    let replaced = map.any_owner_entries.insert(key, token);
    pending.0 = None;
    map.any_owner_kind = Some(kind);
    drop(map);
    if let Some(replaced) = replaced {
        super::any_owner::release(replaced)?;
    }
    Ok(())
}

fn map_any_owner_v1_set_i32(
    map: super::handle::MapLease,
    key_bytes: &[u8],
    value: i32,
) -> Result<(), MapAnyOwnerRuntimeError> {
    let key = copy_map_any_owner_key(key_bytes)?;
    insert_map_any_owner_entry(map, super::any_owner::AnyOwnerKind::I32, key, || {
        super::any_owner::allocate_i32(value).map_err(Into::into)
    })
}

fn map_any_owner_v1_set_string(
    map: super::handle::MapLease,
    key_bytes: &[u8],
    value_bytes: &[u8],
) -> Result<(), MapAnyOwnerRuntimeError> {
    let key = copy_map_any_owner_key(key_bytes)?;
    let value =
        std::str::from_utf8(value_bytes).map_err(|_| MapAnyOwnerRuntimeError::InvalidInput)?;
    insert_map_any_owner_entry(
        map,
        super::any_owner::AnyOwnerKind::OwnedString,
        key,
        || super::any_owner::allocate_string(value).map_err(Into::into),
    )
}

fn map_any_owner_v1_remove(
    mut map: super::handle::MapLease,
    key_bytes: &[u8],
) -> Result<(), MapAnyOwnerRuntimeError> {
    let key = copy_map_any_owner_key(key_bytes)?;
    let removed = map.any_owner_entries.remove(&key);
    drop(map);
    if let Some(token) = removed {
        super::any_owner::release(token)?;
    }
    Ok(())
}

/// Create a canonical MapAnyOwner v1 root. Its entries are stored only as
/// typed owner tokens in the versioned runtime sidecar.
#[no_mangle]
pub extern "C" fn mimi_mir_map_any_owner_v1_new() -> i64 {
    super::handle::map_new_handle(super::MimiMap::canonical_any_owner_v1())
}

/// Copy one i32 into a generation-checked Any owner and commit it at `key`.
/// The entry is unchanged if validation, reservation, or owner allocation
/// fails; successful overwrite releases the old token after dropping the Map
/// lease.
///
/// # Safety
/// `key` must reference `key_len` readable bytes for this call. It must be
/// UTF-8 and contain no NUL. The handle must be a live MapAnyOwner v1 root.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_map_any_owner_v1_set_i32(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: i32,
) -> i64 {
    if handle == 0 || key.is_null() {
        abort(b"[E0800] MapAnyOwner v1 Set received an invalid handle or key\0");
    }
    let key_len = checked_map_any_owner_len(key_len)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 Set key length is invalid\0"));
    let map = super::handle::map_any_owner_acquire(handle)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 handle is busy or not live\0"));
    // SAFETY: the ABI contract requires the checked key range to remain
    // readable for this call; the pointer is non-null, including for length 0.
    let key_bytes = unsafe { std::slice::from_raw_parts(key, key_len) };
    map_any_owner_v1_set_i32(map, key_bytes, value)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 Set could not commit the i32 owner\0"));
    handle
}

/// Copy one Mimi String into a generation-checked Any owner and commit it at
/// `key`. The copied String preserves its exact UTF-8 bytes, including NUL.
///
/// # Safety
/// For positive lengths, `key` and `value` must point to readable byte ranges
/// of the specified lengths. The key must be UTF-8 and NUL-free. A null value
/// pointer is accepted only for an empty String. The handle must be a live
/// MapAnyOwner v1 root.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_map_any_owner_v1_set_string(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: *const std::ffi::c_char,
    value_len: i64,
) -> i64 {
    if handle == 0 || key.is_null() || (value.is_null() && value_len != 0) {
        abort(b"[E0800] MapAnyOwner v1 String Set received an invalid operand\0");
    }
    let key_len = checked_map_any_owner_len(key_len)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 Set key length is invalid\0"));
    let value_len = checked_map_any_owner_len(value_len)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 String length is invalid\0"));
    let map = super::handle::map_any_owner_acquire(handle)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 handle is busy or not live\0"));
    // SAFETY: the ABI contract requires these checked byte ranges to remain
    // readable for this call; a null value pointer is used only at length 0.
    let key_bytes = unsafe { std::slice::from_raw_parts(key, key_len) };
    let value_bytes = if value_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(value.cast::<u8>(), value_len) }
    };
    map_any_owner_v1_set_string(map, key_bytes, value_bytes).unwrap_or_else(|_| {
        abort(b"[E0800] MapAnyOwner v1 Set could not commit the String owner\0")
    });
    handle
}

/// Remove one entry and release its typed owner after the Map lease is gone.
/// Missing keys leave the root unchanged.
///
/// # Safety
/// `key` must reference `key_len` readable UTF-8 bytes without NUL, and the
/// handle must be a live MapAnyOwner v1 root.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_map_any_owner_v1_remove(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i64 {
    if handle == 0 || key.is_null() {
        abort(b"[E0800] MapAnyOwner v1 Remove received an invalid handle or key\0");
    }
    let key_len = checked_map_any_owner_len(key_len)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 Remove key length is invalid\0"));
    let map = super::handle::map_any_owner_acquire(handle)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 handle is busy or not live\0"));
    // SAFETY: the ABI contract requires this checked key range to remain
    // readable for this call; the pointer is non-null, including at length 0.
    let key_bytes = unsafe { std::slice::from_raw_parts(key, key_len) };
    map_any_owner_v1_remove(map, key_bytes)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 Remove could not release its owner\0"));
    handle
}

/// Return whether the live MapAnyOwner v1 root contains `key`.
///
/// # Safety
/// `key` must reference `key_len` readable UTF-8 bytes without NUL, and the
/// handle must be a live MapAnyOwner v1 root.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_map_any_owner_v1_contains(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i32 {
    if handle == 0 || key.is_null() {
        abort(b"[E0800] MapAnyOwner v1 Contains received an invalid handle or key\0");
    }
    let key_len = checked_map_any_owner_len(key_len)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 Contains key length is invalid\0"));
    let map = super::handle::map_any_owner_acquire(handle)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 handle is busy or not live\0"));
    // SAFETY: the ABI contract requires this checked key range to remain
    // readable for this call; the pointer is non-null, including at length 0.
    let key_bytes = unsafe { std::slice::from_raw_parts(key, key_len) };
    let key = copy_map_any_owner_key(key_bytes)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 Contains key is invalid\0"));
    i32::from(map.any_owner_entries.contains_key(&key))
}

/// Return the checked i32 entry count for a live MapAnyOwner v1 root.
#[no_mangle]
pub extern "C" fn mimi_mir_map_any_owner_v1_size(handle: i64) -> i32 {
    if handle == 0 {
        abort(b"[E0800] MapAnyOwner v1 Size received an invalid handle\0");
    }
    let map = super::handle::map_any_owner_acquire(handle)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 handle is busy or not live\0"));
    i32::try_from(map.any_owner_entries.len())
        .unwrap_or_else(|_| abort(b"[E0802] MapAnyOwner v1 size overflows i32\0"))
}

/// Consume a MapAnyOwner v1 root. The handle table retires the map identity
/// before any typed payload owner is released.
#[no_mangle]
pub extern "C" fn mimi_mir_map_any_owner_v1_drop(handle: i64) {
    if handle == 0 {
        abort(b"[E0800] MapAnyOwner v1 Drop received an invalid handle\0");
    }
    let mut retired_map = super::handle::map_any_owner_drop(handle)
        .unwrap_or_else(|_| abort(b"[E0800] MapAnyOwner v1 handle is busy or not live\0"));
    let owners = std::mem::take(&mut retired_map.any_owner_entries);
    drop(retired_map);
    for token in owners.into_values() {
        super::any_owner::release(token).unwrap_or_else(|_| {
            abort(b"[E0800] MapAnyOwner v1 Drop found an invalid owner token\0")
        });
    }
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

    static MAP_ANY_OWNER_RUNTIME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn map_any_owner_v1_lengths_have_no_backend_only_size_cap() {
        let larger_than_old_policy_cap = 64 * 1024 * 1024 + 1;
        assert_eq!(
            checked_map_any_owner_len(larger_than_old_policy_cap),
            Ok(larger_than_old_policy_cap as usize)
        );
        assert_eq!(
            checked_map_any_owner_len(-1),
            Err(MapAnyOwnerRuntimeError::InvalidInput)
        );
        assert_eq!(
            checked_map_any_owner_len(isize::MAX as i64),
            Ok(isize::MAX as usize)
        );
        if isize::BITS < i64::BITS {
            assert_eq!(
                checked_map_any_owner_len(isize::MAX as i64 + 1),
                Err(MapAnyOwnerRuntimeError::InvalidInput)
            );
        }
    }

    #[test]
    fn checked_size_i32_preserves_the_e0802_boundary() {
        assert_eq!(checked_size_i32(i32::MAX as u128), Ok(i32::MAX));
        assert_eq!(
            checked_size_i32(i32::MAX as u128 + 1),
            Err("E0802: canonical MIR MapRoot.size result overflows i32")
        );
    }

    #[test]
    fn map_any_owner_v1_keeps_tokens_separate_and_releases_them_on_mutation() {
        let _owner_registry_guard = MAP_ANY_OWNER_RUNTIME_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Keep one slot live so destroy advances the typed handle generation
        // without compacting the otherwise-empty Map table.
        let table_guard = super::super::mimi_map_new();
        let map = mimi_mir_map_any_owner_v1_new();
        assert_ne!(map, 0);
        assert_eq!(mimi_mir_map_any_owner_v1_size(map), 0);

        let number_key = b"number";
        // SAFETY: this test passes valid byte ranges and a fresh live typed Map.
        assert_eq!(
            unsafe {
                mimi_mir_map_any_owner_v1_set_i32(
                    map,
                    number_key.as_ptr(),
                    number_key.len() as i64,
                    -37,
                )
            },
            map
        );
        let number_token = {
            let map_lease = super::super::handle::map_any_owner_acquire(map).unwrap();
            *map_lease
                .any_owner_entries
                .get("number")
                .expect("typed entry was committed")
        };
        assert_eq!(super::super::any_owner::read_i32(number_token), Ok(-37));

        // SAFETY: `set_i32` is called with a valid static key and live typed
        // Map. Overwrite must retire its previous owner token.
        unsafe {
            mimi_mir_map_any_owner_v1_set_i32(map, number_key.as_ptr(), number_key.len() as i64, 91)
        };
        assert_eq!(
            super::super::any_owner::read_i32(number_token),
            Err(super::super::any_owner::AnyOwnerError::StaleToken)
        );
        let number_token = {
            let map_lease = super::super::handle::map_any_owner_acquire(map).unwrap();
            *map_lease
                .any_owner_entries
                .get("number")
                .expect("overwritten entry remains present")
        };
        assert_eq!(super::super::any_owner::read_i32(number_token), Ok(91));

        assert_eq!(mimi_mir_map_any_owner_v1_size(map), 1);

        mimi_mir_map_any_owner_v1_drop(map);
        assert_eq!(
            super::super::any_owner::read_i32(number_token),
            Err(super::super::any_owner::AnyOwnerError::StaleToken)
        );
        assert!(matches!(
            super::super::handle::map_any_owner_acquire(map),
            Err(super::super::handle::HandleError::StaleGeneration)
        ));
        assert!(matches!(
            super::super::handle::map_any_owner_drop(map),
            Err(super::super::handle::HandleError::StaleGeneration)
        ));

        // Strings use their own homogeneous root, with embedded NUL bytes
        // preserved in the owned payload and released by Remove.
        let string_map = mimi_mir_map_any_owner_v1_new();
        let string_key = b"nul-string";
        let string_value = b"left\0right \xe9\x9b\xaa";
        // SAFETY: both byte ranges are live for the call; the value is valid
        // UTF-8 with an embedded NUL, and the runtime copies it into an owner.
        unsafe {
            mimi_mir_map_any_owner_v1_set_string(
                string_map,
                string_key.as_ptr(),
                string_key.len() as i64,
                string_value.as_ptr().cast(),
                string_value.len() as i64,
            )
        };
        let string_token = {
            let map_lease = super::super::handle::map_any_owner_acquire(string_map).unwrap();
            *map_lease
                .any_owner_entries
                .get("nul-string")
                .expect("typed String entry was committed")
        };
        assert_eq!(
            super::super::any_owner::copy_string(string_token).as_deref(),
            Ok("left\0right 雪")
        );
        assert_eq!(mimi_mir_map_any_owner_v1_size(string_map), 1);
        // SAFETY: `string_key` is a valid live UTF-8 key slice.
        assert_eq!(
            unsafe {
                mimi_mir_map_any_owner_v1_contains(
                    string_map,
                    string_key.as_ptr(),
                    string_key.len() as i64,
                )
            },
            1
        );
        // SAFETY: same live UTF-8 key slice and Map preconditions as above.
        assert_eq!(
            unsafe {
                mimi_mir_map_any_owner_v1_remove(
                    string_map,
                    string_key.as_ptr(),
                    string_key.len() as i64,
                )
            },
            string_map
        );
        assert_eq!(
            super::super::any_owner::copy_string(string_token),
            Err(super::super::any_owner::AnyOwnerError::StaleToken)
        );
        assert_eq!(mimi_mir_map_any_owner_v1_size(string_map), 0);
        mimi_mir_map_any_owner_v1_drop(string_map);

        unsafe { super::super::mimi_map_destroy(table_guard) };
    }

    #[test]
    fn map_any_owner_v1_pins_one_payload_kind_for_the_root_lifetime() {
        let _owner_registry_guard = MAP_ANY_OWNER_RUNTIME_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let baseline = super::super::any_owner::live_count().expect("owner table is live");
        let map = mimi_mir_map_any_owner_v1_new();
        let key = b"entry";
        let other_key = b"other";

        let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
        map_any_owner_v1_set_i32(lease, key, 17).expect("first Set pins i32");
        assert_eq!(
            super::super::any_owner::live_count(),
            Ok(baseline + 1),
            "one owner backs the committed entry"
        );
        let original_i32 = {
            let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
            assert_eq!(
                lease.any_owner_kind,
                Some(super::super::any_owner::AnyOwnerKind::I32)
            );
            *lease.any_owner_entries.get("entry").unwrap()
        };

        let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
        assert_eq!(
            map_any_owner_v1_set_string(lease, key, b"must not overwrite"),
            Err(MapAnyOwnerRuntimeError::KindMismatch),
            "cross-kind overwrite is rejected"
        );
        let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
        assert_eq!(
            map_any_owner_v1_set_string(lease, other_key, b"must not insert"),
            Err(MapAnyOwnerRuntimeError::KindMismatch),
            "cross-kind insertion is rejected"
        );
        {
            let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
            assert_eq!(lease.any_owner_entries.len(), 1);
            assert_eq!(lease.any_owner_entries.get("entry"), Some(&original_i32));
        }
        assert_eq!(super::super::any_owner::read_i32(original_i32), Ok(17));
        assert_eq!(super::super::any_owner::live_count(), Ok(baseline + 1));

        let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
        map_any_owner_v1_remove(lease, key).expect("remove releases the entry owner");
        assert_eq!(mimi_mir_map_any_owner_v1_size(map), 0);
        assert_eq!(
            super::super::any_owner::read_i32(original_i32),
            Err(super::super::any_owner::AnyOwnerError::StaleToken)
        );
        assert_eq!(super::super::any_owner::live_count(), Ok(baseline));
        {
            let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
            assert_eq!(
                lease.any_owner_kind,
                Some(super::super::any_owner::AnyOwnerKind::I32)
            );
        }
        let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
        assert_eq!(
            map_any_owner_v1_set_string(lease, other_key, b"still rejected"),
            Err(MapAnyOwnerRuntimeError::KindMismatch),
            "Remove does not clear the root's pinned kind"
        );
        assert_eq!(mimi_mir_map_any_owner_v1_size(map), 0);
        assert_eq!(super::super::any_owner::live_count(), Ok(baseline));

        let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
        map_any_owner_v1_set_i32(lease, other_key, 23)
            .expect("the pinned kind remains usable after Remove");
        mimi_mir_map_any_owner_v1_drop(map);
        assert_eq!(super::super::any_owner::live_count(), Ok(baseline));

        let string_map = mimi_mir_map_any_owner_v1_new();
        let lease = super::super::handle::map_any_owner_acquire(string_map).unwrap();
        map_any_owner_v1_set_string(lease, key, b"owned string")
            .expect("a distinct root may pin String");
        let original_string = {
            let lease = super::super::handle::map_any_owner_acquire(string_map).unwrap();
            assert_eq!(
                lease.any_owner_kind,
                Some(super::super::any_owner::AnyOwnerKind::OwnedString)
            );
            *lease.any_owner_entries.get("entry").unwrap()
        };
        let lease = super::super::handle::map_any_owner_acquire(string_map).unwrap();
        assert_eq!(
            map_any_owner_v1_set_i32(lease, key, 31),
            Err(MapAnyOwnerRuntimeError::KindMismatch),
            "the reverse cross-kind overwrite is rejected"
        );
        assert_eq!(
            super::super::any_owner::copy_string(original_string).as_deref(),
            Ok("owned string")
        );
        assert_eq!(mimi_mir_map_any_owner_v1_size(string_map), 1);
        assert_eq!(super::super::any_owner::live_count(), Ok(baseline + 1));

        let lease = super::super::handle::map_any_owner_acquire(string_map).unwrap();
        map_any_owner_v1_remove(lease, key).expect("remove releases the String owner");
        let lease = super::super::handle::map_any_owner_acquire(string_map).unwrap();
        assert_eq!(
            map_any_owner_v1_set_i32(lease, other_key, 37),
            Err(MapAnyOwnerRuntimeError::KindMismatch),
            "Remove does not clear a String root's pinned kind"
        );
        assert_eq!(super::super::any_owner::live_count(), Ok(baseline));
        mimi_mir_map_any_owner_v1_drop(string_map);
        assert_eq!(super::super::any_owner::live_count(), Ok(baseline));
    }

    #[test]
    fn map_any_owner_v1_rejects_legacy_helpers_and_legacy_maps() {
        let typed = mimi_mir_map_any_owner_v1_new();
        assert!(matches!(
            super::super::handle::map_acquire(typed),
            Err(super::super::handle::HandleError::Invalid)
        ));
        assert!(matches!(
            super::super::handle::map_root_size(typed),
            Err(super::super::handle::HandleError::Invalid)
        ));
        assert_eq!(
            super::super::mimi_map_lease_acquire(typed),
            super::super::HANDLE_ERR_INVALID
        );
        assert_eq!(
            super::super::mimi_map_begin_destroy(typed),
            super::super::HANDLE_ERR_INVALID
        );
        // Generic destroy fails closed on typed roots; the versioned drop is
        // still able to consume the same live handle afterward.
        unsafe { super::super::mimi_map_destroy(typed) };
        assert_eq!(mimi_mir_map_any_owner_v1_size(typed), 0);
        mimi_mir_map_any_owner_v1_drop(typed);

        let legacy = super::super::mimi_map_new();
        assert!(matches!(
            super::super::handle::map_any_owner_acquire(legacy),
            Err(super::super::handle::HandleError::Invalid)
        ));
        assert!(matches!(
            super::super::handle::map_any_owner_drop(legacy),
            Err(super::super::handle::HandleError::Invalid)
        ));
        unsafe { super::super::mimi_map_destroy(legacy) };
    }

    #[test]
    fn map_any_owner_v1_invalid_key_and_string_leave_entries_unchanged() {
        let map = mimi_mir_map_any_owner_v1_new();
        let key_with_nul = b"bad\0key";
        let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
        assert_eq!(
            map_any_owner_v1_set_i32(lease, key_with_nul, 7),
            Err(MapAnyOwnerRuntimeError::InvalidInput)
        );
        assert_eq!(mimi_mir_map_any_owner_v1_size(map), 0);
        assert_eq!(
            super::super::handle::map_any_owner_acquire(map)
                .unwrap()
                .any_owner_kind,
            None,
            "a failed Set does not pin the root kind"
        );

        let invalid_utf8 = [0xff];
        let lease = super::super::handle::map_any_owner_acquire(map).unwrap();
        assert_eq!(
            map_any_owner_v1_set_string(lease, b"valid", &invalid_utf8),
            Err(MapAnyOwnerRuntimeError::InvalidInput)
        );
        assert_eq!(mimi_mir_map_any_owner_v1_size(map), 0);
        assert_eq!(
            super::super::handle::map_any_owner_acquire(map)
                .unwrap()
                .any_owner_kind,
            None,
            "invalid String bytes do not pin the root kind"
        );
        mimi_mir_map_any_owner_v1_drop(map);
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
