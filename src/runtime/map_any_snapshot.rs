//! Persistent ordinary-map snapshots whose entries own typed `Any` values.
//!
//! This is the runtime layer for the distinct
//! `ordinary-map-owner-lifecycle-i32-string-v1` profile.
//! It is intentionally independent of the MapAnyOwner v1 root API: the map
//! stores `AnyOwnerToken`s only inside non-copyable RAII values and never
//! encodes them as `ValueHandle`s.
//!
//! Root-producing ABIs return zero on failure; Size and Contains return -1.
//! This lets the native consumer clean up every live root before its trap.
//! Rust staging locals are dropped before an error sentinel is returned.

use std::collections::HashMap;

#[cfg(test)]
use std::cell::{Cell, RefCell};

use super::any_owner::{self, AnyOwnerError, AnyOwnerKind, AnyOwnerReceiptError, AnyOwnerToken};

/// A single owned `Any` value. The token is released exactly once on drop.
///
/// This wrapper deliberately does not implement `Clone` or `Copy`; callers
/// must use `try_clone` so owner-table allocation failures remain observable.
pub(super) struct OwnedAny {
    token: AnyOwnerToken,
}

impl OwnedAny {
    fn from_i32(value: i32) -> Result<Self, AnyOwnerError> {
        snapshot_allocation_checkpoint().map_err(|_| AnyOwnerError::Capacity)?;
        let token = any_owner::allocate_i32(value)?;
        record_created_token(token);
        Ok(Self { token })
    }

    fn from_string(value: &str) -> Result<Self, AnyOwnerError> {
        snapshot_allocation_checkpoint().map_err(|_| AnyOwnerError::Capacity)?;
        let token = any_owner::allocate_string(value)?;
        record_created_token(token);
        Ok(Self { token })
    }

    /// Read the exact descriptor sidecar without exposing token wire bits.
    pub(super) fn descriptor_identity(
        &self,
    ) -> Result<any_owner::DescriptorIdentity, AnyOwnerError> {
        any_owner::descriptor_identity(self.token)
    }

    /// Read an i32 through its typed descriptor check.
    pub(super) fn read_i32(&self) -> Result<i32, AnyOwnerError> {
        any_owner::read_i32(self.token)
    }

    /// Return a fallible owned String copy through its typed descriptor check.
    pub(super) fn copy_string(&self) -> Result<String, AnyOwnerError> {
        any_owner::copy_string(self.token)
    }

    /// Return a new independent owner for this value.
    pub(super) fn try_clone(&self) -> Result<Self, AnyOwnerError> {
        snapshot_allocation_checkpoint().map_err(|_| AnyOwnerError::Capacity)?;
        let token = any_owner::clone_owner(self.token)?;
        record_created_token(token);
        Ok(Self { token })
    }

    /// Transfer the fresh owner only after creator-domain registration succeeds.
    /// Every failure leaves this RAII guard responsible for the exact token.
    fn publish_managed(self) -> Result<i64, ManagedAnyStatus> {
        let descriptor =
            any_owner::descriptor_identity(self.token).map_err(ManagedAnyStatus::from)?;
        if descriptor.version != 1 {
            return Err(ManagedAnyStatus::VersionMismatch);
        }
        super::ordinary_map_domain::register_fresh_managed_any(self.token, descriptor)
            .map_err(ManagedAnyStatus::from)?;
        let accepted = std::mem::ManuallyDrop::new(self);
        Ok(accepted.token.wire_bits() as i64)
    }
}

impl Drop for OwnedAny {
    fn drop(&mut self) {
        // A live token is guaranteed by this private wrapper's constructors.
        // Failure means the owner registry violated its internal invariant;
        // continuing would silently leak or double-release the payload.
        if any_owner::release(self.token).is_err() && !super::ordinary_map_domain::in_trap_cleanup()
        {
            std::process::abort();
        }
        // During orderly trap drain, stale/poisoned releases are secondary
        // errors and cannot replace the original diagnostic or stop the rest
        // of the creator's Any/String domain from being settled.
    }
}

/// Errors returned while staging an ordinary map snapshot update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OrdinaryMapSnapshotError {
    Capacity,
    Owner(AnyOwnerError),
}

impl From<AnyOwnerError> for OrdinaryMapSnapshotError {
    fn from(error: AnyOwnerError) -> Self {
        match error {
            AnyOwnerError::Capacity => Self::Capacity,
            other => Self::Owner(other),
        }
    }
}

/// An immutable root containing heterogeneous owned `i32` and `String` values.
///
/// Keys are Rust UTF-8 strings, hashed over their full byte contents. Embedded
/// NUL bytes are valid and remain part of the key.
pub(crate) struct OrdinaryMapSnapshot {
    entries: HashMap<String, OwnedAny>,
}

/// Typed lookup result for profile-specific ordinary Map getters.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OrdinaryMapLookup<T> {
    Missing,
    TypeMismatch,
    Value(T),
}

impl OrdinaryMapSnapshot {
    pub(super) fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Number of entries in this root.
    pub(super) fn size(&self) -> usize {
        self.entries.len()
    }

    /// Whether this root contains `key`.
    pub(super) fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Copy one `i32` into a new persistent root, leaving this root unchanged.
    pub(super) fn set_i32(&self, key: &str, value: i32) -> Result<Self, OrdinaryMapSnapshotError> {
        let owned_key = try_copy_string(key)?;
        let owned_value = OwnedAny::from_i32(value)?;
        self.with_entry(owned_key, owned_value)
    }

    /// Copy one UTF-8 string into a new persistent root, leaving this root
    /// unchanged. NUL bytes in either the key or value are preserved.
    pub(super) fn set_string(
        &self,
        key: &str,
        value: &str,
    ) -> Result<Self, OrdinaryMapSnapshotError> {
        let owned_key = try_copy_string(key)?;
        let owned_value = OwnedAny::from_string(value)?;
        self.with_entry(owned_key, owned_value)
    }

    /// Create a root with independent owner tokens for every entry.
    pub(super) fn clone_root(&self) -> Result<Self, OrdinaryMapSnapshotError> {
        let mut entries = HashMap::new();
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve(self.entries.len())
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;

        for (key, value) in &self.entries {
            let copied_key = try_copy_string(key)?;
            let copied_value = value.try_clone()?;
            entries.insert(copied_key, copied_value);
        }

        Ok(Self { entries })
    }

    /// Return an independent owner clone. Missing keys follow the ordinary
    /// Map result convention: `(false, Int(0))`, represented here by a valid
    /// freshly allocated i32-zero owner.
    pub(super) fn get(&self, key: &str) -> Result<(bool, OwnedAny), OrdinaryMapSnapshotError> {
        match self.entries.get(key) {
            Some(value) => Ok((true, value.try_clone()?)),
            None => Ok((false, OwnedAny::from_i32(0)?)),
        }
    }

    /// Snapshot all values in deterministic UTF-8 key-byte order. Each value
    /// owns a fresh token and survives every update or destruction of this root.
    pub(super) fn values(&self) -> Result<Vec<OwnedAny>, OrdinaryMapSnapshotError> {
        let ordered = self.sorted_entries()?;
        let mut values = Vec::new();
        snapshot_allocation_checkpoint()?;
        values
            .try_reserve_exact(ordered.len())
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        for (_, value) in ordered {
            values.push(value.try_clone()?);
        }
        Ok(values)
    }

    /// Snapshot copied keys and independently owned values in the same order
    /// as `values`. A failed stage drops every token already cloned.
    pub(super) fn to_list(&self) -> Result<Vec<(String, OwnedAny)>, OrdinaryMapSnapshotError> {
        let ordered = self.sorted_entries()?;
        let mut entries = Vec::new();
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve_exact(ordered.len())
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        for (key, value) in ordered {
            let key = try_copy_string(key)?;
            let value = value.try_clone()?;
            entries.push((key, value));
        }
        Ok(entries)
    }

    fn sorted_entries(&self) -> Result<Vec<(&String, &OwnedAny)>, OrdinaryMapSnapshotError> {
        let mut entries = Vec::new();
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve_exact(self.entries.len())
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        entries.extend(self.entries.iter());
        // Unstable sorting is allocation-free, and unique Map keys make its
        // stability irrelevant. Sorting full bytes preserves NUL and UTF-8.
        entries.sort_unstable_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
        Ok(entries)
    }

    /// Read an i32 entry without exposing its owner token or storage.
    pub(super) fn get_i32_typed(
        &self,
        key: &str,
    ) -> Result<OrdinaryMapLookup<i32>, OrdinaryMapSnapshotError> {
        let Some(value) = self.entries.get(key) else {
            return Ok(OrdinaryMapLookup::Missing);
        };
        match any_owner::validate(value.token)? {
            super::any_owner::AnyOwnerKind::I32 => {
                Ok(OrdinaryMapLookup::Value(any_owner::read_i32(value.token)?))
            }
            super::any_owner::AnyOwnerKind::OwnedString => Ok(OrdinaryMapLookup::TypeMismatch),
        }
    }

    /// Copy a String entry out of its owner. The returned Rust String is an
    /// intermediate only; the FFI getter copies it into Mimi's canonical
    /// `{ptr, len}` allocation domain before returning to generated code.
    pub(super) fn get_string_typed(
        &self,
        key: &str,
    ) -> Result<OrdinaryMapLookup<String>, OrdinaryMapSnapshotError> {
        let Some(value) = self.entries.get(key) else {
            return Ok(OrdinaryMapLookup::Missing);
        };
        match any_owner::validate(value.token)? {
            super::any_owner::AnyOwnerKind::I32 => Ok(OrdinaryMapLookup::TypeMismatch),
            super::any_owner::AnyOwnerKind::OwnedString => {
                #[cfg(test)]
                ordinary_map_owned_get_allocation_checkpoint_for_test()
                    .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
                Ok(OrdinaryMapLookup::Value(any_owner::copy_string(
                    value.token,
                )?))
            }
        }
    }

    /// Create a new persistent root without `key`. A missing key still
    /// produces an independent root, preserving value-style Map semantics.
    pub(super) fn remove(&self, key: &str) -> Result<Self, OrdinaryMapSnapshotError> {
        let removes_entry = self.entries.contains_key(key);
        let final_len = self
            .entries
            .len()
            .checked_sub(usize::from(removes_entry))
            .ok_or(OrdinaryMapSnapshotError::Capacity)?;
        let mut entries = HashMap::new();
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve(final_len)
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;

        for (old_key, old_value) in &self.entries {
            if old_key == key {
                continue;
            }
            let copied_key = try_copy_string(old_key)?;
            let copied_value = old_value.try_clone()?;
            entries.insert(copied_key, copied_value);
        }

        Ok(Self { entries })
    }

    pub(super) fn size_i32(&self) -> Result<i32, OrdinaryMapSnapshotError> {
        i32::try_from(self.entries.len()).map_err(|_| OrdinaryMapSnapshotError::Capacity)
    }

    fn with_entry(&self, key: String, value: OwnedAny) -> Result<Self, OrdinaryMapSnapshotError> {
        let replaces_existing = self.entries.contains_key(key.as_str());
        let final_len = self
            .entries
            .len()
            .checked_add(usize::from(!replaces_existing))
            .ok_or(OrdinaryMapSnapshotError::Capacity)?;

        // Reserve the complete final table before cloning any values. Every
        // subsequent insertion is then allocation-free; the source is never
        // modified and a staged failure drops all owners already cloned.
        let mut entries = HashMap::new();
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve(final_len)
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;

        for (old_key, old_value) in &self.entries {
            if old_key == key.as_str() {
                continue;
            }
            let copied_key = try_copy_string(old_key)?;
            let copied_value = old_value.try_clone()?;
            entries.insert(copied_key, copied_value);
        }

        entries.insert(key, value);
        Ok(Self { entries })
    }
}

fn try_copy_string(value: &str) -> Result<String, OrdinaryMapSnapshotError> {
    snapshot_allocation_checkpoint()?;
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
    copy.push_str(value);
    Ok(copy)
}

#[cfg(test)]
thread_local! {
    static SNAPSHOT_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static SNAPSHOT_TRACK_CREATED: Cell<bool> = const { Cell::new(false) };
    static SNAPSHOT_CREATED_TOKENS: RefCell<Vec<AnyOwnerToken>> = const { RefCell::new(Vec::new()) };
    static OWNED_GET_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_GET_ALLOCATION_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_GET_FAILURE_TRIGGERED: Cell<bool> = const { Cell::new(false) };
}

/// Test-only allocation oracle shared by the independent owned-Get consumers.
/// Typed getters count key, stored String copy, and canonical allocation.
/// Ordinary Any get counts key then owner clone; ManagedAny clone counts the
/// owner stage; ManagedAny String copy counts payload copy then canonical
/// allocation. Persistent Map construction and domain registration are separate.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_get_allocation_checkpoint_for_test() -> Result<(), ()> {
    OWNED_GET_FAIL_AFTER.with(|fail_after| match fail_after.get() {
        Some(remaining) => {
            OWNED_GET_ALLOCATION_ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
            if remaining == 0 {
                OWNED_GET_FAILURE_TRIGGERED.with(|triggered| triggered.set(true));
                Err(())
            } else {
                fail_after.set(Some(remaining - 1));
                Ok(())
            }
        }
        None => Ok(()),
    })
}

#[cfg(test)]
pub(crate) fn ordinary_map_owned_get_allocation_attempts_for_test() -> usize {
    OWNED_GET_ALLOCATION_ATTEMPTS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn ordinary_map_owned_get_allocation_failure_triggered_for_test() -> bool {
    OWNED_GET_FAILURE_TRIGGERED.with(Cell::get)
}

/// Scope a fault to the calling test thread and restore even after unwinding.
/// Native JIT execution must bind the real runtime helpers from this process
/// and run on this thread to observe the same oracle as reference/bytecode.
#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_get_allocation_failure_for_test<T>(
    stages_before_failure: usize,
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint {
        fail_after: Option<usize>,
        attempts: usize,
        triggered: bool,
    }
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_GET_FAIL_AFTER.with(|value| value.set(self.fail_after));
            OWNED_GET_ALLOCATION_ATTEMPTS.with(|value| value.set(self.attempts));
            OWNED_GET_FAILURE_TRIGGERED.with(|value| value.set(self.triggered));
        }
    }

    let _restore = RestoreFailpoint {
        fail_after: OWNED_GET_FAIL_AFTER.with(|value| value.replace(Some(stages_before_failure))),
        attempts: OWNED_GET_ALLOCATION_ATTEMPTS.with(|value| value.replace(0)),
        triggered: OWNED_GET_FAILURE_TRIGGERED.with(|value| value.replace(false)),
    };
    operation()
}

fn snapshot_allocation_checkpoint() -> Result<(), OrdinaryMapSnapshotError> {
    #[cfg(test)]
    {
        return SNAPSHOT_FAIL_AFTER.with(|fail_after| match fail_after.get() {
            Some(0) => Err(OrdinaryMapSnapshotError::Capacity),
            Some(remaining) => {
                fail_after.set(Some(remaining - 1));
                Ok(())
            }
            None => Ok(()),
        });
    }
    #[cfg(not(test))]
    Ok(())
}

fn record_created_token(token: AnyOwnerToken) {
    #[cfg(test)]
    SNAPSHOT_TRACK_CREATED.with(|track| {
        if track.get() {
            SNAPSHOT_CREATED_TOKENS.with(|tokens| tokens.borrow_mut().push(token));
        }
    });
    #[cfg(not(test))]
    let _ = token;
}

#[cfg(test)]
fn begin_failure_injection(fail_after: usize) {
    SNAPSHOT_CREATED_TOKENS.with(|tokens| tokens.borrow_mut().clear());
    SNAPSHOT_TRACK_CREATED.with(|track| track.set(true));
    SNAPSHOT_FAIL_AFTER.with(|failure| failure.set(Some(fail_after)));
}

#[cfg(test)]
fn end_failure_injection() -> Vec<AnyOwnerToken> {
    SNAPSHOT_FAIL_AFTER.with(|failure| failure.set(None));
    SNAPSHOT_TRACK_CREATED.with(|track| track.set(false));
    SNAPSHOT_CREATED_TOKENS.with(|tokens| std::mem::take(&mut *tokens.borrow_mut()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrdinaryMapSnapshotRuntimeError {
    InvalidInput,
    Capacity,
    SizeOverflow,
    Owner(AnyOwnerError),
    Handle(super::handle::HandleError),
}

impl From<OrdinaryMapSnapshotError> for OrdinaryMapSnapshotRuntimeError {
    fn from(error: OrdinaryMapSnapshotError) -> Self {
        match error {
            OrdinaryMapSnapshotError::Capacity => Self::Capacity,
            OrdinaryMapSnapshotError::Owner(error) => error.into(),
        }
    }
}

impl From<AnyOwnerError> for OrdinaryMapSnapshotRuntimeError {
    fn from(error: AnyOwnerError) -> Self {
        match error {
            AnyOwnerError::Capacity => Self::Capacity,
            other => Self::Owner(other),
        }
    }
}

impl From<super::handle::HandleError> for OrdinaryMapSnapshotRuntimeError {
    fn from(error: super::handle::HandleError) -> Self {
        Self::Handle(error)
    }
}

fn new_root(snapshot: OrdinaryMapSnapshot) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    snapshot_allocation_checkpoint()?;
    match super::handle::map_new_handle_fallible(
        super::MimiMap::ordinary_map_owner_lifecycle_i32_string_v1(snapshot),
    ) {
        Ok(handle) => super::ordinary_map_domain::publish_new_root(
            super::ordinary_map_domain::OrdinaryMapProfile::OwnerLifecycleI32StringV1,
            handle,
        )
        .map_err(|_| OrdinaryMapSnapshotRuntimeError::Capacity),
        Err(uninstalled_map) => {
            // The handle allocator returns failed roots after releasing its
            // table lock, so their Any owner Drop glue runs outside it.
            drop(uninstalled_map);
            Err(OrdinaryMapSnapshotRuntimeError::Capacity)
        }
    }
}

fn acquire_root(handle: i64) -> Result<super::handle::MapLease, OrdinaryMapSnapshotRuntimeError> {
    super::handle::ordinary_map_owner_lifecycle_i32_string_v1_acquire(handle).map_err(Into::into)
}

fn snapshot_from_map(
    map: &super::MimiMap,
) -> Result<&OrdinaryMapSnapshot, OrdinaryMapSnapshotRuntimeError> {
    map.ordinary_map_snapshot
        .as_ref()
        .ok_or(OrdinaryMapSnapshotRuntimeError::InvalidInput)
}

fn new_empty_root() -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    new_root(OrdinaryMapSnapshot::new())
}

fn set_i32_root(
    handle: i64,
    key: &str,
    value: i32,
) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    let source = acquire_root(handle)?;
    let snapshot = snapshot_from_map(&source)?.set_i32(key, value)?;
    drop(source);
    new_root(snapshot)
}

fn set_string_root(
    handle: i64,
    key: &str,
    value: &str,
) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    let source = acquire_root(handle)?;
    let snapshot = snapshot_from_map(&source)?.set_string(key, value)?;
    drop(source);
    new_root(snapshot)
}

fn remove_root(handle: i64, key: &str) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    let source = acquire_root(handle)?;
    let snapshot = snapshot_from_map(&source)?.remove(key)?;
    drop(source);
    new_root(snapshot)
}

fn clone_root_handle(handle: i64) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    let source = acquire_root(handle)?;
    let snapshot = snapshot_from_map(&source)?.clone_root()?;
    drop(source);
    new_root(snapshot)
}

fn size_root(handle: i64) -> Result<i32, OrdinaryMapSnapshotRuntimeError> {
    let root = acquire_root(handle)?;
    snapshot_from_map(&root)?
        .size_i32()
        .map_err(|_| OrdinaryMapSnapshotRuntimeError::SizeOverflow)
}

fn contains_root(handle: i64, key: &str) -> Result<bool, OrdinaryMapSnapshotRuntimeError> {
    let root = acquire_root(handle)?;
    Ok(snapshot_from_map(&root)?.contains(key))
}

fn drop_root(handle: i64) -> Result<(), OrdinaryMapSnapshotRuntimeError> {
    super::handle::ordinary_map_owner_lifecycle_i32_string_v1_drop(handle)?;
    super::ordinary_map_domain::forget_root(
        super::ordinary_map_domain::OrdinaryMapProfile::OwnerLifecycleI32StringV1,
        handle,
    );
    Ok(())
}

fn runtime_abort_for(error: OrdinaryMapSnapshotRuntimeError, operation: &'static [u8]) -> ! {
    match error {
        OrdinaryMapSnapshotRuntimeError::SizeOverflow => unsafe {
            super::mimi_runtime_abort(c"[E0802] ordinary Map snapshot size exceeds i32".as_ptr())
        },
        OrdinaryMapSnapshotRuntimeError::InvalidInput
        | OrdinaryMapSnapshotRuntimeError::Capacity
        | OrdinaryMapSnapshotRuntimeError::Owner(_)
        | OrdinaryMapSnapshotRuntimeError::Handle(_) => unsafe {
            super::mimi_runtime_abort(operation.as_ptr().cast())
        },
    }
}

unsafe fn checked_abi_bytes<'a>(
    pointer: *const u8,
    length: i64,
) -> Result<&'a [u8], OrdinaryMapSnapshotRuntimeError> {
    let length =
        usize::try_from(length).map_err(|_| OrdinaryMapSnapshotRuntimeError::InvalidInput)?;
    if length > isize::MAX as usize {
        return Err(OrdinaryMapSnapshotRuntimeError::InvalidInput);
    }
    if length == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() {
        return Err(OrdinaryMapSnapshotRuntimeError::InvalidInput);
    }
    // SAFETY: the caller of this helper guarantees `pointer` is readable for
    // `length` bytes; null, negative, and oversized lengths were rejected.
    Ok(unsafe { std::slice::from_raw_parts(pointer, length) })
}

fn checked_key(bytes: &[u8]) -> Result<String, OrdinaryMapSnapshotRuntimeError> {
    let source =
        std::str::from_utf8(bytes).map_err(|_| OrdinaryMapSnapshotRuntimeError::InvalidInput)?;
    try_copy_string(source).map_err(Into::into)
}

/// Create an empty ordinary Map lifecycle v1 root, or return zero on failure.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new() -> i64 {
    new_empty_root().unwrap_or(0)
}

/// Create a persistent root with an owned i32 entry. The source root remains
/// live and unchanged. Keys use exact byte lengths and may contain NUL.
/// Return zero on failure, after releasing every staged key and entry owner.
///
/// # Safety
/// For a positive `key_len`, `key` must reference that many readable bytes.
/// The bytes must form valid UTF-8. The handle must be a live root created by
/// this profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_i32(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: i32,
) -> i64 {
    let result = (|| {
        let key_bytes = unsafe { checked_abi_bytes(key, key_len) }?;
        let key = checked_key(key_bytes)?;
        set_i32_root(handle, &key, value)
    })();
    result.unwrap_or(0)
}

/// Create a persistent root with an owned String entry. The source root
/// remains live and unchanged. Keys and values use exact byte lengths.
/// Return zero on failure, after releasing every staged key and entry owner.
///
/// # Safety
/// For positive lengths, the pointers must reference readable byte ranges of
/// the specified lengths. Both byte ranges must form valid UTF-8. The handle
/// must be a live root created by this profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_string(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: *const u8,
    value_len: i64,
) -> i64 {
    let result = (|| {
        let key_bytes = unsafe { checked_abi_bytes(key, key_len) }?;
        let value_bytes = unsafe { checked_abi_bytes(value, value_len) }?;
        let key = checked_key(key_bytes)?;
        let value = std::str::from_utf8(value_bytes)
            .map_err(|_| OrdinaryMapSnapshotRuntimeError::InvalidInput)?;
        set_string_root(handle, &key, value)
    })();
    result.unwrap_or(0)
}

/// Create a persistent root without one key. The source remains live and a
/// missing key still returns an independent root with the same contents.
/// Return zero on failure, after releasing every staged key and entry owner.
///
/// # Safety
/// For a positive `key_len`, `key` must reference that many readable bytes.
/// The bytes must form valid UTF-8. The handle must be a live root created by
/// this profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_remove(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i64 {
    let result = (|| {
        let key_bytes = unsafe { checked_abi_bytes(key, key_len) }?;
        let key = checked_key(key_bytes)?;
        remove_root(handle, &key)
    })();
    result.unwrap_or(0)
}

/// Create a fresh ordinary Map lifecycle v1 root with independent typed
/// entry owners. The source remains live and unchanged. Invalid or stale
/// handles and staging failures return zero, as with persistent Set.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_clone(handle: i64) -> i64 {
    clone_root_handle(handle).unwrap_or(0)
}

/// Return the checked i32 entry count for a live ordinary Map lifecycle v1
/// root, or -1 on failure.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size(handle: i64) -> i32 {
    size_root(handle).unwrap_or(-1)
}

/// Return 0 or 1 for absence/presence, or -1 on failure.
///
/// # Safety
/// For a positive `key_len`, `key` must reference that many readable bytes.
/// The bytes must form valid UTF-8. The handle must be a live root created by
/// this profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_contains(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i32 {
    let result = (|| {
        let key_bytes = unsafe { checked_abi_bytes(key, key_len) }?;
        let key = checked_key(key_bytes)?;
        contains_root(handle, &key).map(i32::from)
    })();
    result.unwrap_or(-1)
}

/// Consume an ordinary Map lifecycle v1 root and release every typed entry
/// owner after the shared Map handle table has retired it.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop(handle: i64) {
    drop_root(handle).unwrap_or_else(|error| {
        runtime_abort_for(error, b"[E0800] ordinary Map lifecycle Drop failed\0")
    });
}

/// Getter status ABI for `ordinary-map-owned-get-i32-string-v1`.
pub const ORDINARY_MAP_OWNED_GET_OK: i32 = 0;
pub const ORDINARY_MAP_OWNED_GET_MISSING: i32 = 1;
pub const ORDINARY_MAP_OWNED_GET_TYPE_MISMATCH: i32 = 2;
pub const ORDINARY_MAP_OWNED_GET_CAPACITY: i32 = 3;

fn new_owned_get_root(
    snapshot: OrdinaryMapSnapshot,
) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    snapshot_allocation_checkpoint()?;
    match super::handle::map_new_handle_fallible(
        super::MimiMap::ordinary_map_owned_get_i32_string_v1(snapshot),
    ) {
        Ok(handle) => super::ordinary_map_domain::publish_new_root(
            super::ordinary_map_domain::OrdinaryMapProfile::OwnedGetI32StringV1,
            handle,
        )
        .map_err(|_| OrdinaryMapSnapshotRuntimeError::Capacity),
        Err(uninstalled_map) => {
            drop(uninstalled_map);
            Err(OrdinaryMapSnapshotRuntimeError::Capacity)
        }
    }
}

fn acquire_owned_get_root(
    handle: i64,
) -> Result<super::handle::MapLease, OrdinaryMapSnapshotRuntimeError> {
    super::handle::ordinary_map_owned_get_i32_string_v1_acquire(handle).map_err(Into::into)
}

fn derive_owned_get_root(
    handle: i64,
    update: impl FnOnce(&OrdinaryMapSnapshot) -> Result<OrdinaryMapSnapshot, OrdinaryMapSnapshotError>,
) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    let source = acquire_owned_get_root(handle)?;
    let snapshot = update(snapshot_from_map(&source)?)?;
    drop(source);
    new_owned_get_root(snapshot)
}

fn clone_owned_get_root(handle: i64) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    derive_owned_get_root(handle, OrdinaryMapSnapshot::clone_root)
}

fn get_owned_i32_from_root(
    root: &super::handle::MapLease,
    key: &str,
) -> Result<OrdinaryMapLookup<i32>, OrdinaryMapSnapshotRuntimeError> {
    Ok(snapshot_from_map(root)?.get_i32_typed(key)?)
}

fn get_owned_string_from_root(
    root: &super::handle::MapLease,
    key: &str,
) -> Result<OrdinaryMapLookup<String>, OrdinaryMapSnapshotRuntimeError> {
    Ok(snapshot_from_map(root)?.get_string_typed(key)?)
}

fn drop_owned_get_root(handle: i64) -> Result<(), OrdinaryMapSnapshotRuntimeError> {
    super::handle::ordinary_map_owned_get_i32_string_v1_drop(handle)?;
    super::ordinary_map_domain::forget_root(
        super::ordinary_map_domain::OrdinaryMapProfile::OwnedGetI32StringV1,
        handle,
    );
    Ok(())
}

unsafe fn owned_get_key(
    key: *const u8,
    key_len: i64,
) -> Result<String, OrdinaryMapSnapshotRuntimeError> {
    let bytes = unsafe { checked_abi_bytes(key, key_len) }?;
    #[cfg(test)]
    {
        // Input validation precedes the injected allocation failure, just as
        // checked_key validates UTF-8 before its real fallible copy.
        std::str::from_utf8(bytes).map_err(|_| OrdinaryMapSnapshotRuntimeError::InvalidInput)?;
        ordinary_map_owned_get_allocation_checkpoint_for_test()
            .map_err(|_| OrdinaryMapSnapshotRuntimeError::Capacity)?;
    }
    checked_key(bytes)
}

/// Validate the profile-specific handle before any fallible key allocation.
/// A stale or wrong-profile root must never be masked as the recoverable
/// Capacity status by an allocation failure.
unsafe fn acquire_owned_get_key(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> Result<(super::handle::MapLease, String), OrdinaryMapSnapshotRuntimeError> {
    let root = acquire_owned_get_root(handle)?;
    let key = unsafe { owned_get_key(key, key_len) }?;
    Ok((root, key))
}

/// Create an empty root for the distinct `ordinary-map-owned-get-i32-string-v1`
/// runtime profile. Return zero on failure.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_new() -> i64 {
    new_owned_get_root(OrdinaryMapSnapshot::new()).unwrap_or(0)
}

/// Create a persistent root with a copied i32 value. The source stays live.
/// The key uses an exact UTF-8 byte length, so embedded NUL is preserved.
/// Return zero on failure, after releasing every staged key and entry owner.
///
/// # Safety
/// For positive `key_len`, `key` points to that many readable valid UTF-8
/// bytes. `handle` is a live root made by this exact profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: i32,
) -> i64 {
    let result = (|| {
        let key = unsafe { checked_abi_bytes(key, key_len) }?;
        let key = checked_key(key)?;
        derive_owned_get_root(handle, |source| source.set_i32(&key, value))
    })();
    result.unwrap_or(0)
}

/// Create a persistent root with a copied String value. The source stays live.
/// Keys and values use exact UTF-8 byte lengths, including embedded NUL.
/// Return zero on failure, after releasing every staged key and entry owner.
///
/// # Safety
/// For positive lengths, both pointers address readable valid UTF-8 byte
/// ranges of the specified lengths. `handle` is a live root made by this
/// exact profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: *const u8,
    value_len: i64,
) -> i64 {
    let result = (|| {
        let key = unsafe { checked_abi_bytes(key, key_len) }?;
        let value = unsafe { checked_abi_bytes(value, value_len) }?;
        let key = checked_key(key)?;
        let value = std::str::from_utf8(value)
            .map_err(|_| OrdinaryMapSnapshotRuntimeError::InvalidInput)?;
        derive_owned_get_root(handle, |source| source.set_string(&key, value))
    })();
    result.unwrap_or(0)
}

/// Create a persistent root without `key`; the source stays live.
/// Return zero on failure, after releasing every staged key and entry owner.
///
/// # Safety
/// For positive `key_len`, `key` points to that many readable valid UTF-8
/// bytes. `handle` is a live root made by this exact profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_remove(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i64 {
    let result = (|| {
        let key = unsafe { checked_abi_bytes(key, key_len) }?;
        let key = checked_key(key)?;
        derive_owned_get_root(handle, |source| source.remove(&key))
    })();
    result.unwrap_or(0)
}

/// Create a fresh owned-Get root with independent typed entry owners. The
/// source remains live. This preserves the owned-Get handle namespace and
/// returns zero for invalid handles or capacity, as with persistent Set.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_clone(handle: i64) -> i64 {
    clone_owned_get_root(handle).unwrap_or(0)
}

/// Return the entry count in a live owned-Get root, or -1 on failure.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_size(handle: i64) -> i32 {
    let result = (|| {
        let root = acquire_owned_get_root(handle)?;
        snapshot_from_map(&root)?
            .size_i32()
            .map_err(|_| OrdinaryMapSnapshotRuntimeError::SizeOverflow)
    })();
    result.unwrap_or(-1)
}

/// Return 0 or 1 for absence/presence of `key`, or -1 on failure.
///
/// # Safety
/// For positive `key_len`, `key` points to that many readable valid UTF-8
/// bytes. `handle` is a live root made by this exact profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_contains(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i32 {
    let result = (|| {
        let key = unsafe { checked_abi_bytes(key, key_len) }?;
        let key = checked_key(key)?;
        let root = acquire_owned_get_root(handle)?;
        snapshot_from_map(&root).map(|snapshot| i32::from(snapshot.contains(&key)))
    })();
    result.unwrap_or(-1)
}

/// Get an i32 into `out_value`. Status 0 means present, 1 means missing, 2
/// means present with a different stored type, and 3 means an allocation
/// capacity failure while staging the key. Invalid/stale/wrong-profile roots
/// abort through the runtime error path and never become lookup statuses.
///
/// # Safety
/// `out_value` points to one writable i32. For positive `key_len`, `key`
/// points to that many readable valid UTF-8 bytes. `handle` is a live root
/// made by this exact profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_get_i32(
    handle: i64,
    key: *const u8,
    key_len: i64,
    out_value: *mut i32,
) -> i32 {
    if out_value.is_null() {
        runtime_abort_for(
            OrdinaryMapSnapshotRuntimeError::InvalidInput,
            b"[E0800] ordinary Map owned-Get i32 received a null output\0",
        );
    }
    unsafe { *out_value = 0 };
    let (root, key) = match unsafe { acquire_owned_get_key(handle, key, key_len) } {
        Ok(root_and_key) => root_and_key,
        Err(OrdinaryMapSnapshotRuntimeError::Capacity) => return ORDINARY_MAP_OWNED_GET_CAPACITY,
        Err(error) => runtime_abort_for(error, b"[E0800] ordinary Map owned-Get i32 failed\0"),
    };
    match get_owned_i32_from_root(&root, &key) {
        Ok(OrdinaryMapLookup::Value(value)) => {
            unsafe { *out_value = value };
            ORDINARY_MAP_OWNED_GET_OK
        }
        Ok(OrdinaryMapLookup::Missing) => ORDINARY_MAP_OWNED_GET_MISSING,
        Ok(OrdinaryMapLookup::TypeMismatch) => ORDINARY_MAP_OWNED_GET_TYPE_MISMATCH,
        Err(OrdinaryMapSnapshotRuntimeError::Capacity) => ORDINARY_MAP_OWNED_GET_CAPACITY,
        Err(error) => runtime_abort_for(error, b"[E0800] ordinary Map owned-Get i32 failed\0"),
    }
}

/// Get a Mimi String into `(out_ptr, out_len)`. On status 0, `out_ptr`
/// points to a fresh, NUL-terminated `mimi_alloc` payload with the canonical
/// direct String `{ptr, len}` layout; the caller owns it and must release it
/// with `mimi_string_free`. Empty strings are successful non-null allocations
/// with length zero. Statuses are 0=Ok, 1=Missing, 2=TypeMismatch, 3=Capacity.
/// Invalid/stale/wrong-profile roots abort and never become lookup statuses.
///
/// # Safety
/// Both output pointers point to writable values. For positive `key_len`,
/// `key` points to that many readable valid UTF-8 bytes. `handle` is a live
/// root made by this exact profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
    handle: i64,
    key: *const u8,
    key_len: i64,
    out_ptr: *mut *mut std::ffi::c_char,
    out_len: *mut i64,
) -> i32 {
    if out_ptr.is_null() || out_len.is_null() {
        runtime_abort_for(
            OrdinaryMapSnapshotRuntimeError::InvalidInput,
            b"[E0800] ordinary Map owned-Get String received a null output\0",
        );
    }
    unsafe {
        *out_ptr = std::ptr::null_mut();
        *out_len = 0;
    }
    let (root, key) = match unsafe { acquire_owned_get_key(handle, key, key_len) } {
        Ok(root_and_key) => root_and_key,
        Err(OrdinaryMapSnapshotRuntimeError::Capacity) => return ORDINARY_MAP_OWNED_GET_CAPACITY,
        Err(error) => runtime_abort_for(error, b"[E0800] ordinary Map owned-Get String failed\0"),
    };
    let value = match get_owned_string_from_root(&root, &key) {
        Ok(OrdinaryMapLookup::Value(value)) => value,
        Ok(OrdinaryMapLookup::Missing) => return ORDINARY_MAP_OWNED_GET_MISSING,
        Ok(OrdinaryMapLookup::TypeMismatch) => return ORDINARY_MAP_OWNED_GET_TYPE_MISMATCH,
        Err(OrdinaryMapSnapshotRuntimeError::Capacity) => return ORDINARY_MAP_OWNED_GET_CAPACITY,
        Err(error) => runtime_abort_for(error, b"[E0800] ordinary Map owned-Get String failed\0"),
    };
    // The value is now an independent String snapshot, so do not hold the
    // per-root operation lease during the outer ABI allocation.
    drop(root);
    let Ok(length) = i64::try_from(value.len()) else {
        return ORDINARY_MAP_OWNED_GET_CAPACITY;
    };
    #[cfg(test)]
    if ordinary_map_owned_get_allocation_checkpoint_for_test().is_err() {
        return ORDINARY_MAP_OWNED_GET_CAPACITY;
    }
    let ptr = super::alloc_c_string_from_bytes(value.as_bytes());
    if ptr.is_null() {
        return ORDINARY_MAP_OWNED_GET_CAPACITY;
    }
    unsafe {
        *out_ptr = ptr;
        *out_len = length;
    }
    ORDINARY_MAP_OWNED_GET_OK
}

/// Exact status ABI shared by ordinary `(bool, Any)` get and ManagedAny v1.
/// Missing is successful and publishes a fresh i32-zero owner with found=0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
enum ManagedAnyStatus {
    Ok = 0,
    InvalidInput = 1,
    TypeMismatch = 2,
    Capacity = 3,
    InvalidToken = 4,
    StaleToken = 5,
    VersionMismatch = 6,
    DescriptorMismatch = 7,
    Poisoned = 8,
    UnmanagedToken = 9,
    RootInvalid = 10,
    RootStaleDestroyed = 11,
    RootReentrant = 12,
    DomainConflict = 13,
}

impl From<AnyOwnerError> for ManagedAnyStatus {
    fn from(error: AnyOwnerError) -> Self {
        match error {
            AnyOwnerError::InvalidToken => Self::InvalidToken,
            AnyOwnerError::StaleToken => Self::StaleToken,
            AnyOwnerError::DescriptorMismatch => Self::DescriptorMismatch,
            AnyOwnerError::Capacity => Self::Capacity,
            AnyOwnerError::Poisoned => Self::Poisoned,
        }
    }
}

impl From<AnyOwnerReceiptError> for ManagedAnyStatus {
    fn from(error: AnyOwnerReceiptError) -> Self {
        match error {
            AnyOwnerReceiptError::KindMismatch => Self::TypeMismatch,
            AnyOwnerReceiptError::VersionMismatch => Self::VersionMismatch,
            AnyOwnerReceiptError::Owner(error) => error.into(),
        }
    }
}

impl From<super::ordinary_map_domain::ManagedAnyDomainError> for ManagedAnyStatus {
    fn from(error: super::ordinary_map_domain::ManagedAnyDomainError) -> Self {
        match error {
            super::ordinary_map_domain::ManagedAnyDomainError::Capacity => Self::Capacity,
            super::ordinary_map_domain::ManagedAnyDomainError::ConflictingReceipt => {
                Self::DomainConflict
            }
            super::ordinary_map_domain::ManagedAnyDomainError::Poisoned => Self::Poisoned,
        }
    }
}

impl From<OrdinaryMapSnapshotRuntimeError> for ManagedAnyStatus {
    fn from(error: OrdinaryMapSnapshotRuntimeError) -> Self {
        match error {
            OrdinaryMapSnapshotRuntimeError::InvalidInput => Self::InvalidInput,
            OrdinaryMapSnapshotRuntimeError::Capacity
            | OrdinaryMapSnapshotRuntimeError::SizeOverflow => Self::Capacity,
            OrdinaryMapSnapshotRuntimeError::Owner(error) => error.into(),
            OrdinaryMapSnapshotRuntimeError::Handle(error) => match error {
                super::handle::HandleError::Invalid => Self::RootInvalid,
                super::handle::HandleError::StaleGeneration
                | super::handle::HandleError::Destroyed => Self::RootStaleDestroyed,
                super::handle::HandleError::Reentrant => Self::RootReentrant,
            },
        }
    }
}

fn managed_token_receipt(
    owner: i64,
) -> Result<(AnyOwnerToken, any_owner::DescriptorIdentity), ManagedAnyStatus> {
    let token = AnyOwnerToken::from_wire_bits(owner as u64).map_err(ManagedAnyStatus::from)?;
    let descriptor = super::ordinary_map_domain::managed_any_descriptor(token)
        .map_err(ManagedAnyStatus::from)?;
    match descriptor {
        Some(descriptor) => Ok((token, descriptor)),
        None => {
            // A negative domain lookup never authorizes payload access. Inspect
            // only liveness to distinguish stale tokens from live entry owners.
            any_owner::validate(token).map_err(ManagedAnyStatus::from)?;
            Err(ManagedAnyStatus::UnmanagedToken)
        }
    }
}

fn managed_kind(kind: i32) -> Result<AnyOwnerKind, ManagedAnyStatus> {
    match kind {
        1 => Ok(AnyOwnerKind::I32),
        2 => Ok(AnyOwnerKind::OwnedString),
        _ => Err(ManagedAnyStatus::InvalidInput),
    }
}

fn managed_allocation_checkpoint() -> Result<(), ManagedAnyStatus> {
    #[cfg(test)]
    ordinary_map_owned_get_allocation_checkpoint_for_test()
        .map_err(|_| ManagedAnyStatus::Capacity)?;
    Ok(())
}

/// Return the ordinary Map `(found, Any)` shape through an independent typed
/// owner token. Status 0 includes a missing key: found=0 and a fresh i32-zero
/// owner. Each success registers the snapshot in its creator's execution domain
/// before publication. Map mutations and destruction cannot invalidate it.
///
/// # Safety
/// Non-null outputs point to writable values and do not overlap. For positive
/// key_len, key addresses that many readable bytes. Neither Any wire bits nor
/// a legacy ValueHandle may be substituted for this exact profile's root.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
    handle: i64,
    key: *const u8,
    key_len: i64,
    out_found: *mut i32,
    out_owner: *mut i64,
) -> i32 {
    if !out_found.is_null() {
        unsafe { *out_found = 0 };
    }
    if !out_owner.is_null() {
        unsafe { *out_owner = 0 };
    }
    if out_found.is_null() || out_owner.is_null() {
        return ManagedAnyStatus::InvalidInput as i32;
    }
    let result = (|| {
        let (root, key) = unsafe { acquire_owned_get_key(handle, key, key_len) }
            .map_err(ManagedAnyStatus::from)?;
        managed_allocation_checkpoint()?;
        let (found, snapshot) = snapshot_from_map(&root)
            .map_err(ManagedAnyStatus::from)?
            .get(&key)
            .map_err(OrdinaryMapSnapshotRuntimeError::from)
            .map_err(ManagedAnyStatus::from)?;
        drop(root);
        let owner = snapshot.publish_managed()?;
        Ok::<_, ManagedAnyStatus>((found, owner))
    })();
    match result {
        Ok((found, owner)) => {
            unsafe {
                *out_found = i32::from(found);
                *out_owner = owner;
            }
            ManagedAnyStatus::Ok as i32
        }
        Err(status) => status as i32,
    }
}

/// Snapshot every value from an ordinary owned-Get root into a canonical
/// `List<ManagedAny>`.  The list owns one fresh generation-bearing v1 token per
/// element; it never stores entry tokens or borrows the source root.  Values
/// are ordered by the same full UTF-8 key-byte order as `OrdinaryMapSnapshot::values`.
/// A null result means the source handle, owner table, domain registration, or
/// list allocation rejected the operation.  Any published prefix is released
/// before returning null, so a failed stage cannot leak or double-release.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_values(
    handle: i64,
) -> *mut super::MimiList {
    let root = match acquire_owned_get_root(handle) {
        Ok(root) => root,
        Err(_) => return std::ptr::null_mut(),
    };
    let values = match snapshot_from_map(&root).and_then(|snapshot| {
        snapshot
            .values()
            .map_err(OrdinaryMapSnapshotRuntimeError::from)
    }) {
        Ok(values) => values,
        Err(_) => return std::ptr::null_mut(),
    };
    // Every OwnedAny in `values` is independent of the source root.  Release
    // the root lease before publishing any token into the result domain.
    drop(root);
    let list = super::mimi_mir_list_new_managed_any();
    if list.is_null() {
        return std::ptr::null_mut();
    }
    for value in values {
        let owner = match value.publish_managed() {
            Ok(owner) => owner,
            Err(_) => {
                // No token was published for this value; prior list elements
                // are the only live result owners and are drained below.
                unsafe { super::mimi_mir_list_drop_managed_any(list) };
                return std::ptr::null_mut();
            }
        };
        let pushed = unsafe { super::mimi_mir_list_push_managed_any(list, owner) } != 0;
        if !pushed {
            // The failed append leaves `owner` outside the list.  Consume it
            // exactly once before draining the prefix already transferred.
            let _ = super::mimi_mir_managed_any_i32_string_v1_drop(owner, 1);
            unsafe { super::mimi_mir_list_drop_managed_any(list) };
            return std::ptr::null_mut();
        }
    }
    list
}

/// Allocate and publish one fresh generation-bearing owner for the checked
/// `i32 -> Any` injection. The Checker/MIR receipt selects the i32 tag; this
/// runtime entry only materializes the canonical v1 owner and reports
/// allocation or domain-registration failure through the ordinary status ABI.
///
/// # Safety
/// out_owner points to one writable i64 and does not alias the source scalar.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_managed_any_i32_string_v1_pack_i32(
    value: i32,
    out_owner: *mut i64,
) -> i32 {
    if out_owner.is_null() {
        return ManagedAnyStatus::InvalidInput as i32;
    }
    unsafe { *out_owner = 0 };
    let result = OwnedAny::from_i32(value)
        .map_err(ManagedAnyStatus::from)
        .and_then(OwnedAny::publish_managed);
    match result {
        Ok(owner) => {
            unsafe { *out_owner = owner };
            ManagedAnyStatus::Ok as i32
        }
        Err(status) => status as i32,
    }
}

/// Clone with the exact pinned descriptor version; the new token has its own
/// creator-domain obligation. The source is borrowed and stays live.
///
/// # Safety
/// out_owner points to one writable i64; owner is the opaque ManagedAny token.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_managed_any_i32_string_v1_clone(
    owner: i64,
    expected_version: i32,
    out_owner: *mut i64,
) -> i32 {
    if out_owner.is_null() {
        return ManagedAnyStatus::InvalidInput as i32;
    }
    unsafe { *out_owner = 0 };
    let result = (|| {
        let (token, descriptor) = managed_token_receipt(owner)?;
        let token = any_owner::clone_owner_v1_with_checkpoint(
            token,
            descriptor,
            expected_version as u32,
            || managed_allocation_checkpoint().map_err(|_| AnyOwnerError::Capacity),
        )
        .map_err(ManagedAnyStatus::from)?;
        record_created_token(token);
        OwnedAny { token }.publish_managed()
    })();
    match result {
        Ok(owner) => {
            unsafe { *out_owner = owner };
            ManagedAnyStatus::Ok as i32
        }
        Err(status) => status as i32,
    }
}

/// Consume exactly one ManagedAny owner. Version, descriptor and liveness
/// are checked under the same owner-table lock which detaches its payload;
/// Drop glue runs only after that lock ends. Normal release may run on another
/// thread and forgets the original creator's association after success.
#[no_mangle]
pub extern "C" fn mimi_mir_managed_any_i32_string_v1_drop(
    owner: i64,
    expected_version: i32,
) -> i32 {
    let result = (|| {
        let (token, descriptor) = managed_token_receipt(owner)?;
        any_owner::release_owner_v1(token, descriptor, expected_version as u32)
            .map_err(ManagedAnyStatus::from)?;
        super::ordinary_map_domain::forget_managed_any(token);
        Ok::<_, ManagedAnyStatus>(())
    })();
    match result {
        Ok(()) => ManagedAnyStatus::Ok as i32,
        Err(status) => status as i32,
    }
}

/// Borrow a checked i32 snapshot. expected_kind is 1 for i32, 2 for String;
/// descriptor version is 1. A mismatch leaves the source owner unchanged.
///
/// # Safety
/// out_value points to one writable i32; owner is an opaque ManagedAny token.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_managed_any_i32_string_v1_read_i32(
    owner: i64,
    expected_kind: i32,
    expected_version: i32,
    out_value: *mut i32,
) -> i32 {
    if out_value.is_null() {
        return ManagedAnyStatus::InvalidInput as i32;
    }
    unsafe { *out_value = 0 };
    let result = (|| {
        let (token, descriptor) = managed_token_receipt(owner)?;
        any_owner::read_i32_v1(
            token,
            descriptor,
            managed_kind(expected_kind)?,
            expected_version as u32,
        )
        .map_err(ManagedAnyStatus::from)
    })();
    match result {
        Ok(value) => {
            unsafe { *out_value = value };
            ManagedAnyStatus::Ok as i32
        }
        Err(status) => status as i32,
    }
}

/// Borrow a checked String snapshot and return a fresh canonical `{ptr, len}`
/// copy, registered in the copying thread's String domain before publication.
/// The original Any remains live. Empty strings have a non-null allocation;
/// explicit lengths preserve Unicode and embedded NUL.
///
/// # Safety
/// Each non-null output points to one writable value and outputs do not overlap.
/// The returned successful String is released once by mimi_string_free/mimi_free.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_managed_any_i32_string_v1_copy_string(
    owner: i64,
    expected_kind: i32,
    expected_version: i32,
    out_ptr: *mut *mut std::ffi::c_char,
    out_len: *mut i64,
) -> i32 {
    if !out_ptr.is_null() {
        unsafe { *out_ptr = std::ptr::null_mut() };
    }
    if !out_len.is_null() {
        unsafe { *out_len = 0 };
    }
    if out_ptr.is_null() || out_len.is_null() {
        return ManagedAnyStatus::InvalidInput as i32;
    }
    let result = (|| {
        let (token, descriptor) = managed_token_receipt(owner)?;
        let kind = managed_kind(expected_kind)?;
        let value = any_owner::copy_string_v1_with_checkpoint(
            token,
            descriptor,
            kind,
            expected_version as u32,
            || managed_allocation_checkpoint().map_err(|_| AnyOwnerError::Capacity),
        )
        .map_err(ManagedAnyStatus::from)?;
        let length = i64::try_from(value.len()).map_err(|_| ManagedAnyStatus::Capacity)?;
        managed_allocation_checkpoint()?;
        let pointer = super::alloc_c_string_from_bytes(value.as_bytes());
        if pointer.is_null() {
            return Err(ManagedAnyStatus::Capacity);
        }
        if let Err(error) =
            super::ordinary_map_domain::register_fresh_owned_string(pointer, value.len())
        {
            // Registration returned and its lock has ended. Roll back exactly
            // this fresh canonical allocation; source Any stays borrowed/live.
            super::mimi_free(pointer.cast());
            return Err(match error {
                super::ordinary_map_domain::StringRegistrationError::Capacity => {
                    ManagedAnyStatus::Capacity
                }
                super::ordinary_map_domain::StringRegistrationError::ConflictingReceipt => {
                    ManagedAnyStatus::DomainConflict
                }
            });
        }
        Ok::<_, ManagedAnyStatus>((pointer, length))
    })();
    match result {
        Ok((pointer, length)) => {
            unsafe {
                *out_ptr = pointer;
                *out_len = length;
            }
            ManagedAnyStatus::Ok as i32
        }
        Err(status) => status as i32,
    }
}

/// Consume a root created by the owned-Get profile and drop its typed entries.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(handle: i64) {
    drop_owned_get_root(handle).unwrap_or_else(|error| {
        runtime_abort_for(error, b"[E0800] ordinary Map owned-Get Drop failed\0")
    });
}

#[cfg(test)]
mod tests {
    use super::{OrdinaryMapSnapshot, OwnedAny};
    use crate::runtime::any_owner::{self, AnyOwnerKind};

    fn read_i32(value: &OwnedAny) -> i32 {
        value.read_i32().expect("value is an i32 owner")
    }

    fn read_string(value: &OwnedAny) -> String {
        value.copy_string().expect("value is a string owner")
    }

    #[test]
    fn unicode_and_nul_keys_keep_their_full_utf8_bytes_and_values_are_heterogeneous() {
        let root = OrdinaryMapSnapshot::new()
            .set_string("键\0🧪", "值\0🚀")
            .expect("string entry");
        let root = root
            .set_i32("integer", -17)
            .expect("heterogeneous integer entry");

        assert_eq!(root.size(), 2);
        assert!(root.contains("键\0🧪"));
        assert!(root.contains("integer"));
        assert!(!root.contains("键"));

        let (present, text) = root.get("键\0🧪").expect("get string");
        assert!(present);
        assert_eq!(read_string(&text), "值\0🚀");
        assert_eq!(
            any_owner::validate(text.token),
            Ok(AnyOwnerKind::OwnedString)
        );

        let (present, integer) = root.get("integer").expect("get integer");
        assert!(present);
        assert_eq!(read_i32(&integer), -17);
        assert_eq!(any_owner::validate(integer.token), Ok(AnyOwnerKind::I32));
    }

    #[test]
    fn updates_are_persistent_and_do_not_change_the_source_root() {
        let original = OrdinaryMapSnapshot::new()
            .set_i32("count", 4)
            .expect("initial root");
        let updated = original
            .set_i32("count", 9)
            .expect("persistent replacement");

        assert_eq!(original.size(), 1);
        assert_eq!(updated.size(), 1);
        assert_eq!(read_i32(&original.get("count").unwrap().1), 4);
        assert_eq!(read_i32(&updated.get("count").unwrap().1), 9);
    }

    #[test]
    fn get_snapshot_survives_all_source_roots_being_dropped() {
        let root = OrdinaryMapSnapshot::new()
            .set_string("kept", "alive")
            .expect("root");
        let (present, snapshot) = root.get("kept").expect("get snapshot");
        assert!(present);
        let token = snapshot.token;
        drop(root);

        assert_eq!(any_owner::copy_string(token), Ok("alive".to_owned()));
        drop(snapshot);
        assert_eq!(
            any_owner::validate(token),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
    }

    #[test]
    fn missing_get_returns_false_with_a_valid_i32_zero_owner() {
        let root = OrdinaryMapSnapshot::new();
        let (present, zero) = root.get("absent").expect("missing get");

        assert!(!present);
        assert_eq!(read_i32(&zero), 0);
        assert_eq!(any_owner::validate(zero.token), Ok(AnyOwnerKind::I32));
        assert_eq!(root.size(), 0);
        assert!(!root.contains("absent"));
    }

    #[test]
    fn cloned_roots_have_independent_owners_in_either_drop_order() {
        let root = OrdinaryMapSnapshot::new().set_i32("n", 23).expect("root");
        let cloned = root.clone_root().expect("clone root");
        let root_token = root.entries.get("n").unwrap().token;
        let cloned_token = cloned.entries.get("n").unwrap().token;
        assert_ne!(root_token, cloned_token);

        drop(cloned);
        assert_eq!(any_owner::read_i32(root_token), Ok(23));
        drop(root);
        assert_eq!(
            any_owner::validate(root_token),
            Err(any_owner::AnyOwnerError::StaleToken)
        );

        let root = OrdinaryMapSnapshot::new()
            .set_i32("n", 31)
            .expect("second root");
        let cloned = root.clone_root().expect("second clone");
        let cloned_token = cloned.entries.get("n").unwrap().token;

        drop(root);
        assert_eq!(any_owner::read_i32(cloned_token), Ok(31));
        drop(cloned);
        assert_eq!(
            any_owner::validate(cloned_token),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
    }

    fn read_root_string(root: &OrdinaryMapSnapshot, key: &str) -> String {
        let entry = root.entries.get(key).expect("expected string entry");
        read_string(entry)
    }

    fn assert_string_clone_lifetimes(source_dropped_first: bool) {
        let unicode_key = "普通键🧭";
        let nul_key = "键\0尾";
        let unicode_value = "原始值🌿";
        let nul_value = "值\0🧪";
        let root = OrdinaryMapSnapshot::new()
            .set_string(unicode_key, unicode_value)
            .expect("Unicode key and value");
        let root = root
            .set_string(nul_key, nul_value)
            .expect("NUL key and value");
        let cloned = root.clone_root().expect("clone String root");

        let root_unicode_token = root.entries.get(unicode_key).unwrap().token;
        let cloned_unicode_token = cloned.entries.get(unicode_key).unwrap().token;
        let root_nul_token = root.entries.get(nul_key).unwrap().token;
        let cloned_nul_token = cloned.entries.get(nul_key).unwrap().token;
        assert_ne!(root_unicode_token, cloned_unicode_token);
        assert_ne!(root_nul_token, cloned_nul_token);
        assert_eq!(read_root_string(&root, unicode_key), unicode_value);
        assert_eq!(read_root_string(&root, nul_key), nul_value);
        assert_eq!(read_root_string(&cloned, unicode_key), unicode_value);
        assert_eq!(read_root_string(&cloned, nul_key), nul_value);

        let (present, value_snapshot) = root.get(nul_key).expect("owned NUL value snapshot");
        assert!(present);
        let snapshot_token = value_snapshot.token;
        assert_ne!(snapshot_token, root_nul_token);
        assert_eq!(read_string(&value_snapshot), nul_value);

        let overwritten = cloned
            .set_string(unicode_key, "分支更新✨")
            .expect("overwrite cloned Unicode entry");
        let overwritten_unicode_token = overwritten.entries.get(unicode_key).unwrap().token;
        let overwritten_nul_token = overwritten.entries.get(nul_key).unwrap().token;
        assert_ne!(overwritten_unicode_token, cloned_unicode_token);
        assert_eq!(read_root_string(&cloned, unicode_key), unicode_value);
        assert_eq!(read_root_string(&overwritten, unicode_key), "分支更新✨");
        assert_eq!(read_root_string(&overwritten, nul_key), nul_value);
        assert_eq!(read_string(&value_snapshot), nul_value);

        let without_nul = overwritten
            .remove(nul_key)
            .expect("remove cloned NUL entry");
        let without_nul_unicode_token = without_nul.entries.get(unicode_key).unwrap().token;
        assert!(without_nul.contains(unicode_key));
        assert!(!without_nul.contains(nul_key));
        assert!(overwritten.contains(nul_key));
        assert!(root.contains(nul_key));
        assert_eq!(read_root_string(&without_nul, unicode_key), "分支更新✨");
        assert_eq!(read_root_string(&overwritten, nul_key), nul_value);
        assert_eq!(read_string(&value_snapshot), nul_value);

        if source_dropped_first {
            drop(root);
            assert_eq!(
                any_owner::validate(root_unicode_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(
                any_owner::validate(root_nul_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(read_root_string(&cloned, unicode_key), unicode_value);
            assert_eq!(read_root_string(&cloned, nul_key), nul_value);
            assert_eq!(read_root_string(&overwritten, unicode_key), "分支更新✨");
            assert_eq!(read_string(&value_snapshot), nul_value);

            drop(cloned);
            assert_eq!(
                any_owner::validate(cloned_unicode_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(
                any_owner::validate(cloned_nul_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            drop(overwritten);
            assert_eq!(
                any_owner::validate(overwritten_unicode_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(
                any_owner::validate(overwritten_nul_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            drop(without_nul);
            assert_eq!(
                any_owner::validate(without_nul_unicode_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
        } else {
            drop(cloned);
            assert_eq!(
                any_owner::validate(cloned_unicode_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(
                any_owner::validate(cloned_nul_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            drop(overwritten);
            assert_eq!(
                any_owner::validate(overwritten_unicode_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(
                any_owner::validate(overwritten_nul_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            drop(without_nul);
            assert_eq!(
                any_owner::validate(without_nul_unicode_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(read_root_string(&root, unicode_key), unicode_value);
            assert_eq!(read_root_string(&root, nul_key), nul_value);

            drop(root);
            assert_eq!(
                any_owner::validate(root_unicode_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(
                any_owner::validate(root_nul_token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
        }

        assert_eq!(
            any_owner::copy_string(snapshot_token),
            Ok(nul_value.to_owned())
        );
        drop(value_snapshot);
        assert_eq!(
            any_owner::validate(snapshot_token),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
    }

    #[test]
    fn cloned_string_roots_and_get_snapshots_survive_overwrite_remove_and_both_drop_orders() {
        assert_string_clone_lifetimes(false);
        assert_string_clone_lifetimes(true);
    }

    fn map_entry_token(handle: i64, key: &str) -> any_owner::AnyOwnerToken {
        let map = super::super::handle::ordinary_map_owner_lifecycle_i32_string_v1_acquire(handle)
            .expect("ordinary snapshot root");
        map.ordinary_map_snapshot
            .as_ref()
            .and_then(|root| root.entries.get(key))
            .map(|value| value.token)
            .expect("typed entry")
    }

    #[test]
    fn runtime_root_set_and_remove_preserve_sources_and_release_each_owner() {
        let empty = super::new_empty_root().expect("empty root");
        let first = super::set_string_root(empty, "k\0🧪", "old\0值").expect("first root");
        let first_string = map_entry_token(first, "k\0🧪");
        let mixed = super::set_i32_root(first, "count", 0).expect("mixed root");
        let mixed_string = map_entry_token(mixed, "k\0🧪");
        let replaced = super::set_i32_root(mixed, "k\0🧪", 7).expect("cross-kind replace");

        assert_ne!(first_string, mixed_string);
        assert_eq!(super::size_root(empty), Ok(0));
        assert_eq!(super::size_root(first), Ok(1));
        assert_eq!(super::contains_root(first, "count"), Ok(false));
        assert_eq!(super::size_root(mixed), Ok(2));
        assert_eq!(super::contains_root(mixed, "k\0🧪"), Ok(true));
        assert_eq!(super::size_root(replaced), Ok(2));
        assert_eq!(
            any_owner::validate(map_entry_token(replaced, "k\0🧪")),
            Ok(AnyOwnerKind::I32)
        );

        let removed = super::remove_root(replaced, "k\0🧪").expect("remove root");
        let missing_remove = super::remove_root(removed, "not present").expect("missing remove");
        assert_eq!(super::size_root(removed), Ok(1));
        assert_eq!(super::contains_root(removed, "k\0🧪"), Ok(false));
        assert_eq!(super::size_root(missing_remove), Ok(1));
        assert_eq!(super::contains_root(missing_remove, "count"), Ok(true));

        super::drop_root(empty).unwrap();
        super::drop_root(first).unwrap();
        assert_eq!(
            any_owner::validate(first_string),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        assert_eq!(
            any_owner::copy_string(mixed_string),
            Ok("old\0值".to_owned())
        );
        super::drop_root(mixed).unwrap();
        assert_eq!(
            any_owner::validate(mixed_string),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        super::drop_root(replaced).unwrap();
        super::drop_root(removed).unwrap();
        super::drop_root(missing_remove).unwrap();
        assert!(matches!(
            super::size_root(replaced),
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(_))
        ));
    }

    #[test]
    fn runtime_profile_handles_reject_legacy_and_map_any_owner_roots() {
        let legacy = crate::runtime::mimi_map_new();
        assert!(matches!(
            super::size_root(legacy),
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(
                super::super::handle::HandleError::Invalid
            ))
        ));
        unsafe { crate::runtime::mimi_map_destroy(legacy) };

        let map_any_owner =
            super::super::handle::map_new_handle(crate::runtime::MimiMap::canonical_any_owner_v1());
        assert!(matches!(
            super::contains_root(map_any_owner, "k"),
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(
                super::super::handle::HandleError::Invalid
            ))
        ));
        assert_eq!(
            super::drop_root(map_any_owner),
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(
                super::super::handle::HandleError::Invalid
            ))
        );
        super::super::handle::map_any_owner_drop(map_any_owner).unwrap();
    }

    #[test]
    fn staged_set_failures_preserve_source_and_retire_every_temporary_owner() {
        let empty = super::new_empty_root().expect("empty root");
        let first = super::set_string_root(empty, "text", "owned").expect("string root");
        let source = super::set_i32_root(first, "count", 4).expect("mixed source root");
        super::drop_root(empty).unwrap();
        super::drop_root(first).unwrap();
        let source_string = map_entry_token(source, "text");
        let source_integer = map_entry_token(source, "count");

        let mut saw_failure = false;
        let mut saw_partial_owner_rollback = false;
        let mut saw_publication_rollback = false;
        let mut succeeded = false;

        // The operation checkpoints key copying, the new payload owner, table
        // reservation, both retained entry clones, and final root publication.
        for fail_after in 0..16 {
            super::begin_failure_injection(fail_after);
            let result = super::set_i32_root(source, "new", 9);
            let staged = super::end_failure_injection();

            match result {
                Err(super::OrdinaryMapSnapshotRuntimeError::Capacity) => {
                    saw_failure = true;
                    saw_partial_owner_rollback |= !staged.is_empty();
                    saw_publication_rollback |= staged.len() == 3;
                    assert_eq!(super::size_root(source), Ok(2));
                    assert_eq!(super::contains_root(source, "new"), Ok(false));
                    assert_eq!(
                        any_owner::validate(source_string),
                        Ok(AnyOwnerKind::OwnedString)
                    );
                    assert_eq!(any_owner::read_i32(source_integer), Ok(4));
                    for token in staged {
                        assert_eq!(
                            any_owner::validate(token),
                            Err(any_owner::AnyOwnerError::StaleToken),
                            "failed staging must retire every temporary token"
                        );
                    }
                }
                Ok(published) => {
                    succeeded = true;
                    assert_eq!(super::size_root(source), Ok(2));
                    assert_eq!(super::size_root(published), Ok(3));
                    super::drop_root(published).unwrap();
                    for token in staged {
                        assert_eq!(
                            any_owner::validate(token),
                            Err(any_owner::AnyOwnerError::StaleToken),
                            "dropping the published root must release its cloned owners"
                        );
                    }
                    break;
                }
                Err(error) => panic!("unexpected staged Set error: {error:?}"),
            }
        }

        assert!(saw_failure);
        assert!(saw_partial_owner_rollback);
        assert!(saw_publication_rollback);
        assert!(succeeded);
        super::drop_root(source).unwrap();
    }

    #[test]
    fn exported_runtime_abi_uses_explicit_utf8_byte_lengths_for_nul_keys() {
        let key = "钥匙\0🔑";
        let value = "值\0🧪";
        let first = super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new();
        let second = unsafe {
            super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_string(
                first,
                key.as_ptr(),
                key.len() as i64,
                value.as_ptr(),
                value.len() as i64,
            )
        };
        assert_ne!(first, second);
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_contains(
                    first,
                    key.as_ptr(),
                    key.len() as i64,
                )
            },
            0
        );
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_contains(
                    second,
                    key.as_ptr(),
                    key.len() as i64,
                )
            },
            1
        );
        assert_eq!(
            super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size(second),
            1
        );
        super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop(first);
        super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop(second);
    }

    #[test]
    fn owned_get_profile_returns_present_zero_and_empty_string_as_success() {
        let first = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let with_zero = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32(
                first,
                b"zero".as_ptr(),
                4,
                0,
            )
        };
        let with_empty = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string(
                with_zero,
                b"empty".as_ptr(),
                5,
                b"".as_ptr(),
                0,
            )
        };

        let mut integer = 81;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_i32(
                    with_empty,
                    b"zero".as_ptr(),
                    4,
                    &mut integer,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_OK
        );
        assert_eq!(integer, 0);

        let mut text = std::ptr::null_mut();
        let mut length = -1;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                    with_empty,
                    b"empty".as_ptr(),
                    5,
                    &mut text,
                    &mut length,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_OK
        );
        assert!(
            !text.is_null(),
            "present empty String must own an allocation"
        );
        assert_eq!(length, 0);
        unsafe { crate::runtime::mimi_string_free(text) };

        // Each Set created a persistent branch, leaving its source readable.
        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(first),
            0
        );
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_contains(
                    with_zero,
                    b"empty".as_ptr(),
                    5,
                )
            },
            0
        );
        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(with_empty),
            2
        );

        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(first);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(with_zero);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(with_empty);
    }

    #[test]
    fn owned_get_profile_distinguishes_missing_from_type_mismatch() {
        let empty = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let with_number = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32(
                empty,
                b"number".as_ptr(),
                6,
                17,
            )
        };
        let root = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string(
                with_number,
                b"text".as_ptr(),
                4,
                b"value".as_ptr(),
                5,
            )
        };

        let mut integer = 99;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_i32(
                    root,
                    b"missing".as_ptr(),
                    7,
                    &mut integer,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_MISSING
        );
        assert_eq!(integer, 0);
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_i32(
                    root,
                    b"text".as_ptr(),
                    4,
                    &mut integer,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_TYPE_MISMATCH
        );
        assert_eq!(integer, 0);

        let mut text = std::ptr::dangling_mut::<std::ffi::c_char>();
        let mut length = 99;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                    root,
                    b"missing".as_ptr(),
                    7,
                    &mut text,
                    &mut length,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_MISSING
        );
        assert!(text.is_null());
        assert_eq!(length, 0);
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                    root,
                    b"number".as_ptr(),
                    6,
                    &mut text,
                    &mut length,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_TYPE_MISMATCH
        );
        assert!(text.is_null());
        assert_eq!(length, 0);

        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(root);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(with_number);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(empty);
    }

    #[test]
    fn owned_get_profile_reports_capacity_without_confusing_it_with_missing() {
        let root = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        super::begin_failure_injection(0);
        let mut integer = 73;
        let status = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_i32(
                root,
                b"key".as_ptr(),
                3,
                &mut integer,
            )
        };
        let staged = super::end_failure_injection();
        assert!(staged.is_empty());
        assert_eq!(status, super::ORDINARY_MAP_OWNED_GET_CAPACITY);
        assert_eq!(integer, 0);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(root);
    }

    #[test]
    fn owned_get_string_allocation_failures_clear_outputs_and_preserve_source() {
        let empty = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let key = "snapshot\0雪";
        let value = "kept\0🌿";
        let root = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string(
                empty,
                key.as_ptr(),
                key.len() as i64,
                value.as_ptr(),
                value.len() as i64,
            )
        };

        for stage in 0..3 {
            let mut text = std::ptr::dangling_mut();
            let mut length = 73;
            let status =
                super::with_ordinary_map_owned_get_allocation_failure_for_test(stage, || {
                    let status = unsafe {
                        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                            root,
                            key.as_ptr(),
                            key.len() as i64,
                            &mut text,
                            &mut length,
                        )
                    };
                    assert!(super::ordinary_map_owned_get_allocation_failure_triggered_for_test());
                    assert_eq!(
                        super::ordinary_map_owned_get_allocation_attempts_for_test(),
                        stage + 1
                    );
                    status
                });
            assert_eq!(
                status,
                super::ORDINARY_MAP_OWNED_GET_CAPACITY,
                "stage {stage}"
            );
            assert!(
                text.is_null(),
                "stage {stage} must not publish a partial owned payload"
            );
            assert_eq!(length, 0, "stage {stage} must clear the output length");
            assert_eq!(
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(root),
                1
            );
        }

        let mut text = std::ptr::null_mut();
        let mut length = -1;
        let status = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                root,
                key.as_ptr(),
                key.len() as i64,
                &mut text,
                &mut length,
            )
        };
        assert_eq!(status, super::ORDINARY_MAP_OWNED_GET_OK);
        assert!(!text.is_null());
        assert_eq!(length, value.len() as i64);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(empty);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(root);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length as usize) },
            value.as_bytes(),
            "the recovered String snapshot must outlive both roots"
        );
        unsafe { super::super::mimi_string_free(text) };
    }

    #[test]
    fn owned_get_profile_validates_root_before_reporting_key_capacity() {
        let owned_get = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let lifecycle = super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new();

        super::begin_failure_injection(0);
        let stale = unsafe { super::acquire_owned_get_key(i64::MAX, b"key".as_ptr(), 3) };
        let staged = super::end_failure_injection();
        assert!(staged.is_empty());
        assert!(matches!(
            stale,
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(
                super::super::handle::HandleError::Invalid
            ))
        ));

        super::begin_failure_injection(0);
        let wrong_mode = unsafe { super::acquire_owned_get_key(lifecycle, b"key".as_ptr(), 3) };
        let staged = super::end_failure_injection();
        assert!(staged.is_empty());
        assert!(matches!(
            wrong_mode,
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(
                super::super::handle::HandleError::Invalid
            ))
        ));

        super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop(lifecycle);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(owned_get);
    }

    #[test]
    fn owned_get_profile_keeps_dynamic_nul_and_unicode_keys_and_values() {
        let key = String::from("动态键\0雪🧭");
        let value = String::from("值\0Unicode🌿");
        let empty = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let root = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string(
                empty,
                key.as_ptr(),
                key.len() as i64,
                value.as_ptr(),
                value.len() as i64,
            )
        };
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_contains(
                    root,
                    key.as_ptr(),
                    key.len() as i64,
                )
            },
            1
        );

        let mut text = std::ptr::null_mut();
        let mut length = 0;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                    root,
                    key.as_ptr(),
                    key.len() as i64,
                    &mut text,
                    &mut length,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_OK
        );
        assert_eq!(length as usize, value.len());
        let output = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length as usize) };
        assert_eq!(output, value.as_bytes());
        unsafe { crate::runtime::mimi_string_free(text) };
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(root);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(empty);
    }

    #[test]
    fn owned_get_string_value_is_independent_after_its_source_root_is_dropped() {
        let initial = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let root = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string(
                initial,
                b"key".as_ptr(),
                3,
                b"owned\0value".as_ptr(),
                11,
            )
        };
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(initial);

        let mut text = std::ptr::null_mut();
        let mut length = 0;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                    root,
                    b"key".as_ptr(),
                    3,
                    &mut text,
                    &mut length,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_OK
        );
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(root);
        let output = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length as usize) };
        assert_eq!(output, b"owned\0value");
        unsafe { crate::runtime::mimi_string_free(text) };
    }

    #[test]
    fn owned_get_handles_reject_other_map_root_modes_and_stale_handles() {
        let owned_get = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let lifecycle = super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new();
        let any_owner =
            super::super::handle::map_new_handle(crate::runtime::MimiMap::canonical_any_owner_v1());
        let map_root = crate::runtime::mimi_mir_map_root_new();
        let legacy = crate::runtime::mimi_map_new();

        for other in [lifecycle, any_owner, map_root, legacy] {
            assert!(matches!(
                super::acquire_owned_get_root(other),
                Err(super::OrdinaryMapSnapshotRuntimeError::Handle(
                    super::super::handle::HandleError::Invalid
                ))
            ));
        }
        assert!(matches!(
            super::super::handle::ordinary_map_owner_lifecycle_i32_string_v1_acquire(owned_get),
            Err(super::super::handle::HandleError::Invalid)
        ));
        assert!(matches!(
            super::super::handle::map_any_owner_acquire(owned_get),
            Err(super::super::handle::HandleError::Invalid)
        ));
        assert!(matches!(
            super::super::handle::map_acquire(owned_get),
            Err(super::super::handle::HandleError::Invalid)
        ));

        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(owned_get);
        assert!(matches!(
            super::acquire_owned_get_root(owned_get),
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(_))
        ));
        assert!(matches!(
            super::acquire_owned_get_root(i64::MAX),
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(
                super::super::handle::HandleError::Invalid
            ))
        ));

        super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop(lifecycle);
        super::super::handle::map_any_owner_drop(any_owner).unwrap();
        crate::runtime::mimi_mir_map_root_drop(map_root);
        unsafe { crate::runtime::mimi_map_destroy(legacy) };
    }

    #[test]
    fn values_and_to_list_are_key_byte_sorted_independent_owned_snapshots() {
        let root = OrdinaryMapSnapshot::new()
            .set_string("零🌿", "")
            .unwrap()
            .set_i32("z", -17)
            .unwrap()
            .set_string("a\0", "值\0雪🧭")
            .unwrap()
            .set_i32("a", 0)
            .unwrap();
        let values = root.values().expect("owned values snapshot");
        let pairs = root.to_list().expect("owned key/value snapshot");
        let (present, get) = root.get("a\0").unwrap();
        let (zero_present, zero) = root.get("a").unwrap();
        let (missing_present, missing) = root.get("missing").unwrap();
        assert!(present && zero_present);
        assert!(!missing_present);
        assert_eq!(read_i32(&zero), 0);
        assert_eq!(read_i32(&missing), 0);
        assert_ne!(zero.token, missing.token);
        assert_eq!(
            pairs
                .iter()
                .map(|(key, _)| key.as_bytes())
                .collect::<Vec<_>>(),
            vec![
                b"a".as_slice(),
                b"a\0".as_slice(),
                b"z".as_slice(),
                "零🌿".as_bytes()
            ]
        );
        let keys = ["a", "a\0", "z", "零🌿"];
        let mut tokens = Vec::new();
        for (index, key) in keys.into_iter().enumerate() {
            let source = root.entries.get(key).unwrap();
            assert_ne!(values[index].token, source.token);
            assert_ne!(pairs[index].1.token, source.token);
            assert_ne!(values[index].token, pairs[index].1.token);
            assert_eq!(
                values[index].descriptor_identity(),
                source.descriptor_identity()
            );
            assert_eq!(
                pairs[index].1.descriptor_identity(),
                source.descriptor_identity()
            );
            assert_eq!(source.descriptor_identity().unwrap().version, 1);
            tokens.extend([values[index].token, pairs[index].1.token]);
        }
        let overwritten = root.set_i32("a\0", 81).unwrap();
        let removed = overwritten.remove("零🌿").unwrap();
        let source_tokens: Vec<_> = root.entries.values().map(|value| value.token).collect();
        drop(root);
        drop(overwritten);
        drop(removed);
        for token in source_tokens {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
        }
        for snapshot in [&values[0], &pairs[0].1] {
            assert_eq!(read_i32(snapshot), 0);
            assert_eq!(
                snapshot.copy_string(),
                Err(any_owner::AnyOwnerError::DescriptorMismatch)
            );
        }
        for snapshot in [&values[1], &pairs[1].1, &get] {
            assert_eq!(read_string(snapshot), "值\0雪🧭");
            assert_eq!(
                snapshot.read_i32(),
                Err(any_owner::AnyOwnerError::DescriptorMismatch)
            );
        }
        assert_eq!(read_i32(&values[2]), -17);
        assert_eq!(read_i32(&pairs[2].1), -17);
        assert_eq!(read_string(&values[3]), "");
        assert_eq!(read_string(&pairs[3].1), "");
        assert_eq!(read_i32(&zero), 0);
        assert_eq!(read_i32(&missing), 0);
        tokens.extend([get.token, zero.token, missing.token]);
        drop(values);
        drop(pairs);
        drop(get);
        drop(zero);
        drop(missing);
        for token in tokens {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
        }
    }

    #[test]
    fn values_ffi_publishes_independent_managed_any_list_owners() {
        let empty = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        assert_ne!(empty, 0);
        let with_number = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32(
                empty,
                b"b".as_ptr(),
                1,
                17,
            )
        };
        assert_ne!(with_number, 0);
        let root = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string(
                with_number,
                b"a".as_ptr(),
                1,
                b"owned\0text".as_ptr(),
                10,
            )
        };
        assert_ne!(root, 0);
        let source_number = profile_entry_token(root, true, "b");
        let source_text = profile_entry_token(root, true, "a");

        let list = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_values(root);
        assert!(!list.is_null());
        // The list is canonical ManagedAny storage, not a legacy string/map
        // list.  Length validates every generation-bearing owner token.
        assert_eq!(
            unsafe { super::super::mimi_mir_list_len_managed_any(list) },
            2
        );
        assert_eq!(
            unsafe { (*list).element_kind },
            super::super::ListElementKind::ManagedAny
        );
        let first = unsafe { *((*list).data as *const i64) };
        let second = unsafe { *((*list).data as *const i64).add(1) };
        // `a` sorts before `b`, so the String token precedes the i32 token.
        assert_ne!(first, source_text.wire_bits() as i64);
        assert_ne!(second, source_number.wire_bits() as i64);
        assert_eq!(
            any_owner::copy_string(any_owner::AnyOwnerToken::from_wire_bits(first as u64).unwrap()),
            Ok("owned\0text".to_owned())
        );
        assert_eq!(
            any_owner::read_i32(any_owner::AnyOwnerToken::from_wire_bits(second as u64).unwrap()),
            Ok(17)
        );
        let cloned_list = unsafe { super::super::mimi_mir_list_clone_managed_any(list) };
        assert!(!cloned_list.is_null());
        assert_eq!(
            unsafe { super::super::mimi_mir_list_len_managed_any(cloned_list) },
            2
        );
        let cloned_first = unsafe { *((*cloned_list).data as *const i64) };
        let cloned_second = unsafe { *((*cloned_list).data as *const i64).add(1) };
        assert_ne!(cloned_first, first);
        assert_ne!(cloned_second, second);

        // Destroying every source snapshot must not invalidate list elements;
        // each slot owns a fresh token and has its own creator-domain receipt.
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(empty);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(with_number);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(root);
        assert_eq!(
            any_owner::validate(source_number),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        assert_eq!(
            any_owner::validate(source_text),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        assert_eq!(
            any_owner::copy_string(any_owner::AnyOwnerToken::from_wire_bits(first as u64).unwrap()),
            Ok("owned\0text".to_owned())
        );

        unsafe { super::super::mimi_mir_list_drop_managed_any(list) };
        assert_eq!(
            any_owner::validate(any_owner::AnyOwnerToken::from_wire_bits(first as u64).unwrap()),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        assert_eq!(
            any_owner::copy_string(
                any_owner::AnyOwnerToken::from_wire_bits(cloned_first as u64).unwrap()
            ),
            Ok("owned\0text".to_owned())
        );
        assert_eq!(
            any_owner::read_i32(
                any_owner::AnyOwnerToken::from_wire_bits(cloned_second as u64).unwrap()
            ),
            Ok(17)
        );
        unsafe { super::super::mimi_mir_list_drop_managed_any(cloned_list) };
        assert_eq!(
            any_owner::validate(any_owner::AnyOwnerToken::from_wire_bits(second as u64).unwrap()),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        for cloned in [cloned_first, cloned_second] {
            assert_eq!(
                any_owner::validate(
                    any_owner::AnyOwnerToken::from_wire_bits(cloned as u64).unwrap()
                ),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
        }
    }

    #[test]
    fn values_ffi_staging_failures_retire_all_clones_and_keep_source_live() {
        let empty = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let first = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32(
                empty,
                b"i".as_ptr(),
                1,
                -4,
            )
        };
        let root = unsafe {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string(
                first,
                b"s".as_ptr(),
                1,
                b"text".as_ptr(),
                4,
            )
        };
        let source_i = profile_entry_token(root, true, "i");
        let source_s = profile_entry_token(root, true, "s");
        // Values staging has four injectable allocation checkpoints for this
        // two-element root: sorted entries, result vector, and one owner clone
        // per element. Every failing prefix must retire all staged clones.
        for failure_stage in 0..4 {
            super::begin_failure_injection(failure_stage);
            let result = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_values(root);
            let staged = super::end_failure_injection();
            assert!(result.is_null(), "stage {failure_stage} should fail closed");
            for token in staged {
                assert_eq!(
                    any_owner::validate(token),
                    Err(any_owner::AnyOwnerError::StaleToken),
                    "stage {failure_stage} leaked a staged owner"
                );
            }
            assert_eq!(any_owner::read_i32(source_i), Ok(-4));
            assert_eq!(any_owner::copy_string(source_s), Ok("text".to_owned()));
        }
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(empty);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(first);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(root);
    }

    #[test]
    fn every_snapshot_staging_failure_returns_to_the_source_owner_baseline() {
        let root = OrdinaryMapSnapshot::new()
            .set_i32("zero", 0)
            .unwrap()
            .set_string("text\0雪", "值\0🌿")
            .unwrap()
            .set_string("empty", "")
            .unwrap();
        let source_tokens: Vec<_> = root.entries.values().map(|value| value.token).collect();
        let source_descriptors: Vec<_> = source_tokens
            .iter()
            .map(|token| any_owner::descriptor_identity(*token).unwrap())
            .collect();
        // Each attempt tracks only tokens it allocated, avoiding global-count
        // races with other tests. Retiring all of them proves owner baseline
        // restoration; the original source tokens must stay unchanged.
        for (operation, success_stage) in [(0, 7), (1, 5), (2, 8), (3, 1), (4, 1)] {
            let mut succeeded = false;
            for fail_after in 0..=success_stage {
                super::begin_failure_injection(fail_after);
                let result = match operation {
                    0 => root.clone_root().map(drop),
                    1 => root.values().map(drop),
                    2 => root.to_list().map(drop),
                    3 => root.get("text\0雪").map(drop),
                    4 => root.get("missing").map(drop),
                    _ => unreachable!(),
                };
                let staged = super::end_failure_injection();
                if fail_after < success_stage {
                    assert!(matches!(
                        result,
                        Err(super::OrdinaryMapSnapshotError::Capacity)
                    ));
                } else {
                    result.expect("all allocation stages admitted");
                    succeeded = true;
                }
                for token in staged {
                    assert_eq!(
                        any_owner::validate(token),
                        Err(any_owner::AnyOwnerError::StaleToken),
                        "operation {operation}, stage {fail_after} left a staged owner live"
                    );
                }
                for (token, descriptor) in source_tokens.iter().zip(&source_descriptors) {
                    assert_eq!(any_owner::descriptor_identity(*token), Ok(*descriptor));
                }
                assert_eq!(root.size(), 3);
                assert_eq!(read_root_string(&root, "text\0雪"), "值\0🌿");
            }
            assert!(succeeded);
        }
        drop(root);
        for token in source_tokens {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
        }
    }

    fn profile_entry_token(handle: i64, owned_get: bool, key: &str) -> any_owner::AnyOwnerToken {
        let root = if owned_get {
            super::acquire_owned_get_root(handle)
        } else {
            super::acquire_root(handle)
        }
        .expect("exact profile root");
        super::snapshot_from_map(&root)
            .unwrap()
            .entries
            .get(key)
            .unwrap()
            .token
    }

    fn clone_profile_root(
        handle: i64,
        owned_get: bool,
    ) -> Result<i64, super::OrdinaryMapSnapshotRuntimeError> {
        if owned_get {
            super::clone_owned_get_root(handle)
        } else {
            super::clone_root_handle(handle)
        }
    }

    fn drop_profile_root(handle: i64, owned_get: bool) {
        if owned_get {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(handle);
        } else {
            super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop(handle);
        }
    }

    fn mixed_profile_root(owned_get: bool) -> i64 {
        let empty = if owned_get {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new()
        } else {
            super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new()
        };
        let integer = if owned_get {
            super::derive_owned_get_root(empty, |root| root.set_i32("n", 0))
        } else {
            super::set_i32_root(empty, "n", 0)
        }
        .unwrap();
        let mixed = if owned_get {
            super::derive_owned_get_root(integer, |root| root.set_string("text\0雪", "值\0🧭"))
        } else {
            super::set_string_root(integer, "text\0雪", "值\0🧭")
        }
        .unwrap();
        drop_profile_root(empty, owned_get);
        drop_profile_root(integer, owned_get);
        mixed
    }

    #[test]
    fn runtime_clone_abis_keep_fresh_mixed_owners_in_both_destruction_orders() {
        for owned_get in [false, true] {
            for source_first in [false, true] {
                let source = mixed_profile_root(owned_get);
                let cloned = if owned_get {
                    super::mimi_mir_ordinary_map_owned_get_i32_string_v1_clone(source)
                } else {
                    super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_clone(source)
                };
                assert_ne!(source, cloned);
                let source_number = profile_entry_token(source, owned_get, "n");
                let source_text = profile_entry_token(source, owned_get, "text\0雪");
                let cloned_number = profile_entry_token(cloned, owned_get, "n");
                let cloned_text = profile_entry_token(cloned, owned_get, "text\0雪");
                assert_ne!(source_number, cloned_number);
                assert_ne!(source_text, cloned_text);
                assert_eq!(
                    any_owner::descriptor_identity(source_number),
                    any_owner::descriptor_identity(cloned_number)
                );
                assert_eq!(
                    any_owner::descriptor_identity(source_text),
                    any_owner::descriptor_identity(cloned_text)
                );
                let (first, second, live_number, live_text) = if source_first {
                    (source, cloned, cloned_number, cloned_text)
                } else {
                    (cloned, source, source_number, source_text)
                };
                drop_profile_root(first, owned_get);
                assert_eq!(any_owner::read_i32(live_number), Ok(0));
                assert_eq!(any_owner::copy_string(live_text), Ok("值\0🧭".to_owned()));
                drop_profile_root(second, owned_get);
                for token in [source_number, source_text, cloned_number, cloned_text] {
                    assert_eq!(
                        any_owner::validate(token),
                        Err(any_owner::AnyOwnerError::StaleToken)
                    );
                }
            }
        }
    }

    #[test]
    fn runtime_clone_staging_failures_retire_every_owner_without_publishing_a_root() {
        for owned_get in [false, true] {
            let source = mixed_profile_root(owned_get);
            let original_number = profile_entry_token(source, owned_get, "n");
            let original_text = profile_entry_token(source, owned_get, "text\0雪");
            // Table reserve + two key/owner copies + final root publication.
            for fail_after in 0..=6 {
                super::begin_failure_injection(fail_after);
                let result = clone_profile_root(source, owned_get);
                let staged = super::end_failure_injection();
                if fail_after < 6 {
                    assert_eq!(
                        result,
                        Err(super::OrdinaryMapSnapshotRuntimeError::Capacity)
                    );
                    if fail_after == 5 {
                        assert_eq!(staged.len(), 2, "publication failure stages both owners");
                    }
                } else {
                    let cloned = result.expect("clone succeeds after all staging checkpoints");
                    assert_ne!(source, cloned);
                    assert_eq!(staged.len(), 2);
                    drop_profile_root(cloned, owned_get);
                }
                for token in staged {
                    assert_eq!(
                        any_owner::validate(token),
                        Err(any_owner::AnyOwnerError::StaleToken)
                    );
                }
                assert_eq!(any_owner::read_i32(original_number), Ok(0));
                assert_eq!(
                    any_owner::copy_string(original_text),
                    Ok("值\0🧭".to_owned())
                );
            }
            drop_profile_root(source, owned_get);
            assert_eq!(
                any_owner::validate(original_number),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(
                any_owner::validate(original_text),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
        }
    }

    #[test]
    fn runtime_clone_validates_namespace_and_generation_before_allocation() {
        let lifecycle = mixed_profile_root(false);
        let owned_get = mixed_profile_root(true);
        let legacy = crate::runtime::mimi_map_new();
        let map_root = crate::runtime::mimi_mir_map_root_new();
        let owner_v1 =
            super::super::handle::map_new_handle(crate::runtime::MimiMap::canonical_any_owner_v1());
        let value = profile_entry_token(lifecycle, false, "n");
        let stale = super::new_empty_root().unwrap();
        super::drop_root(stale).unwrap();
        for profile in [false, true] {
            let wrong_profile = if profile { lifecycle } else { owned_get };
            for invalid in [
                wrong_profile,
                legacy,
                map_root,
                owner_v1,
                value.wire_bits() as i64,
                stale,
                i64::MAX,
            ] {
                super::begin_failure_injection(0);
                let result = clone_profile_root(invalid, profile);
                let staged = super::end_failure_injection();
                assert!(staged.is_empty());
                assert!(matches!(
                    result,
                    Err(super::OrdinaryMapSnapshotRuntimeError::Handle(_))
                ));
            }
        }
        assert_eq!(
            any_owner::AnyOwnerToken::from_wire_bits(lifecycle as u64),
            Err(any_owner::AnyOwnerError::InvalidToken),
            "Map roots cannot be interpreted as Any owner tokens"
        );
        drop_profile_root(lifecycle, false);
        drop_profile_root(owned_get, true);
        unsafe { crate::runtime::mimi_map_destroy(legacy) };
        crate::runtime::mimi_mir_map_root_drop(map_root);
        super::super::handle::map_any_owner_drop(owner_v1).unwrap();
    }

    fn profile_set_i32_abi(handle: i64, owned_get: bool, key: &[u8], value: i32) -> i64 {
        unsafe {
            if owned_get {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32(
                    handle,
                    key.as_ptr(),
                    key.len() as i64,
                    value,
                )
            } else {
                super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_i32(
                    handle,
                    key.as_ptr(),
                    key.len() as i64,
                    value,
                )
            }
        }
    }

    fn profile_set_string_abi(handle: i64, owned_get: bool, key: &[u8], value: &[u8]) -> i64 {
        unsafe {
            if owned_get {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string(
                    handle,
                    key.as_ptr(),
                    key.len() as i64,
                    value.as_ptr(),
                    value.len() as i64,
                )
            } else {
                super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_string(
                    handle,
                    key.as_ptr(),
                    key.len() as i64,
                    value.as_ptr(),
                    value.len() as i64,
                )
            }
        }
    }

    fn profile_remove_abi(handle: i64, owned_get: bool, key: &[u8]) -> i64 {
        unsafe {
            if owned_get {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_remove(
                    handle,
                    key.as_ptr(),
                    key.len() as i64,
                )
            } else {
                super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_remove(
                    handle,
                    key.as_ptr(),
                    key.len() as i64,
                )
            }
        }
    }

    fn profile_size_abi(handle: i64, owned_get: bool) -> i32 {
        if owned_get {
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(handle)
        } else {
            super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size(handle)
        }
    }

    fn profile_contains_abi(handle: i64, owned_get: bool, key: &[u8]) -> i32 {
        unsafe {
            if owned_get {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_contains(
                    handle,
                    key.as_ptr(),
                    key.len() as i64,
                )
            } else {
                super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_contains(
                    handle,
                    key.as_ptr(),
                    key.len() as i64,
                )
            }
        }
    }

    #[test]
    fn runtime_error_sentinels_release_staging_and_preserve_source_owners() {
        for owned_get in [false, true] {
            let source = mixed_profile_root(owned_get);
            let integer = profile_entry_token(source, owned_get, "n");
            let string = profile_entry_token(source, owned_get, "text\0雪");
            for (operation, success_stage) in [(0, 9), (1, 9), (2, 7)] {
                for fail_after in 0..=success_stage {
                    super::begin_failure_injection(fail_after);
                    let result = match operation {
                        0 => profile_set_i32_abi(source, owned_get, b"new", 41),
                        1 => profile_set_string_abi(source, owned_get, b"new", "新\0🌿".as_bytes()),
                        2 => profile_remove_abi(source, owned_get, b"missing"),
                        _ => unreachable!(),
                    };
                    let staged = super::end_failure_injection();
                    if fail_after < success_stage {
                        assert_eq!(result, 0, "operation {operation}, stage {fail_after}");
                    } else {
                        assert_ne!(result, 0);
                        drop_profile_root(result, owned_get);
                    }
                    for token in staged {
                        assert_eq!(
                            any_owner::validate(token),
                            Err(any_owner::AnyOwnerError::StaleToken)
                        );
                    }
                    assert_eq!(profile_size_abi(source, owned_get), 2);
                    assert_eq!(profile_contains_abi(source, owned_get, b"new"), 0);
                    assert_eq!(any_owner::read_i32(integer), Ok(0));
                    assert_eq!(any_owner::copy_string(string), Ok("值\0🧭".to_owned()));
                }
            }
            assert_eq!(profile_set_i32_abi(source, owned_get, &[0xff], 7), 0);
            assert_eq!(
                profile_set_string_abi(source, owned_get, b"new", &[0xff]),
                0
            );
            assert_eq!(profile_remove_abi(source, owned_get, &[0xff]), 0);
            assert_eq!(profile_contains_abi(source, owned_get, &[0xff]), -1);
            super::begin_failure_injection(0);
            let contains = profile_contains_abi(source, owned_get, b"n");
            let staged = super::end_failure_injection();
            assert_eq!(contains, -1);
            assert!(staged.is_empty());
            assert_eq!(profile_contains_abi(source, owned_get, b"n"), 1);
            assert_eq!(profile_contains_abi(source, owned_get, b"missing"), 0);
            drop_profile_root(source, owned_get);
            assert_eq!(profile_size_abi(source, owned_get), -1);
            assert_eq!(profile_contains_abi(source, owned_get, b"n"), -1);
            assert_eq!(profile_set_i32_abi(source, owned_get, b"new", 7), 0);
            assert_eq!(
                profile_set_string_abi(source, owned_get, b"new", b"value"),
                0
            );
            assert_eq!(profile_remove_abi(source, owned_get, b"n"), 0);
        }
    }

    #[test]
    fn runtime_registration_failure_rolls_back_new_set_remove_and_clone_roots() {
        for owned_get in [false, true] {
            let source = mixed_profile_root(owned_get);
            let integer = profile_entry_token(source, owned_get, "n");
            let string = profile_entry_token(source, owned_get, "text\0雪");
            for operation in 0..5 {
                // Track all temporary owners, but let every snapshot allocation
                // pass so the domain fails only after the Map handle is installed.
                super::begin_failure_injection(usize::MAX);
                let result = super::super::ordinary_map_domain::with_registration_failure_for_test(
                    || match operation {
                        0 => {
                            if owned_get {
                                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new()
                            } else {
                                super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new()
                            }
                        }
                        1 => profile_set_i32_abi(source, owned_get, b"new", 17),
                        2 => profile_set_string_abi(source, owned_get, b"new", "值\0雪".as_bytes()),
                        3 => profile_remove_abi(source, owned_get, b"missing"),
                        4 => {
                            if owned_get {
                                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_clone(source)
                            } else {
                                super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_clone(
                                    source,
                                )
                            }
                        }
                        _ => unreachable!(),
                    },
                );
                let staged = super::end_failure_injection();
                assert_eq!(
                    result, 0,
                    "operation {operation} must not publish an unregistered root"
                );
                for token in staged {
                    assert_eq!(
                        any_owner::validate(token),
                        Err(any_owner::AnyOwnerError::StaleToken)
                    );
                }
                assert_eq!(profile_size_abi(source, owned_get), 2);
                assert_eq!(any_owner::read_i32(integer), Ok(0));
                assert_eq!(any_owner::copy_string(string), Ok("值\0🧭".to_owned()));
            }
            drop_profile_root(source, owned_get);
        }
    }
}

#[cfg(test)]
mod managed_any_tests {
    use super::*;

    fn root() -> i64 {
        new_owned_get_root(
            OrdinaryMapSnapshot::new()
                .set_i32("zero", 0)
                .unwrap()
                .set_string("空\0键", "")
                .unwrap()
                .set_string("text", "值\0雪🧪")
                .unwrap(),
        )
        .unwrap()
    }

    fn get(root: i64, key: &str) -> (i32, i64) {
        let (mut found, mut owner) = (-1, 0);
        assert_eq!(
            unsafe {
                mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                    root,
                    key.as_ptr(),
                    key.len() as i64,
                    &mut found,
                    &mut owner,
                )
            },
            0
        );
        assert_ne!(owner, 0);
        (found, owner)
    }

    fn copy(owner: i64) -> (*mut std::ffi::c_char, i64) {
        let (mut pointer, mut length) = (std::ptr::null_mut(), -1);
        assert_eq!(
            unsafe {
                mimi_mir_managed_any_i32_string_v1_copy_string(
                    owner,
                    2,
                    1,
                    &mut pointer,
                    &mut length,
                )
            },
            0
        );
        assert!(!pointer.is_null());
        (pointer, length)
    }

    #[test]
    fn checked_i32_pack_publishes_one_canonical_owned_any() {
        let mut owner = 77;
        assert_eq!(
            unsafe { mimi_mir_managed_any_i32_string_v1_pack_i32(-17, &mut owner) },
            0
        );
        assert_ne!(owner, 0);
        let mut value = 0;
        assert_eq!(
            unsafe { mimi_mir_managed_any_i32_string_v1_read_i32(owner, 1, 1, &mut value) },
            0
        );
        assert_eq!(value, -17);
        assert_eq!(mimi_mir_managed_any_i32_string_v1_drop(owner, 1), 0);
    }

    #[test]
    fn ordinary_get_any_missing_zero_empty_and_snapshots_have_independent_owners() {
        let source = root();
        let missing = get(source, "absent");
        let present_zero = get(source, "zero");
        assert_eq!((missing.0, present_zero.0), (0, 1));
        assert_ne!(missing.1, present_zero.1);
        for owner in [missing.1, present_zero.1] {
            let mut value = -1;
            assert_eq!(
                unsafe { mimi_mir_managed_any_i32_string_v1_read_i32(owner, 1, 1, &mut value) },
                0
            );
            assert_eq!(value, 0);
        }
        let empty = get(source, "空\0键");
        assert_eq!(empty.0, 1);
        let (empty_ptr, empty_len) = copy(empty.1);
        assert_eq!(empty_len, 0);
        unsafe { super::super::mimi_string_free(empty_ptr) };
        let text = get(source, "text");
        let mut cloned = 0;
        assert_eq!(
            unsafe { mimi_mir_managed_any_i32_string_v1_clone(text.1, 1, &mut cloned) },
            0
        );
        assert_ne!(cloned, text.1);
        let updated =
            derive_owned_get_root(source, |snapshot| snapshot.set_string("text", "changed"))
                .unwrap();
        let removed = derive_owned_get_root(updated, |snapshot| snapshot.remove("text")).unwrap();
        for root in [source, updated, removed] {
            drop_owned_get_root(root).unwrap();
        }
        assert_eq!(mimi_mir_managed_any_i32_string_v1_drop(text.1, 1), 0);
        let (pointer, length) = copy(cloned);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) },
            "值\0雪🧪".as_bytes()
        );
        assert_eq!(mimi_mir_managed_any_i32_string_v1_drop(cloned, 1), 0);
        // The canonical copied String survives release of every Any and Map.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) },
            "值\0雪🧪".as_bytes()
        );
        unsafe { super::super::mimi_string_free(pointer) };
        for owner in [missing.1, present_zero.1, empty.1] {
            assert_eq!(mimi_mir_managed_any_i32_string_v1_drop(owner, 1), 0);
        }
    }

    #[test]
    fn managed_any_abi_failures_clear_outputs_and_preserve_borrowed_source() {
        let source = root();
        let owner = get(source, "text").1;
        let mut value = 77;
        assert_eq!(
            unsafe { mimi_mir_managed_any_i32_string_v1_read_i32(owner, 1, 1, &mut value) },
            2
        );
        assert_eq!(value, 0);
        let (mut pointer, mut length) = (std::ptr::dangling_mut(), 77);
        assert_eq!(
            unsafe {
                mimi_mir_managed_any_i32_string_v1_copy_string(
                    owner,
                    1,
                    1,
                    &mut pointer,
                    &mut length,
                )
            },
            2
        );
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert_eq!(
            unsafe {
                mimi_mir_managed_any_i32_string_v1_copy_string(
                    owner,
                    2,
                    2,
                    &mut pointer,
                    &mut length,
                )
            },
            6
        );
        let mut clone = 77;
        assert_eq!(
            unsafe { mimi_mir_managed_any_i32_string_v1_clone(owner, 2, &mut clone) },
            6
        );
        assert_eq!(clone, 0);
        assert_eq!(mimi_mir_managed_any_i32_string_v1_drop(owner, 2), 6);
        let raw = any_owner::allocate_i32(7).unwrap();
        assert_eq!(
            unsafe {
                mimi_mir_managed_any_i32_string_v1_read_i32(
                    raw.wire_bits() as i64,
                    1,
                    1,
                    &mut value,
                )
            },
            9
        );
        any_owner::release(raw).unwrap();
        assert_eq!(
            unsafe { mimi_mir_managed_any_i32_string_v1_read_i32(0, 1, 1, &mut value) },
            4
        );
        let (pointer, _) = copy(owner);
        unsafe { super::super::mimi_string_free(pointer) };
        assert_eq!(mimi_mir_managed_any_i32_string_v1_drop(owner, 1), 0);
        assert_eq!(mimi_mir_managed_any_i32_string_v1_drop(owner, 1), 5);
        assert_eq!(
            unsafe { mimi_mir_managed_any_i32_string_v1_read_i32(owner, 1, 1, &mut value) },
            5
        );
        drop_owned_get_root(source).unwrap();
    }

    #[test]
    fn ordinary_get_any_rejects_wrong_root_stale_reentrant_and_invalid_inputs_without_publication()
    {
        let wrong = new_empty_root().unwrap();
        let source = root();
        let (mut found, mut owner) = (77, 77);
        assert_eq!(
            unsafe {
                mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                    wrong,
                    b"text".as_ptr(),
                    4,
                    &mut found,
                    &mut owner,
                )
            },
            10
        );
        assert_eq!((found, owner), (0, 0));
        drop_root(wrong).unwrap();
        let lease = acquire_owned_get_root(source).unwrap();
        assert_eq!(
            unsafe {
                mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                    source,
                    b"text".as_ptr(),
                    4,
                    &mut found,
                    &mut owner,
                )
            },
            12
        );
        drop(lease);
        assert_eq!(
            unsafe {
                mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                    source,
                    [0xff].as_ptr(),
                    1,
                    &mut found,
                    &mut owner,
                )
            },
            1
        );
        owner = 77;
        assert_eq!(
            unsafe {
                mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                    source,
                    b"text".as_ptr(),
                    4,
                    std::ptr::null_mut(),
                    &mut owner,
                )
            },
            1
        );
        assert_eq!(owner, 0);
        drop_owned_get_root(source).unwrap();
        // A destroyed root remains StaleGeneration (11) whether its slot is
        // retained or the empty table compacts it; generation_floor preserves
        // the stale identity across both cases.
        assert_eq!(
            unsafe {
                mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                    source,
                    b"text".as_ptr(),
                    4,
                    &mut found,
                    &mut owner,
                )
            },
            11
        );
        assert_eq!((found, owner), (0, 0));
        // A forged wire handle outside the encodable Map index range remains
        // RootInvalid (10), distinct from a real root whose lifetime ended.
        found = 77;
        owner = 77;
        assert_eq!(
            unsafe {
                mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                    i64::MAX,
                    b"text".as_ptr(),
                    4,
                    &mut found,
                    &mut owner,
                )
            },
            10
        );
        assert_eq!((found, owner), (0, 0));
    }

    #[test]
    fn trap_cleanup_ignores_stale_entry_secondary_error_and_restores_cleanup_scope() {
        let source = root();
        let lease = acquire_owned_get_root(source).unwrap();
        let token = snapshot_from_map(&lease)
            .unwrap()
            .entries
            .get("zero")
            .unwrap()
            .token;
        drop(lease);
        // Deliberately corrupt a test-only entry's owner obligation to reach
        // the old RAII-abort branch through a real raw root trap retirement.
        any_owner::release(token).unwrap();
        let text = get(source, "text").1;
        assert!(!super::super::ordinary_map_domain::in_trap_cleanup());
        super::super::ordinary_map_domain::drain_current_thread_roots();
        assert!(!super::super::ordinary_map_domain::in_trap_cleanup());
        assert!(acquire_owned_get_root(source).is_err());
        assert_eq!(mimi_mir_managed_any_i32_string_v1_drop(text, 1), 5);
    }

    #[test]
    fn managed_any_allocation_and_registration_failures_roll_back_fresh_owner_and_string() {
        let source = root();
        let owner = get(source, "text").1;
        for stage in 0..2 {
            let (mut found, mut fresh) = (77, 77);
            with_ordinary_map_owned_get_allocation_failure_for_test(stage, || {
                assert_eq!(
                    unsafe {
                        mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                            source,
                            b"text".as_ptr(),
                            4,
                            &mut found,
                            &mut fresh,
                        )
                    },
                    3
                );
                assert!(ordinary_map_owned_get_allocation_failure_triggered_for_test());
            });
            assert_eq!((found, fresh), (0, 0));
            let (mut pointer, mut length) = (std::ptr::dangling_mut(), 77);
            with_ordinary_map_owned_get_allocation_failure_for_test(stage, || {
                assert_eq!(
                    unsafe {
                        mimi_mir_managed_any_i32_string_v1_copy_string(
                            owner,
                            2,
                            1,
                            &mut pointer,
                            &mut length,
                        )
                    },
                    3
                );
            });
            assert!(pointer.is_null());
            assert_eq!(length, 0);
        }
        let mut clone = 77;
        with_ordinary_map_owned_get_allocation_failure_for_test(0, || {
            assert_eq!(
                unsafe { mimi_mir_managed_any_i32_string_v1_clone(owner, 1, &mut clone) },
                3
            );
            // An invalid checked receipt wins over injected allocation failure.
            assert_eq!(
                unsafe { mimi_mir_managed_any_i32_string_v1_clone(owner, 2, &mut clone) },
                6
            );
        });
        assert_eq!(clone, 0);
        begin_failure_injection(usize::MAX);
        super::super::ordinary_map_domain::with_registration_failure_for_test(|| {
            let (mut found, mut fresh) = (77, 77);
            assert_eq!(
                unsafe {
                    mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                        source,
                        b"text".as_ptr(),
                        4,
                        &mut found,
                        &mut fresh,
                    )
                },
                3
            );
            assert_eq!((found, fresh), (0, 0));
            assert_eq!(
                unsafe { mimi_mir_managed_any_i32_string_v1_clone(owner, 1, &mut clone) },
                3
            );
            let (mut pointer, mut length) = (std::ptr::dangling_mut(), 77);
            assert_eq!(
                unsafe {
                    mimi_mir_managed_any_i32_string_v1_copy_string(
                        owner,
                        2,
                        1,
                        &mut pointer,
                        &mut length,
                    )
                },
                3
            );
            assert!(pointer.is_null());
            assert_eq!(length, 0);
        });
        let staged = end_failure_injection();
        assert_eq!(staged.len(), 2);
        for token in staged {
            assert_eq!(any_owner::validate(token), Err(AnyOwnerError::StaleToken));
        }
        let (pointer, _) = copy(owner);
        unsafe { super::super::mimi_string_free(pointer) };
        assert_eq!(mimi_mir_managed_any_i32_string_v1_drop(owner, 1), 0);
        drop_owned_get_root(source).unwrap();
    }
}
