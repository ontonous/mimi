//! Persistent ordinary-map snapshots whose entries own typed `Any` values.
//!
//! This module contains the existing distinct v1 profiles and the staged
//! `ordinary_map_owned_any_v2` profile. The v2 snapshot is isolated by its
//! MapStorageMode and stores AnyOwnerTokens only in private non-copyable RAII
//! values; none of these tokens are legacy ValueHandles.
//!
//! The V2 ordinary-Map ABI uses status/out for every fallible operation.
//! Status zero means success; every nonzero status is an error handled by the
//! compiler-native trap adapter. V1 exports retain their historical ABI.
//! Rust staging locals are dropped before an error status is returned.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(test)]
use std::cell::{Cell, RefCell};

use super::any_owner::{self, AnyOwnerError, AnyOwnerKind, AnyOwnerReceiptError, AnyOwnerToken};
use super::managed_any_payload_v2::{
    ManagedAnyPayloadV2, ManagedAnyPayloadV2Glue, ManagedAnyPayloadV2Identity,
};

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

/// One unpublished or Map-entry V2 owner. The token always comes from the
/// shared generation-bearing any_owner table; this is not a second token set.
pub(super) struct OwnedAnyV2 {
    token: AnyOwnerToken,
}

impl OwnedAnyV2 {
    fn from_payload(payload: ManagedAnyPayloadV2) -> Result<Self, AnyOwnerError> {
        snapshot_allocation_checkpoint().map_err(|_| AnyOwnerError::Capacity)?;
        let token = any_owner::allocate_managed_v2(payload)?;
        record_created_token(token);
        Ok(Self { token })
    }

    fn from_borrowed(
        identity: ManagedAnyPayloadV2Identity,
        glue: ManagedAnyPayloadV2Glue,
        source: *const u8,
    ) -> Result<Self, AnyOwnerError> {
        snapshot_allocation_checkpoint().map_err(|_| AnyOwnerError::Capacity)?;
        // SAFETY: FFI callers provide a readable pointer matching this exact
        // TypeDesc layout. The V2 owner module validates size/alignment and
        // clones to private storage before taking ownership.
        let payload = unsafe { ManagedAnyPayloadV2::clone_from_borrowed(identity, glue, source) }
            .map_err(|error| match error {
            super::managed_any_payload_v2::ManagedAnyPayloadV2Error::Capacity => {
                AnyOwnerError::Capacity
            }
            super::managed_any_payload_v2::ManagedAnyPayloadV2Error::Poisoned => {
                AnyOwnerError::Poisoned
            }
            super::managed_any_payload_v2::ManagedAnyPayloadV2Error::UnsupportedAggregate => {
                AnyOwnerError::UnsupportedAggregate
            }
            super::managed_any_payload_v2::ManagedAnyPayloadV2Error::CallbackConflict => {
                AnyOwnerError::CallbackConflict
            }
            super::managed_any_payload_v2::ManagedAnyPayloadV2Error::CloneFailed(status) => {
                AnyOwnerError::CloneFailed(status)
            }
            _ => AnyOwnerError::DescriptorMismatch,
        })?;
        Self::from_payload(payload)
    }

    fn clone_published_owner(
        owner: i64,
        expected: ManagedAnyPayloadV2Identity,
    ) -> Result<Self, ManagedAnyStatus> {
        let (source, actual) = managed_v2_token_receipt(owner)?;
        if actual != expected {
            return Err(ManagedAnyStatus::TypeMismatch);
        }
        managed_allocation_checkpoint()?;
        let token = any_owner::clone_managed_v2_owner(source).map_err(ManagedAnyStatus::from)?;
        record_created_token(token);
        Ok(Self { token })
    }

    fn identity(&self) -> Result<ManagedAnyPayloadV2Identity, AnyOwnerError> {
        any_owner::managed_v2_identity(self.token)
    }

    fn try_clone(&self) -> Result<Self, AnyOwnerError> {
        snapshot_allocation_checkpoint().map_err(|_| AnyOwnerError::Capacity)?;
        let token = any_owner::clone_managed_v2_owner(self.token)?;
        record_created_token(token);
        Ok(Self { token })
    }

    /// Transfer a token into the creator cleanup domain only once its exact
    /// actual descriptor receipt has been recorded successfully.
    fn publish_managed(self) -> Result<i64, ManagedAnyStatus> {
        let descriptor = self.identity().map_err(ManagedAnyStatus::from)?;
        super::ordinary_map_domain::register_fresh_managed_any_v2(self.token, descriptor)
            .map_err(ManagedAnyStatus::from)?;
        let accepted = std::mem::ManuallyDrop::new(self);
        Ok(accepted.token.wire_bits() as i64)
    }
}

impl Drop for OwnedAnyV2 {
    fn drop(&mut self) {
        if any_owner::release(self.token).is_err() && !super::ordinary_map_domain::in_trap_cleanup()
        {
            std::process::abort();
        }
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

/// An immutable legacy v1 root containing owned `i32` and `String` values.
///
/// Keys are Rust UTF-8 strings, hashed over their full byte contents. Embedded
/// NUL bytes are valid and remain part of the key.
pub(crate) struct OrdinaryMapSnapshot {
    entries: HashMap<String, OwnedAny>,
}

/// Unified version-2 ordinary Map storage. Each entry owns a token from the
/// shared Any registry and therefore preserves its actual tag, canonical type
/// ID, TypeDesc closure hash, layout, and descriptor-pinned callbacks.
pub(crate) struct OrdinaryMapAnyV2Snapshot {
    entries: HashMap<String, OwnedAnyV2>,
}

impl OrdinaryMapAnyV2Snapshot {
    pub(super) fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub(super) fn size(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Set by cloning a descriptor-checked published owner into the persistent
    /// snapshot. The caller retains its source owner and this root owns a
    /// separate token/payload instance.
    fn set_owned(&self, key: &str, value: OwnedAnyV2) -> Result<Self, OrdinaryMapSnapshotError> {
        let owned_key = try_copy_string(key)?;
        self.with_entry(owned_key, value)
    }

    /// Return an independent hit snapshot. Missing stays `None`; the FFI
    /// adapter creates the mandated i32-zero owner from its sealed core
    /// descriptor receipt and compiler-generated scalar glue.
    fn get_any(&self, key: &str) -> Result<Option<OwnedAnyV2>, OrdinaryMapSnapshotError> {
        self.entries
            .get(key)
            .map(OwnedAnyV2::try_clone)
            .transpose()
            .map_err(Into::into)
    }

    fn clone_root(&self) -> Result<Self, OrdinaryMapSnapshotError> {
        let mut entries = HashMap::new();
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve(self.entries.len())
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        for (key, value) in &self.entries {
            let key = try_copy_string(key)?;
            let value = value.try_clone()?;
            entries.insert(key, value);
        }
        Ok(Self { entries })
    }

    fn remove(&self, key: &str) -> Result<Self, OrdinaryMapSnapshotError> {
        let mut entries = HashMap::new();
        let final_len = self
            .entries
            .len()
            .checked_sub(usize::from(self.entries.contains_key(key)))
            .ok_or(OrdinaryMapSnapshotError::Capacity)?;
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve(final_len)
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        for (old_key, old_value) in &self.entries {
            if old_key == key {
                continue;
            }
            entries.insert(try_copy_string(old_key)?, old_value.try_clone()?);
        }
        Ok(Self { entries })
    }

    fn values(&self) -> Result<Vec<OwnedAnyV2>, OrdinaryMapSnapshotError> {
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

    fn to_list(&self) -> Result<Vec<(String, OwnedAnyV2)>, OrdinaryMapSnapshotError> {
        let ordered = self.sorted_entries()?;
        let mut entries = Vec::new();
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve_exact(ordered.len())
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        for (key, value) in ordered {
            entries.push((try_copy_string(key)?, value.try_clone()?));
        }
        Ok(entries)
    }

    fn sorted_entries(&self) -> Result<Vec<(&String, &OwnedAnyV2)>, OrdinaryMapSnapshotError> {
        let mut entries = Vec::new();
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve_exact(self.entries.len())
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        entries.extend(self.entries.iter());
        entries.sort_unstable_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
        Ok(entries)
    }

    fn size_i32(&self) -> Result<i32, OrdinaryMapSnapshotError> {
        i32::try_from(self.entries.len()).map_err(|_| OrdinaryMapSnapshotError::Capacity)
    }

    fn with_entry(&self, key: String, value: OwnedAnyV2) -> Result<Self, OrdinaryMapSnapshotError> {
        let replaces_existing = self.entries.contains_key(key.as_str());
        let final_len = self
            .entries
            .len()
            .checked_add(usize::from(!replaces_existing))
            .ok_or(OrdinaryMapSnapshotError::Capacity)?;
        let mut entries = HashMap::new();
        snapshot_allocation_checkpoint()?;
        entries
            .try_reserve(final_len)
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        for (old_key, old_value) in &self.entries {
            if old_key == key.as_str() {
                continue;
            }
            entries.insert(try_copy_string(old_key)?, old_value.try_clone()?);
        }
        entries.insert(key, value);
        Ok(Self { entries })
    }
}

/// An independently owned, immutable `values` result.  Its wire identity is
/// deliberately a separate opaque handle: it is neither a `MimiList` with
/// V1 `ManagedAny` slots nor an Any owner token.  Indexing clones one element
/// into a separately published ManagedAny V2 owner.
struct OrdinaryMapAnyV2ValuesSnapshot {
    values: Vec<OwnedAnyV2>,
}

impl OrdinaryMapAnyV2ValuesSnapshot {
    fn try_clone(&self) -> Result<Self, OrdinaryMapSnapshotError> {
        let mut values = Vec::new();
        snapshot_allocation_checkpoint()?;
        values
            .try_reserve_exact(self.values.len())
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        for value in &self.values {
            values.push(value.try_clone()?);
        }
        Ok(Self { values })
    }
}

/// Owner domains that can appear in Mimi lists. The V2 owner-list registry
/// deliberately recognizes only its canonical V2 domain; a V1 list or an
/// unrelated list allocation cannot borrow the same pointer receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManagedAnyV2ListDomain {
    CanonicalOwnerListV2,
    LegacyManagedAnyV1,
}

/// Creator-thread cleanup receipt for a canonical `MimiList<ManagedAnyV2>`.
/// The list owns both its Box header/backing buffer and its element tokens;
/// tracking only the tokens would leave the list allocation behind after an
/// orderly runtime trap.
#[derive(Clone, Copy)]
struct RegisteredManagedAnyV2List {
    creator: super::handle::LeaseThreadId,
    domain: ManagedAnyV2ListDomain,
    list: usize,
}

// This registry owns identities only. List destruction and V2 payload Drop
// glue always run after its mutex has been released.
static MANAGED_ANY_V2_LISTS: Mutex<Vec<RegisteredManagedAnyV2List>> = Mutex::new(Vec::new());

#[cfg(test)]
thread_local! {
    static MANAGED_ANY_V2_LIST_REGISTRATION_FAIL: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(super) fn with_managed_any_v2_list_registration_failure_for_test<T>(
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            MANAGED_ANY_V2_LIST_REGISTRATION_FAIL.with(|fail| fail.set(self.0));
        }
    }

    let _restore = Restore(MANAGED_ANY_V2_LIST_REGISTRATION_FAIL.with(|fail| fail.replace(true)));
    operation()
}

/// Register a newly allocated canonical V2 owner list before publishing its
/// pointer. False means the caller still owns the list and must free it.
pub(super) fn register_managed_any_v2_list(list: *mut super::MimiList) -> bool {
    if list.is_null() {
        return false;
    }
    #[cfg(test)]
    if MANAGED_ANY_V2_LIST_REGISTRATION_FAIL.with(Cell::get) {
        return false;
    }
    let mut lists = MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let list_identity = list as usize;
    if lists
        .iter()
        .any(|registered| registered.list == list_identity)
        || lists.try_reserve(1).is_err()
    {
        return false;
    }
    lists.push(RegisteredManagedAnyV2List {
        creator: super::handle::current_lease_thread_id(),
        domain: ManagedAnyV2ListDomain::CanonicalOwnerListV2,
        list: list_identity,
    });
    true
}

/// Check that the exact pointer belongs to the requested list ownership
/// domain. The creator is intentionally not compared here: explicit Drop may
/// happen on another thread, while trap draining filters by creator below.
pub(super) fn managed_any_v2_list_is_registered(
    list: *const super::MimiList,
    domain: ManagedAnyV2ListDomain,
) -> bool {
    if list.is_null() {
        return false;
    }
    MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .any(|registered| registered.list == list as usize && registered.domain == domain)
}

/// Remove a list's cleanup receipt before normal destruction. This is also
/// called by the generic MimiList deallocator, so every supported free path
/// retires the same identity exactly once.
pub(super) fn forget_managed_any_v2_list(
    list: *mut super::MimiList,
    domain: ManagedAnyV2ListDomain,
) -> bool {
    if list.is_null() {
        return false;
    }
    let mut lists = MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(index) = lists
        .iter()
        .position(|registered| registered.list == list as usize && registered.domain == domain)
    {
        lists.remove(index);
        return true;
    }
    false
}

/// Retire every V2 owner list created by the trapping native thread. Detach a
/// pointer identity from the registry lock before invoking the normal typed
/// list Drop path; Drop itself removes the receipt before releasing any
/// element owner. Remaining independently published tokens are drained by
/// `ordinary_map_domain` after this function.
pub(super) fn drain_current_thread_managed_any_v2_lists() {
    let Some(creator) = super::handle::cached_lease_thread_id() else {
        return;
    };
    loop {
        let list = {
            let mut lists = MANAGED_ANY_V2_LISTS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let index = lists.iter().rposition(|registered| {
                registered.creator == creator
                    && registered.domain == ManagedAnyV2ListDomain::CanonicalOwnerListV2
            });
            index.map(|index| lists[index].list as *mut super::MimiList)
        };
        let Some(list) = list else {
            return;
        };
        // SAFETY: the matching domain/creator receipt was observed under the
        // registry lock, and normal Drop removes it before freeing the list.
        unsafe { super::mimi_mir_list_drop_managed_any_v2(list) };
    }
}

#[cfg(test)]
pub(super) fn managed_any_v2_list_registered_for_test(list: *mut super::MimiList) -> bool {
    managed_any_v2_list_is_registered(list, ManagedAnyV2ListDomain::CanonicalOwnerListV2)
}

#[cfg(test)]
fn managed_any_v2_list_registered_in_legacy_domain_for_test(list: *mut super::MimiList) -> bool {
    managed_any_v2_list_is_registered(list, ManagedAnyV2ListDomain::LegacyManagedAnyV1)
}

#[derive(Clone)]
struct PublishedOrdinaryMapAnyV2Values {
    // Cleanup ownership follows the thread that published this opaque handle.
    // Acquires are shared across threads, and an explicit Drop may retire it
    // from any thread; the creator is consulted only by trap-domain draining.
    creator: super::handle::LeaseThreadId,
    snapshot: Arc<OrdinaryMapAnyV2ValuesSnapshot>,
}

static V2_VALUES_NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);
static V2_VALUES_HANDLES: OnceLock<Mutex<HashMap<i64, PublishedOrdinaryMapAnyV2Values>>> =
    OnceLock::new();

fn v2_values_handles() -> &'static Mutex<HashMap<i64, PublishedOrdinaryMapAnyV2Values>> {
    V2_VALUES_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn allocate_v2_values_handle() -> Result<i64, ManagedAnyStatus> {
    V2_VALUES_NEXT_HANDLE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            (current > 0).then(|| current.checked_add(1)).flatten()
        })
        .map_err(|_| ManagedAnyStatus::Capacity)
}

fn publish_v2_values_snapshot(
    snapshot: OrdinaryMapAnyV2ValuesSnapshot,
) -> Result<i64, ManagedAnyStatus> {
    snapshot_allocation_checkpoint().map_err(|_| ManagedAnyStatus::Capacity)?;
    let handle = allocate_v2_values_handle()?;
    let published = PublishedOrdinaryMapAnyV2Values {
        creator: super::handle::current_lease_thread_id(),
        snapshot: Arc::new(snapshot),
    };
    let mut registry = v2_values_handles()
        .lock()
        .map_err(|_| ManagedAnyStatus::Poisoned)?;
    registry
        .try_reserve(1)
        .map_err(|_| ManagedAnyStatus::Capacity)?;
    registry.insert(handle, published);
    Ok(handle)
}

fn acquire_v2_values_snapshot(
    handle: i64,
) -> Result<Arc<OrdinaryMapAnyV2ValuesSnapshot>, ManagedAnyStatus> {
    if handle <= 0 {
        return Err(ManagedAnyStatus::InvalidToken);
    }
    let registry = v2_values_handles()
        .lock()
        .map_err(|_| ManagedAnyStatus::Poisoned)?;
    registry
        .get(&handle)
        .map(|published| Arc::clone(&published.snapshot))
        .ok_or_else(|| {
            if handle < V2_VALUES_NEXT_HANDLE.load(Ordering::Relaxed) {
                ManagedAnyStatus::StaleToken
            } else {
                ManagedAnyStatus::InvalidToken
            }
        })
}

fn retire_v2_values_snapshot(handle: i64) -> Result<(), ManagedAnyStatus> {
    if handle <= 0 {
        return Err(ManagedAnyStatus::InvalidToken);
    }
    // Detach under the registry lock, then release the owner graph after the
    // lock has been dropped. Payload callbacks must never run under this lock.
    let removed = v2_values_handles()
        .lock()
        .map_err(|_| ManagedAnyStatus::Poisoned)?
        .remove(&handle);
    match removed {
        Some(snapshot) => {
            drop(snapshot);
            Ok(())
        }
        None if handle < V2_VALUES_NEXT_HANDLE.load(Ordering::Relaxed) => {
            Err(ManagedAnyStatus::StaleToken)
        }
        None => Err(ManagedAnyStatus::InvalidToken),
    }
}

/// Release values snapshots created by the trapping execution thread. Each
/// entry is detached before its V2 owners run Drop glue, so cleanup is
/// idempotent and callback-safe even during an orderly trap drain.
pub(super) fn drain_current_thread_v2_values_snapshots() {
    let Some(creator) = super::handle::cached_lease_thread_id() else {
        return;
    };
    loop {
        let removed = {
            let Ok(mut registry) = v2_values_handles().lock() else {
                return;
            };
            let handle = registry
                .iter()
                .find_map(|(handle, snapshot)| (snapshot.creator == creator).then_some(*handle));
            handle.and_then(|handle| registry.remove(&handle))
        };
        match removed {
            Some(snapshot) => drop(snapshot),
            None => return,
        }
    }
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
            super::any_owner::AnyOwnerKind::OwnedString
            | super::any_owner::AnyOwnerKind::ManagedV2 => Ok(OrdinaryMapLookup::TypeMismatch),
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
            super::any_owner::AnyOwnerKind::I32 | super::any_owner::AnyOwnerKind::ManagedV2 => {
                Ok(OrdinaryMapLookup::TypeMismatch)
            }
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
    static OWNED_ANY_V2_ROOT_PUBLISH_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_GET_ANY_PUBLISH_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_GET_ANY_PUBLISH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_VALUES_PUBLISH_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_VALUES_PUBLISH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_UNPACK_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_UNPACK_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_VALUES_UNPACK_OWNER_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_VALUES_UNPACK_OWNER_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_VALUES_UNPACK_STRING_CLONE_FAIL: Cell<bool> = const { Cell::new(false) };
    static OWNED_ANY_V2_VALUES_UNPACK_STRING_PUBLISH_FAIL: Cell<bool> = const { Cell::new(false) };
    static STRING_CONCAT_FAIL_STAGE: Cell<Option<i32>> = const { Cell::new(None) };
    static STRING_CONCAT_ALLOCATION_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static STRING_CONCAT_PUBLICATION_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
}

/// Shared logical fault points for canonical MIR String concatenation. Stage
/// 0 is allocation; stage 1 is publication after the complete result exists.
#[cfg(test)]
pub(crate) fn ordinary_map_string_concat_checkpoint_for_test(stage: i32) -> Result<(), ()> {
    let attempts = match stage {
        0 => &STRING_CONCAT_ALLOCATION_ATTEMPTS,
        1 => &STRING_CONCAT_PUBLICATION_ATTEMPTS,
        _ => return Ok(()),
    };
    attempts.with(|count| count.set(count.get().saturating_add(1)));
    if STRING_CONCAT_FAIL_STAGE.with(Cell::get) == Some(stage) {
        Err(())
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn ordinary_map_string_concat_checkpoint(stage: i32) -> Result<(), ()> {
    ordinary_map_string_concat_checkpoint_for_test(stage)
}

#[cfg(not(test))]
pub(crate) fn ordinary_map_string_concat_checkpoint(_stage: i32) -> Result<(), ()> {
    Ok(())
}

#[cfg(test)]
pub(crate) fn ordinary_map_string_concat_attempts_for_test(stage: i32) -> usize {
    match stage {
        0 => STRING_CONCAT_ALLOCATION_ATTEMPTS.with(Cell::get),
        1 => STRING_CONCAT_PUBLICATION_ATTEMPTS.with(Cell::get),
        _ => 0,
    }
}

#[cfg(test)]
pub(crate) fn with_ordinary_map_string_concat_failure_for_test<T>(
    stage: i32,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore {
        stage: Option<i32>,
        allocation_attempts: usize,
        publication_attempts: usize,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            STRING_CONCAT_FAIL_STAGE.with(|value| value.set(self.stage));
            STRING_CONCAT_ALLOCATION_ATTEMPTS.with(|value| value.set(self.allocation_attempts));
            STRING_CONCAT_PUBLICATION_ATTEMPTS.with(|value| value.set(self.publication_attempts));
        }
    }

    let _restore = Restore {
        stage: STRING_CONCAT_FAIL_STAGE.with(|value| value.replace(Some(stage))),
        allocation_attempts: STRING_CONCAT_ALLOCATION_ATTEMPTS.with(|value| value.replace(0)),
        publication_attempts: STRING_CONCAT_PUBLICATION_ATTEMPTS.with(|value| value.replace(0)),
    };
    operation()
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

/// A shared logical failpoint at the V2 root publication boundary. Native,
/// reference, and bytecode consumers invoke this only after staging a complete
/// replacement root and immediately before publishing it. The failed outcome
/// therefore leaves the old root and every input owner unchanged.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_root_publish_checkpoint_for_test() -> Result<(), ()> {
    OWNED_ANY_V2_ROOT_PUBLISH_FAIL_AFTER.with(|fail_after| match fail_after.get() {
        Some(remaining) => {
            OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS
                .with(|attempts| attempts.set(attempts.get().saturating_add(1)));
            if remaining == 0 {
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
pub(crate) fn ordinary_map_owned_any_v2_root_publish_attempts_for_test() -> usize {
    OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS.with(Cell::get)
}

/// Install one thread-local root-publication failure and restore the prior
/// oracle state even if the operation unwinds.
#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_root_publish_failure_for_test<T>(
    stages_before_failure: usize,
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint {
        fail_after: Option<usize>,
        attempts: usize,
    }
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_ANY_V2_ROOT_PUBLISH_FAIL_AFTER.with(|value| value.set(self.fail_after));
            OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS.with(|value| value.set(self.attempts));
        }
    }

    let _restore = RestoreFailpoint {
        fail_after: OWNED_ANY_V2_ROOT_PUBLISH_FAIL_AFTER
            .with(|value| value.replace(Some(stages_before_failure))),
        attempts: OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS.with(|value| value.replace(0)),
    };
    operation()
}

/// A shared logical failpoint at the V2 GetAny owner-publication boundary.
/// Consumers invoke this after staging the selected payload (or canonical
/// miss owner) and immediately before exposing the result. A failure therefore
/// drops only the private staged owner and leaves the source Map untouched.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_get_any_publish_checkpoint_for_test() -> Result<(), ()> {
    OWNED_ANY_V2_GET_ANY_PUBLISH_FAIL_AFTER.with(|fail_after| match fail_after.get() {
        Some(remaining) => {
            OWNED_ANY_V2_GET_ANY_PUBLISH_ATTEMPTS
                .with(|attempts| attempts.set(attempts.get().saturating_add(1)));
            if remaining == 0 {
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
pub(crate) fn ordinary_map_owned_any_v2_get_any_publish_attempts_for_test() -> usize {
    OWNED_ANY_V2_GET_ANY_PUBLISH_ATTEMPTS.with(Cell::get)
}

/// Install one thread-local GetAny owner-publication failure and restore the
/// prior oracle state even if the operation unwinds.
#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_get_any_publish_failure_for_test<T>(
    stages_before_failure: usize,
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint {
        fail_after: Option<usize>,
        attempts: usize,
    }
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_ANY_V2_GET_ANY_PUBLISH_FAIL_AFTER.with(|value| value.set(self.fail_after));
            OWNED_ANY_V2_GET_ANY_PUBLISH_ATTEMPTS.with(|value| value.set(self.attempts));
        }
    }

    let _restore = RestoreFailpoint {
        fail_after: OWNED_ANY_V2_GET_ANY_PUBLISH_FAIL_AFTER
            .with(|value| value.replace(Some(stages_before_failure))),
        attempts: OWNED_ANY_V2_GET_ANY_PUBLISH_ATTEMPTS.with(|value| value.replace(0)),
    };
    operation()
}

/// A shared logical failpoint at the V2 Values-list publication boundary.
/// Consumers consult it after staging the complete sorted snapshot and just
/// before exposing it, so failure leaves the source Map and output untouched.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_values_publish_checkpoint_for_test() -> Result<(), ()> {
    OWNED_ANY_V2_VALUES_PUBLISH_FAIL_AFTER.with(|fail_after| match fail_after.get() {
        Some(remaining) => {
            OWNED_ANY_V2_VALUES_PUBLISH_ATTEMPTS
                .with(|attempts| attempts.set(attempts.get().saturating_add(1)));
            if remaining == 0 {
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
pub(crate) fn ordinary_map_owned_any_v2_values_publish_attempts_for_test() -> usize {
    OWNED_ANY_V2_VALUES_PUBLISH_ATTEMPTS.with(Cell::get)
}

/// Install one thread-local Values-publication failure and restore the prior
/// oracle state even if the operation unwinds.
#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_values_publish_failure_for_test<T>(
    stages_before_failure: usize,
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint {
        fail_after: Option<usize>,
        attempts: usize,
    }
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_ANY_V2_VALUES_PUBLISH_FAIL_AFTER.with(|value| value.set(self.fail_after));
            OWNED_ANY_V2_VALUES_PUBLISH_ATTEMPTS.with(|value| value.set(self.attempts));
        }
    }

    let _restore = RestoreFailpoint {
        fail_after: OWNED_ANY_V2_VALUES_PUBLISH_FAIL_AFTER
            .with(|value| value.replace(Some(stages_before_failure))),
        attempts: OWNED_ANY_V2_VALUES_PUBLISH_ATTEMPTS.with(|value| value.replace(0)),
    };
    operation()
}

/// A shared logical failure oracle at the typed V2 payload-unpack boundary.
/// The runtime, reference interpreter, and bytecode VM consult it only after
/// the exact payload TypeDesc matches and before any result bytes are written.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_unpack_checkpoint_for_test() -> Result<(), ()> {
    OWNED_ANY_V2_UNPACK_FAIL_AFTER.with(|fail_after| match fail_after.get() {
        Some(remaining) => {
            OWNED_ANY_V2_UNPACK_ATTEMPTS
                .with(|attempts| attempts.set(attempts.get().saturating_add(1)));
            if remaining == 0 {
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
pub(crate) fn ordinary_map_owned_any_v2_unpack_attempts_for_test() -> usize {
    OWNED_ANY_V2_UNPACK_ATTEMPTS.with(Cell::get)
}

/// Install one thread-local typed-unpack failure and restore prior state even
/// if the operation unwinds.
#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_unpack_failure_for_test<T>(
    stages_before_failure: usize,
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint {
        fail_after: Option<usize>,
        attempts: usize,
    }
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_ANY_V2_UNPACK_FAIL_AFTER.with(|value| value.set(self.fail_after));
            OWNED_ANY_V2_UNPACK_ATTEMPTS.with(|value| value.set(self.attempts));
        }
    }

    let _restore = RestoreFailpoint {
        fail_after: OWNED_ANY_V2_UNPACK_FAIL_AFTER
            .with(|value| value.replace(Some(stages_before_failure))),
        attempts: OWNED_ANY_V2_UNPACK_ATTEMPTS.with(|value| value.replace(0)),
    };
    operation()
}

/// Capacity oracle for the first ValuesUnpack boundary: cloning the selected
/// list owner after bounds succeeds but before the requested TypeDesc is
/// checked. This ordering is observable for a valid index with a wrong cast.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_values_unpack_owner_checkpoint_for_test() -> Result<(), ()>
{
    OWNED_ANY_V2_VALUES_UNPACK_OWNER_FAIL_AFTER.with(|fail_after| match fail_after.get() {
        Some(remaining) => {
            OWNED_ANY_V2_VALUES_UNPACK_OWNER_ATTEMPTS
                .with(|attempts| attempts.set(attempts.get().saturating_add(1)));
            if remaining == 0 {
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
pub(crate) fn ordinary_map_owned_any_v2_values_unpack_owner_attempts_for_test() -> usize {
    OWNED_ANY_V2_VALUES_UNPACK_OWNER_ATTEMPTS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_values_unpack_owner_failure_for_test<T>(
    stages_before_failure: usize,
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint {
        fail_after: Option<usize>,
        attempts: usize,
    }
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_ANY_V2_VALUES_UNPACK_OWNER_FAIL_AFTER.with(|value| value.set(self.fail_after));
            OWNED_ANY_V2_VALUES_UNPACK_OWNER_ATTEMPTS.with(|value| value.set(self.attempts));
        }
    }

    let _restore = RestoreFailpoint {
        fail_after: OWNED_ANY_V2_VALUES_UNPACK_OWNER_FAIL_AFTER
            .with(|value| value.replace(Some(stages_before_failure))),
        attempts: OWNED_ANY_V2_VALUES_UNPACK_OWNER_ATTEMPTS.with(|value| value.replace(0)),
    };
    operation()
}

/// Test-only failure at the final canonical String allocation for a V2
/// ValuesUnpack. Reference and bytecode use this at their corresponding
/// output-copy boundary; native calls the status-returning copy helper below.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_values_unpack_string_clone_checkpoint_for_test(
) -> Result<(), ()> {
    if OWNED_ANY_V2_VALUES_UNPACK_STRING_CLONE_FAIL.with(Cell::get) {
        Err(())
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_values_unpack_string_publish_checkpoint_for_test(
) -> Result<(), ()> {
    if OWNED_ANY_V2_VALUES_UNPACK_STRING_PUBLISH_FAIL.with(Cell::get) {
        Err(())
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_values_unpack_string_clone_failure_for_test<T>(
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint(bool);
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_ANY_V2_VALUES_UNPACK_STRING_CLONE_FAIL.with(|value| value.set(self.0));
        }
    }
    let _restore = RestoreFailpoint(
        OWNED_ANY_V2_VALUES_UNPACK_STRING_CLONE_FAIL.with(|value| value.replace(true)),
    );
    operation()
}

#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_values_unpack_string_publish_failure_for_test<T>(
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint(bool);
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_ANY_V2_VALUES_UNPACK_STRING_PUBLISH_FAIL.with(|value| value.set(self.0));
        }
    }
    let _restore = RestoreFailpoint(
        OWNED_ANY_V2_VALUES_UNPACK_STRING_PUBLISH_FAIL.with(|value| value.replace(true)),
    );
    operation()
}

pub(super) fn ordinary_map_owned_any_v2_values_unpack_owner_checkpoint() -> Result<(), ()> {
    #[cfg(test)]
    {
        return ordinary_map_owned_any_v2_values_unpack_owner_checkpoint_for_test();
    }
    #[cfg(not(test))]
    Ok(())
}

fn ordinary_map_owned_any_v2_values_unpack_string_clone_checkpoint() -> Result<(), ManagedAnyStatus>
{
    #[cfg(test)]
    {
        return ordinary_map_owned_any_v2_values_unpack_string_clone_checkpoint_for_test()
            .map_err(|_| ManagedAnyStatus::Capacity);
    }
    #[cfg(not(test))]
    Ok(())
}

fn ordinary_map_owned_any_v2_values_unpack_string_publish_checkpoint(
) -> Result<(), ManagedAnyStatus> {
    #[cfg(test)]
    {
        return ordinary_map_owned_any_v2_values_unpack_string_publish_checkpoint_for_test()
            .map_err(|_| ManagedAnyStatus::Capacity);
    }
    #[cfg(not(test))]
    Ok(())
}

fn ordinary_map_owned_any_v2_unpack_checkpoint() -> Result<(), ManagedAnyStatus> {
    #[cfg(test)]
    {
        return ordinary_map_owned_any_v2_unpack_checkpoint_for_test()
            .map_err(|_| ManagedAnyStatus::Capacity);
    }
    #[cfg(not(test))]
    Ok(())
}

fn ordinary_map_owned_any_v2_get_any_publish_checkpoint() -> Result<(), ManagedAnyStatus> {
    #[cfg(test)]
    {
        return ordinary_map_owned_any_v2_get_any_publish_checkpoint_for_test()
            .map_err(|_| ManagedAnyStatus::Capacity);
    }
    #[cfg(not(test))]
    Ok(())
}

fn ordinary_map_owned_any_v2_values_publish_checkpoint() -> Result<(), ManagedAnyStatus> {
    #[cfg(test)]
    {
        return ordinary_map_owned_any_v2_values_publish_checkpoint_for_test()
            .map_err(|_| ManagedAnyStatus::Capacity);
    }
    #[cfg(not(test))]
    Ok(())
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

fn new_owned_any_v2_root(
    snapshot: OrdinaryMapAnyV2Snapshot,
) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    snapshot_allocation_checkpoint()?;
    #[cfg(test)]
    ordinary_map_owned_any_v2_root_publish_checkpoint_for_test()
        .map_err(|_| OrdinaryMapSnapshotRuntimeError::Capacity)?;
    match super::handle::map_new_handle_fallible(super::MimiMap::ordinary_map_owned_any_v2(
        snapshot,
    )) {
        Ok(handle) => super::ordinary_map_domain::publish_new_root(
            super::ordinary_map_domain::OrdinaryMapProfile::OwnedAnyV2,
            handle,
        )
        .map_err(|_| OrdinaryMapSnapshotRuntimeError::Capacity),
        Err(uninstalled_map) => {
            drop(uninstalled_map);
            Err(OrdinaryMapSnapshotRuntimeError::Capacity)
        }
    }
}

fn acquire_owned_any_v2_root(
    handle: i64,
) -> Result<super::handle::MapLease, OrdinaryMapSnapshotRuntimeError> {
    super::handle::ordinary_map_owned_any_v2_acquire(handle).map_err(Into::into)
}

fn snapshot_any_v2_from_map(
    map: &super::MimiMap,
) -> Result<&OrdinaryMapAnyV2Snapshot, OrdinaryMapSnapshotRuntimeError> {
    map.ordinary_map_any_v2_snapshot
        .as_ref()
        .ok_or(OrdinaryMapSnapshotRuntimeError::InvalidInput)
}

fn derive_owned_any_v2_root(
    handle: i64,
    update: impl FnOnce(
        &OrdinaryMapAnyV2Snapshot,
    ) -> Result<OrdinaryMapAnyV2Snapshot, OrdinaryMapSnapshotError>,
) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    let source = acquire_owned_any_v2_root(handle)?;
    let snapshot = update(snapshot_any_v2_from_map(&source)?)?;
    drop(source);
    new_owned_any_v2_root(snapshot)
}

fn clone_owned_any_v2_root(handle: i64) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    derive_owned_any_v2_root(handle, OrdinaryMapAnyV2Snapshot::clone_root)
}

fn remove_owned_any_v2_root(
    handle: i64,
    key: &str,
) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    derive_owned_any_v2_root(handle, |snapshot| snapshot.remove(key))
}

fn drop_owned_any_v2_root(handle: i64) -> Result<(), OrdinaryMapSnapshotRuntimeError> {
    super::handle::ordinary_map_owned_any_v2_drop(handle)?;
    super::ordinary_map_domain::forget_root(
        super::ordinary_map_domain::OrdinaryMapProfile::OwnedAnyV2,
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

/// Shared internal status table for ManagedAny and ordinary Map V2 helpers.
/// Ordinary Map Get absence is a successful status with a fresh zero owner.
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
    CloneFailed = 14,
    UnsupportedAggregate = 15,
    CallbackConflict = 16,
    SizeOverflow = 17,
}

impl From<AnyOwnerError> for ManagedAnyStatus {
    fn from(error: AnyOwnerError) -> Self {
        match error {
            AnyOwnerError::InvalidToken => Self::InvalidToken,
            AnyOwnerError::StaleToken => Self::StaleToken,
            AnyOwnerError::DescriptorMismatch => Self::DescriptorMismatch,
            AnyOwnerError::Capacity => Self::Capacity,
            AnyOwnerError::Poisoned => Self::Poisoned,
            AnyOwnerError::UnsupportedAggregate => Self::UnsupportedAggregate,
            AnyOwnerError::CallbackConflict => Self::CallbackConflict,
            AnyOwnerError::CloneFailed(_) => Self::CloneFailed,
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
            OrdinaryMapSnapshotRuntimeError::Capacity => Self::Capacity,
            OrdinaryMapSnapshotRuntimeError::SizeOverflow => Self::SizeOverflow,
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

unsafe fn managed_v2_identity_from_abi(
    tag: i32,
    type_id: *const u8,
    type_id_len: i64,
    descriptor_hash: *const u8,
    size: i64,
    align: i64,
) -> Result<ManagedAnyPayloadV2Identity, ManagedAnyStatus> {
    use super::managed_any_payload_v2::{
        ManagedAnyPayloadV2Tag, ManagedAnyPayloadV2TypeId, DESCRIPTOR_HASH_BYTES,
    };

    let tag = ManagedAnyPayloadV2Tag::try_from(tag).map_err(|_| ManagedAnyStatus::InvalidInput)?;
    let id_bytes =
        unsafe { checked_abi_bytes(type_id, type_id_len) }.map_err(ManagedAnyStatus::from)?;
    let type_id =
        ManagedAnyPayloadV2TypeId::parse(id_bytes).map_err(|_| ManagedAnyStatus::InvalidInput)?;
    if descriptor_hash.is_null() {
        return Err(ManagedAnyStatus::InvalidInput);
    }
    let mut hash = [0; DESCRIPTOR_HASH_BYTES];
    // SAFETY: the ABI contract requires 32 readable raw hash bytes.
    unsafe { std::ptr::copy_nonoverlapping(descriptor_hash, hash.as_mut_ptr(), hash.len()) };
    let size = usize::try_from(size).map_err(|_| ManagedAnyStatus::InvalidInput)?;
    let align = usize::try_from(align).map_err(|_| ManagedAnyStatus::InvalidInput)?;
    Ok(ManagedAnyPayloadV2Identity {
        tag,
        type_id,
        descriptor_hash: hash,
        size,
        align,
    })
}

fn managed_v2_token_receipt(
    owner: i64,
) -> Result<(AnyOwnerToken, ManagedAnyPayloadV2Identity), ManagedAnyStatus> {
    let token = AnyOwnerToken::from_wire_bits(owner as u64).map_err(ManagedAnyStatus::from)?;
    let receipt = super::ordinary_map_domain::managed_any_v2_identity(token)
        .map_err(ManagedAnyStatus::from)?;
    let Some(receipt) = receipt else {
        any_owner::validate(token).map_err(ManagedAnyStatus::from)?;
        return Err(ManagedAnyStatus::UnmanagedToken);
    };
    let actual = any_owner::managed_v2_identity(token).map_err(ManagedAnyStatus::from)?;
    if actual != receipt {
        return Err(ManagedAnyStatus::DescriptorMismatch);
    }
    Ok((token, receipt))
}

/// Exact V2 owner validator used by `ListElementKind::ManagedAnyV2`. The
/// receipt comparison includes tag, type ID, descriptor hash, size, and
/// alignment; no v1 owner descriptor or callback table participates.
pub(super) fn managed_v2_owner_is_valid_for_list(owner: i64) -> bool {
    managed_v2_token_receipt(owner).is_ok()
}

fn map_get_status(error: ManagedAnyStatus) -> i32 {
    error as i32
}

fn managed_any_status_from_code(status: i32) -> ManagedAnyStatus {
    match status {
        0 => ManagedAnyStatus::Ok,
        1 => ManagedAnyStatus::InvalidInput,
        2 => ManagedAnyStatus::TypeMismatch,
        3 => ManagedAnyStatus::Capacity,
        4 => ManagedAnyStatus::InvalidToken,
        5 => ManagedAnyStatus::StaleToken,
        6 => ManagedAnyStatus::VersionMismatch,
        7 => ManagedAnyStatus::DescriptorMismatch,
        8 => ManagedAnyStatus::Poisoned,
        9 => ManagedAnyStatus::UnmanagedToken,
        10 => ManagedAnyStatus::RootInvalid,
        11 => ManagedAnyStatus::RootStaleDestroyed,
        12 => ManagedAnyStatus::RootReentrant,
        13 => ManagedAnyStatus::DomainConflict,
        14 => ManagedAnyStatus::CloneFailed,
        15 => ManagedAnyStatus::UnsupportedAggregate,
        16 => ManagedAnyStatus::CallbackConflict,
        17 => ManagedAnyStatus::SizeOverflow,
        _ => ManagedAnyStatus::DomainConflict,
    }
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

// -------------------------------------------------------------------------
// Ordinary Map/Any V2 concrete-record owner runtime
// -------------------------------------------------------------------------
//
// This profile is intentionally a separate owner domain from both the v1
// i32/String snapshot and the legacy `ValueHandle` Map.  The first supported
// concrete payload is `Label { text: string }`.  A V2 root stores a private
// UTF-8 copy of every key and label field, and persistent Set/Remove operations
// build a new root while leaving their source root untouched.

/// Borrowed C view of the concrete `Label { text: string }` payload accepted by
/// the V2 Set ABI.  The runtime copies `text` before publishing the new root;
/// this struct itself is never retained and is not an owner handle.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MimiMapAnyOwnerV2Label {
    pub text: *const u8,
    pub text_len: i64,
}

/// Owned storage for the concrete V2 payload shapes.  This is intentionally a
/// private enum rather than the v1 AnyOwner table: each branch is deep-copied
/// while building a persistent root, and no wire token is exposed.
enum OrdinaryMapAnyOwnerV2Value {
    I32(i32),
    Label(OrdinaryMapAnyOwnerV2LabelOwner),
}

/// The owned `Label { text: string }` payload in the initial V2 runtime
/// slice.  It has no Clone implementation on purpose: callers must use the
/// fallible snapshot clone path so allocation failure cannot silently
/// duplicate an owner.
struct OrdinaryMapAnyOwnerV2LabelOwner {
    text: String,
}

impl OrdinaryMapAnyOwnerV2Value {
    fn try_clone(&self) -> Result<Self, OrdinaryMapAnyOwnerV2Error> {
        match self {
            Self::I32(value) => Ok(Self::I32(*value)),
            Self::Label(label) => Ok(Self::Label(label.try_clone()?)),
        }
    }
}

impl OrdinaryMapAnyOwnerV2LabelOwner {
    fn try_new(text: String) -> Result<Self, OrdinaryMapAnyOwnerV2Error> {
        Ok(Self { text })
    }

    fn try_clone(&self) -> Result<Self, OrdinaryMapAnyOwnerV2Error> {
        Ok(Self {
            text: v2_copy_string(&self.text)?,
        })
    }
}

/// Immutable persistent V2 root.  No field stores a `ValueHandle` or a v1
/// `AnyOwnerToken`; dropping this value releases all owned String payloads by
/// normal Rust ownership once its dedicated generational root is retired.
pub(crate) struct OrdinaryMapAnyOwnerV2Snapshot {
    entries: HashMap<String, OrdinaryMapAnyOwnerV2Value>,
}

impl OrdinaryMapAnyOwnerV2Snapshot {
    pub(super) fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn try_clone_root(&self) -> Result<Self, OrdinaryMapAnyOwnerV2Error> {
        let mut entries = HashMap::new();
        entries
            .try_reserve(self.entries.len())
            .map_err(|_| OrdinaryMapAnyOwnerV2Error::Capacity)?;
        for (key, value) in &self.entries {
            let key = v2_copy_string(key)?;
            let value = value.try_clone()?;
            entries.insert(key, value);
        }
        Ok(Self { entries })
    }

    fn set_label_owned(
        &self,
        key: String,
        text: String,
    ) -> Result<Self, OrdinaryMapAnyOwnerV2Error> {
        let mut next = self.try_clone_root()?;
        // `try_clone_root` reserved the existing entries; reserve one more
        // slot before insertion so a failed capacity check leaves the source
        // and every staged owner untouched.
        next.entries
            .try_reserve(1)
            .map_err(|_| OrdinaryMapAnyOwnerV2Error::Capacity)?;
        next.entries.insert(
            key,
            OrdinaryMapAnyOwnerV2Value::Label(OrdinaryMapAnyOwnerV2LabelOwner::try_new(text)?),
        );
        Ok(next)
    }

    fn set_i32_owned(&self, key: String, value: i32) -> Result<Self, OrdinaryMapAnyOwnerV2Error> {
        let mut next = self.try_clone_root()?;
        next.entries
            .try_reserve(1)
            .map_err(|_| OrdinaryMapAnyOwnerV2Error::Capacity)?;
        next.entries
            .insert(key, OrdinaryMapAnyOwnerV2Value::I32(value));
        Ok(next)
    }

    fn remove(&self, key: &str) -> Result<Self, OrdinaryMapAnyOwnerV2Error> {
        let mut next = self.try_clone_root()?;
        next.entries.remove(key);
        Ok(next)
    }

    fn size(&self) -> usize {
        self.entries.len()
    }

    fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    #[cfg(test)]
    fn label_text(&self, key: &str) -> Option<&str> {
        match self.entries.get(key) {
            Some(OrdinaryMapAnyOwnerV2Value::Label(owner)) => Some(owner.text.as_str()),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrdinaryMapAnyOwnerV2Error {
    InvalidInput,
    Capacity,
    Handle(super::handle::HandleError),
}

impl From<super::handle::HandleError> for OrdinaryMapAnyOwnerV2Error {
    fn from(error: super::handle::HandleError) -> Self {
        Self::Handle(error)
    }
}

fn v2_copy_string(value: &str) -> Result<String, OrdinaryMapAnyOwnerV2Error> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|_| OrdinaryMapAnyOwnerV2Error::Capacity)?;
    copy.push_str(value);
    Ok(copy)
}

fn v2_snapshot_from_map(
    map: &super::MimiMap,
) -> Result<&OrdinaryMapAnyOwnerV2Snapshot, OrdinaryMapAnyOwnerV2Error> {
    map.ordinary_map_any_owner_v2_record
        .as_ref()
        .ok_or(OrdinaryMapAnyOwnerV2Error::InvalidInput)
}

fn v2_new_root(snapshot: OrdinaryMapAnyOwnerV2Snapshot) -> Result<i64, OrdinaryMapAnyOwnerV2Error> {
    match super::handle::map_new_handle_fallible(super::MimiMap::ordinary_map_any_owner_v2_record(
        snapshot,
    )) {
        Ok(handle) => super::ordinary_map_domain::publish_new_root(
            super::ordinary_map_domain::OrdinaryMapProfile::AnyOwnerV2Record,
            handle,
        )
        .map_err(|_| OrdinaryMapAnyOwnerV2Error::Capacity),
        Err(unpublished) => {
            // The fallible table allocator returns ownership of the complete
            // Map object after unlocking; dropping it rolls back every staged
            // key and Label field without publishing a partial root.
            drop(unpublished);
            Err(OrdinaryMapAnyOwnerV2Error::Capacity)
        }
    }
}

fn v2_new_empty_root() -> Result<i64, OrdinaryMapAnyOwnerV2Error> {
    v2_new_root(OrdinaryMapAnyOwnerV2Snapshot::new())
}

fn v2_acquire_root(handle: i64) -> Result<super::handle::MapLease, OrdinaryMapAnyOwnerV2Error> {
    super::handle::ordinary_map_any_owner_v2_record_acquire(handle).map_err(Into::into)
}

fn v2_derive_root(
    handle: i64,
    update: impl FnOnce(
        &OrdinaryMapAnyOwnerV2Snapshot,
    ) -> Result<OrdinaryMapAnyOwnerV2Snapshot, OrdinaryMapAnyOwnerV2Error>,
) -> Result<i64, OrdinaryMapAnyOwnerV2Error> {
    let source = v2_acquire_root(handle)?;
    let snapshot = update(v2_snapshot_from_map(&source)?)?;
    drop(source);
    v2_new_root(snapshot)
}

fn v2_remove_root(handle: i64, key: &str) -> Result<i64, OrdinaryMapAnyOwnerV2Error> {
    v2_derive_root(handle, |source| source.remove(key))
}

fn v2_set_label_root(
    handle: i64,
    key: String,
    text: String,
) -> Result<i64, OrdinaryMapAnyOwnerV2Error> {
    v2_derive_root(handle, |source| source.set_label_owned(key, text))
}

fn v2_set_i32_root(
    handle: i64,
    key: String,
    value: i32,
) -> Result<i64, OrdinaryMapAnyOwnerV2Error> {
    v2_derive_root(handle, |source| source.set_i32_owned(key, value))
}

fn v2_clone_root(handle: i64) -> Result<i64, OrdinaryMapAnyOwnerV2Error> {
    v2_derive_root(handle, OrdinaryMapAnyOwnerV2Snapshot::try_clone_root)
}

fn v2_size_root(handle: i64) -> Result<i32, OrdinaryMapAnyOwnerV2Error> {
    let root = v2_acquire_root(handle)?;
    let size = v2_snapshot_from_map(&root)?.size();
    i32::try_from(size).map_err(|_| OrdinaryMapAnyOwnerV2Error::Capacity)
}

fn v2_contains_root(handle: i64, key: &str) -> Result<bool, OrdinaryMapAnyOwnerV2Error> {
    let root = v2_acquire_root(handle)?;
    Ok(v2_snapshot_from_map(&root)?.contains(key))
}

fn v2_drop_root(handle: i64) -> Result<(), OrdinaryMapAnyOwnerV2Error> {
    super::handle::ordinary_map_any_owner_v2_record_drop(handle)?;
    super::ordinary_map_domain::forget_root(
        super::ordinary_map_domain::OrdinaryMapProfile::AnyOwnerV2Record,
        handle,
    );
    Ok(())
}

unsafe fn v2_checked_abi_bytes<'a>(
    pointer: *const u8,
    length: i64,
) -> Result<&'a [u8], OrdinaryMapAnyOwnerV2Error> {
    let length = usize::try_from(length).map_err(|_| OrdinaryMapAnyOwnerV2Error::InvalidInput)?;
    if length > isize::MAX as usize {
        return Err(OrdinaryMapAnyOwnerV2Error::InvalidInput);
    }
    if length == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() {
        return Err(OrdinaryMapAnyOwnerV2Error::InvalidInput);
    }
    // SAFETY: the C ABI contract requires `pointer` to reference `length`
    // readable bytes; null/negative/oversized lengths were rejected above.
    Ok(unsafe { std::slice::from_raw_parts(pointer, length) })
}

fn v2_checked_utf8(bytes: &[u8]) -> Result<String, OrdinaryMapAnyOwnerV2Error> {
    let value = std::str::from_utf8(bytes).map_err(|_| OrdinaryMapAnyOwnerV2Error::InvalidInput)?;
    v2_copy_string(value)
}

fn v2_status(error: OrdinaryMapAnyOwnerV2Error) -> i32 {
    match error {
        OrdinaryMapAnyOwnerV2Error::InvalidInput => MIMI_MAP_ANY_OWNER_V2_INVALID_INPUT,
        OrdinaryMapAnyOwnerV2Error::Capacity => MIMI_MAP_ANY_OWNER_V2_CAPACITY,
        OrdinaryMapAnyOwnerV2Error::Handle(error) => match error {
            super::handle::HandleError::Invalid => MIMI_MAP_ANY_OWNER_V2_INVALID_HANDLE,
            super::handle::HandleError::StaleGeneration => MIMI_MAP_ANY_OWNER_V2_STALE,
            super::handle::HandleError::Destroyed => MIMI_MAP_ANY_OWNER_V2_DESTROYED,
            super::handle::HandleError::Reentrant => MIMI_MAP_ANY_OWNER_V2_REENTRANT,
        },
    }
}

pub const MIMI_MAP_ANY_OWNER_V2_OK: i32 = 0;
pub const MIMI_MAP_ANY_OWNER_V2_INVALID_INPUT: i32 = 1;
pub const MIMI_MAP_ANY_OWNER_V2_CAPACITY: i32 = 2;
pub const MIMI_MAP_ANY_OWNER_V2_INVALID_HANDLE: i32 = 3;
pub const MIMI_MAP_ANY_OWNER_V2_STALE: i32 = 4;
pub const MIMI_MAP_ANY_OWNER_V2_DESTROYED: i32 = 5;
pub const MIMI_MAP_ANY_OWNER_V2_REENTRANT: i32 = 6;

/// Create an empty persistent concrete-record root.  Zero is the failure
/// sentinel; valid roots are generational handles from the dedicated Map
/// table and cannot be passed to legacy Map or v1 owner APIs.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_any_owner_v2_record_new() -> i64 {
    v2_new_empty_root().unwrap_or(0)
}

/// Create a persistent root with `Label { text }` under `key`.  Both strings
/// are copied before a new root is registered; source and old roots stay live.
/// Zero means invalid input, allocation failure, stale/wrong-profile handle,
/// or registration failure.
///
/// # Safety
/// For positive lengths, each pointer must reference that many readable UTF-8
/// bytes for the duration of this call.  The pointers are never retained.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_any_owner_v2_record_set_label(
    handle: i64,
    key: *const u8,
    key_len: i64,
    text: *const u8,
    text_len: i64,
) -> i64 {
    let result = (|| {
        let key = unsafe { v2_checked_abi_bytes(key, key_len) }?;
        let text = unsafe { v2_checked_abi_bytes(text, text_len) }?;
        let key = v2_checked_utf8(key)?;
        let text = v2_checked_utf8(text)?;
        v2_set_label_root(handle, key, text)
    })();
    result.unwrap_or(0)
}

/// Create a persistent root with a copied scalar `i32` payload.  The scalar
/// branch is part of the V2 value domain and is not represented by the v1
/// AnyOwner token table.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_any_owner_v2_record_set_i32(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: i32,
) -> i64 {
    let result = (|| {
        let key = unsafe { v2_checked_abi_bytes(key, key_len) }?;
        let key = v2_checked_utf8(key)?;
        v2_set_i32_root(handle, key, value)
    })();
    result.unwrap_or(0)
}

/// C ABI variant that receives the borrowed concrete Label descriptor as one
/// `repr(C)` value.  The descriptor is read only during this call and is never
/// installed as an owner.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_any_owner_v2_record_set(
    handle: i64,
    key: *const u8,
    key_len: i64,
    label: *const MimiMapAnyOwnerV2Label,
) -> i64 {
    if label.is_null() {
        return 0;
    }
    // SAFETY: `label` is required by the C ABI to point to one readable
    // descriptor for the duration of this call.
    let label = unsafe { *label };
    // SAFETY: forwarding the caller's borrowed byte ranges to the checked
    // direct Set ABI; no pointer is retained by the runtime.
    unsafe {
        mimi_mir_ordinary_map_any_owner_v2_record_set_label(
            handle,
            key,
            key_len,
            label.text,
            label.text_len,
        )
    }
}

/// Short alias for native emitters that use the profile name as the root
/// operation prefix.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_any_owner_v2_set_label(
    handle: i64,
    key: *const u8,
    key_len: i64,
    text: *const u8,
    text_len: i64,
) -> i64 {
    // SAFETY: same borrowed-pointer contract as the canonical function.
    unsafe {
        mimi_mir_ordinary_map_any_owner_v2_record_set_label(handle, key, key_len, text, text_len)
    }
}

#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_any_owner_v2_set_i32(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: i32,
) -> i64 {
    // SAFETY: same borrowed-key contract as the canonical V2 Set ABI.
    unsafe { mimi_mir_ordinary_map_any_owner_v2_record_set_i32(handle, key, key_len, value) }
}

/// Create a persistent root without `key`; the source root stays unchanged.
/// Zero means failure.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_any_owner_v2_record_remove(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i64 {
    let result = (|| {
        let key = unsafe { v2_checked_abi_bytes(key, key_len) }?;
        let key = v2_checked_utf8(key)?;
        v2_remove_root(handle, &key)
    })();
    result.unwrap_or(0)
}

/// Create a persistent deep clone of a V2 root, including independent Label
/// field allocations.  Zero means failure.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_any_owner_v2_record_clone(handle: i64) -> i64 {
    v2_clone_root(handle).unwrap_or(0)
}

/// Return the number of entries, or -1 for invalid/stale/wrong-profile roots
/// or an i32 overflow.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_any_owner_v2_record_size(handle: i64) -> i32 {
    v2_size_root(handle).unwrap_or(-1)
}

/// Return 0/1 for absence/presence, or -1 for invalid input or handles.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_any_owner_v2_record_contains(
    handle: i64,
    key: *const u8,
    key_len: i64,
) -> i32 {
    let result = (|| {
        let key = unsafe { v2_checked_abi_bytes(key, key_len) }?;
        let key = v2_checked_utf8(key)?;
        v2_contains_root(handle, &key).map(i32::from)
    })();
    result.unwrap_or(-1)
}

/// Consume a V2 root and drop all private Label field owners.  The return
/// status lets a native emitter diagnose stale generations without entering a
/// legacy abort path.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_any_owner_v2_record_drop(handle: i64) -> i32 {
    match v2_drop_root(handle) {
        Ok(()) => MIMI_MAP_ANY_OWNER_V2_OK,
        Err(error) => v2_status(error),
    }
}

/// Status protocol shared by every V2 Map helper. Status 0 means success;
/// every nonzero status is an error and must reach the adapter's trap path.
/// Get absence is successful and is represented by `found=0` plus a fresh
/// i32-zero V2 owner. Numeric assignments are `ManagedAnyStatus` values.
pub const ORDINARY_MAP_OWNED_ANY_V2_OK: i32 = 0;

fn status_code(status: ManagedAnyStatus) -> i32 {
    status as i32
}

/// Create and publish a fresh V2 owner. Input is borrowed; generated recursive
/// clone glue creates the private payload allocation. Only compiler-native
/// declarations may call this reserved helper.
///
/// # Safety
/// `type_id` spans `type_id_len` bytes, `descriptor_hash` spans 32 raw bytes,
/// `payload` has the declared size/alignment, and both callbacks are trusted
/// compiler-generated glue bound to that exact V2 descriptor receipt.
#[no_mangle]
pub unsafe extern "C" fn mimi_any_payload_v2_new(
    tag: i32,
    type_id: *const u8,
    type_id_len: i64,
    descriptor_hash: *const u8,
    payload: *const u8,
    size: i64,
    align: i64,
    clone_fn: Option<super::managed_any_payload_v2::ManagedAnyPayloadV2CloneGlue>,
    drop_fn: Option<super::managed_any_payload_v2::ManagedAnyPayloadV2DropGlue>,
    out_token: *mut i64,
) -> i32 {
    if !out_token.is_null() {
        unsafe { *out_token = 0 };
    }
    if out_token.is_null() {
        return status_code(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let identity = unsafe {
            managed_v2_identity_from_abi(tag, type_id, type_id_len, descriptor_hash, size, align)
        }?;
        let glue = super::managed_any_payload_v2::ManagedAnyPayloadV2Glue {
            clone: clone_fn.ok_or(ManagedAnyStatus::InvalidInput)?,
            drop: drop_fn.ok_or(ManagedAnyStatus::InvalidInput)?,
        };
        let owner =
            OwnedAnyV2::from_borrowed(identity, glue, payload).map_err(ManagedAnyStatus::from)?;
        owner.publish_managed()
    })();
    match result {
        Ok(token) => {
            unsafe { *out_token = token };
            status_code(ManagedAnyStatus::Ok)
        }
        Err(status) => status_code(status),
    }
}

/// Clone any V2 owner using its runtime-selected pinned descriptor. No
/// callback, expected static type, or version is accepted here.
///
/// # Safety
/// `out_token` points to a writable i64; `token` is an opaque owner token.
#[no_mangle]
pub unsafe extern "C" fn mimi_any_payload_v2_clone(token: i64, out_token: *mut i64) -> i32 {
    if !out_token.is_null() {
        unsafe { *out_token = 0 };
    }
    if out_token.is_null() {
        return status_code(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let (token, _) = managed_v2_token_receipt(token)?;
        managed_allocation_checkpoint()?;
        let cloned = any_owner::clone_managed_v2_owner(token).map_err(ManagedAnyStatus::from)?;
        record_created_token(cloned);
        OwnedAnyV2 { token: cloned }.publish_managed()
    })();
    match result {
        Ok(token) => {
            unsafe { *out_token = token };
            status_code(ManagedAnyStatus::Ok)
        }
        Err(status) => status_code(status),
    }
}

/// Consume one V2 owner through the descriptor-pinned drop callback. A
/// successful release removes its original creator-domain receipt.
#[no_mangle]
pub extern "C" fn mimi_any_payload_v2_drop(token: i64) -> i32 {
    let result = (|| {
        let (token, identity) = managed_v2_token_receipt(token)?;
        any_owner::release_managed_v2_owner(token, identity).map_err(ManagedAnyStatus::from)?;
        super::ordinary_map_domain::forget_managed_any(token);
        Ok::<_, ManagedAnyStatus>(())
    })();
    match result {
        Ok(()) => status_code(ManagedAnyStatus::Ok),
        Err(status) => status_code(status),
    }
}

/// Exact-check and recursively clone a V2 value into uninitialized output
/// storage. Source ownership is borrowed and remains live.
///
/// # Safety
/// `type_id` is canonical, `descriptor_hash` addresses 32 raw bytes, and
/// `output` has the declared output layout. On nonzero clone status the
/// callback contract guarantees output remains wholly uninitialized.
#[no_mangle]
pub unsafe extern "C" fn mimi_any_payload_v2_unpack(
    token: i64,
    expected_tag: i32,
    type_id: *const u8,
    type_id_len: i64,
    descriptor_hash: *const u8,
    output: *mut u8,
    output_size: i64,
    output_align: i64,
) -> i32 {
    let result = (|| {
        let output_size =
            usize::try_from(output_size).map_err(|_| ManagedAnyStatus::InvalidInput)?;
        let output_align =
            usize::try_from(output_align).map_err(|_| ManagedAnyStatus::InvalidInput)?;
        let expected = unsafe {
            managed_v2_identity_from_abi(
                expected_tag,
                type_id,
                type_id_len,
                descriptor_hash,
                i64::try_from(output_size).map_err(|_| ManagedAnyStatus::InvalidInput)?,
                i64::try_from(output_align).map_err(|_| ManagedAnyStatus::InvalidInput)?,
            )
        }?;
        let (token, actual) = managed_v2_token_receipt(token)?;
        if actual != expected {
            return Err(ManagedAnyStatus::TypeMismatch);
        }
        ordinary_map_owned_any_v2_unpack_checkpoint()?;
        managed_allocation_checkpoint()?;
        unsafe {
            any_owner::unpack_managed_v2_owner(token, expected, output, output_size, output_align)
        }
        .map_err(ManagedAnyStatus::from)
    })();
    match result {
        Ok(()) => status_code(ManagedAnyStatus::Ok),
        Err(status) => status_code(status),
    }
}

/// Clone a V2 ValuesUnpack String/Label text into the canonical String domain
/// and publish its pointer only after registration succeeds. Unlike
/// `mimi_str_clone`, this status ABI distinguishes a failed empty-string
/// allocation from the valid empty representation and has no artificial
/// length cap.
///
/// # Safety
/// `source` points to `length` readable bytes when length is nonzero. Each
/// non-null output points to one writable, non-overlapping value.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_any_v2_values_unpack_copy_string(
    source: *const std::ffi::c_char,
    length: i64,
    out_pointer: *mut *mut std::ffi::c_char,
    out_length: *mut i64,
) -> i32 {
    if !out_pointer.is_null() {
        unsafe { *out_pointer = std::ptr::null_mut() };
    }
    if !out_length.is_null() {
        unsafe { *out_length = 0 };
    }
    if out_pointer.is_null()
        || out_length.is_null()
        || length < 0
        || (length > 0 && source.is_null())
    {
        return status_code(ManagedAnyStatus::InvalidInput);
    }
    let Ok(length_usize) = usize::try_from(length) else {
        return status_code(ManagedAnyStatus::Capacity);
    };
    if length_usize > isize::MAX as usize {
        return status_code(ManagedAnyStatus::Capacity);
    }

    let bytes = if length_usize == 0 {
        &[][..]
    } else {
        // SAFETY: the ABI contract supplies `length` readable bytes for a
        // non-null source, checked above.
        unsafe { std::slice::from_raw_parts(source.cast::<u8>(), length_usize) }
    };
    let result = (|| {
        ordinary_map_owned_any_v2_values_unpack_string_clone_checkpoint()?;
        let pointer = super::alloc_c_string_from_bytes(bytes);
        if pointer.is_null() {
            return Err(ManagedAnyStatus::Capacity);
        }
        ordinary_map_owned_any_v2_values_unpack_string_publish_checkpoint().map_err(|error| {
            super::mimi_free(pointer.cast());
            error
        })?;
        if let Err(error) =
            super::ordinary_map_domain::register_fresh_owned_string(pointer, length_usize)
        {
            // Registration returns after releasing its registry lock. Retire
            // this unpublished allocation exactly once on every failure.
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
        Ok(pointer)
    })();
    match result {
        Ok(pointer) => {
            unsafe {
                *out_pointer = pointer;
                *out_length = length;
            }
            status_code(ManagedAnyStatus::Ok)
        }
        Err(status) => status_code(status),
    }
}

/// Create an empty V2 ordinary Map root with its exact profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_new(out_root: *mut i64) -> i32 {
    if !out_root.is_null() {
        unsafe { *out_root = 0 };
    }
    if out_root.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    match new_owned_any_v2_root(OrdinaryMapAnyV2Snapshot::new()) {
        Ok(root) => {
            unsafe { *out_root = root };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(error) => map_get_status(ManagedAnyStatus::from(error)),
    }
}

/// Persistent V2 Map Set by cloning a published, descriptor-pinned Any owner
/// after validating membership in the caller's canonical finite registry.
/// The original root and input owner remain live on success or failure.
///
/// # Safety
/// `out_root` is writable; key and selection spans are readable;
/// `selection_hash32` addresses exactly 32 readable bytes. The selection is
/// a canonical version-1 finite descriptor frame sealed by Core. The owner
/// must be a published V2 token whose pinned actual descriptor is a member.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_set(
    handle: i64,
    key: *const u8,
    key_len: i64,
    payload_owner: i64,
    selection_bytes: *const u8,
    selection_len: i64,
    selection_hash32: *const u8,
    out_root: *mut i64,
) -> i32 {
    if !out_root.is_null() {
        unsafe { *out_root = 0 };
    }
    if out_root.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        // Reject a wrong-profile root before key copies or owner callbacks.
        let source = acquire_owned_any_v2_root(handle).map_err(ManagedAnyStatus::from)?;
        let selection_bytes = unsafe { checked_abi_bytes(selection_bytes, selection_len) }
            .map_err(ManagedAnyStatus::from)?;
        if selection_hash32.is_null() {
            return Err(ManagedAnyStatus::InvalidInput);
        }
        let mut selection_hash = [0u8; 32];
        // SAFETY: the reserved ABI requires exactly 32 readable hash bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                selection_hash32,
                selection_hash.as_mut_ptr(),
                selection_hash.len(),
            )
        };
        let selection = super::managed_any_payload_v2::ManagedAnyPayloadV2Selection::parse(
            selection_bytes,
            &selection_hash,
            false,
        )
        .map_err(|error| match error {
            super::managed_any_payload_v2::ManagedAnyPayloadV2SelectionError::InvalidEncoding => {
                ManagedAnyStatus::InvalidInput
            }
            super::managed_any_payload_v2::ManagedAnyPayloadV2SelectionError::RegistryHashMismatch => {
                ManagedAnyStatus::DescriptorMismatch
            }
        })?;
        let key_bytes =
            unsafe { checked_abi_bytes(key, key_len) }.map_err(ManagedAnyStatus::from)?;
        let key = std::str::from_utf8(key_bytes).map_err(|_| ManagedAnyStatus::InvalidInput)?;
        let (_, actual) = managed_v2_token_receipt(payload_owner)?;
        if !selection.contains(actual) {
            return Err(ManagedAnyStatus::DescriptorMismatch);
        }
        let value = OwnedAnyV2::clone_published_owner(payload_owner, actual)?;
        let snapshot = snapshot_any_v2_from_map(&source)
            .map_err(ManagedAnyStatus::from)?
            .set_owned(key, value)
            .map_err(|error| {
                ManagedAnyStatus::from(OrdinaryMapSnapshotRuntimeError::from(error))
            })?;
        drop(source);
        new_owned_any_v2_root(snapshot).map_err(ManagedAnyStatus::from)
    })();
    match result {
        Ok(root) => {
            unsafe { *out_root = root };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(status) => map_get_status(status),
    }
}

/// Return a fresh Any owner on both hit and miss. Absence is status 0 with
/// `found=0` and a fresh canonical i32-zero V2 owner made from the supplied
/// core receipt and compiler-generated i32 glue.
///
/// # Safety
/// Outputs are writable and non-overlapping. Type ID and hash spans are
/// readable and key is valid UTF-8. The miss descriptor must be the exact
/// canonical i32 receipt (tag 1, i32 layout, core TypeDesc hash).
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_get(
    handle: i64,
    key: *const u8,
    key_len: i64,
    out_found: *mut i32,
    out_token: *mut i64,
    miss_tag: i32,
    miss_type_id: *const u8,
    miss_type_id_len: i64,
    miss_descriptor_hash: *const u8,
    miss_size: i64,
    miss_align: i64,
    miss_clone_fn: Option<super::managed_any_payload_v2::ManagedAnyPayloadV2CloneGlue>,
    miss_drop_fn: Option<super::managed_any_payload_v2::ManagedAnyPayloadV2DropGlue>,
) -> i32 {
    if !out_found.is_null() {
        unsafe { *out_found = 0 };
    }
    if !out_token.is_null() {
        unsafe { *out_token = 0 };
    }
    if out_found.is_null() || out_token.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let root = acquire_owned_any_v2_root(handle).map_err(ManagedAnyStatus::from)?;
        let miss = unsafe {
            managed_v2_identity_from_abi(
                miss_tag,
                miss_type_id,
                miss_type_id_len,
                miss_descriptor_hash,
                miss_size,
                miss_align,
            )
        }?;
        if miss.tag != super::managed_any_payload_v2::ManagedAnyPayloadV2Tag::I32
            || miss.size != std::mem::size_of::<i32>()
            || miss.align != std::mem::align_of::<i32>()
        {
            return Err(ManagedAnyStatus::TypeMismatch);
        }
        let glue = super::managed_any_payload_v2::ManagedAnyPayloadV2Glue {
            clone: miss_clone_fn.ok_or(ManagedAnyStatus::InvalidInput)?,
            drop: miss_drop_fn.ok_or(ManagedAnyStatus::InvalidInput)?,
        };
        let key_bytes =
            unsafe { checked_abi_bytes(key, key_len) }.map_err(ManagedAnyStatus::from)?;
        let key = checked_key(key_bytes).map_err(ManagedAnyStatus::from)?;
        managed_allocation_checkpoint()?;
        let snapshot = snapshot_any_v2_from_map(&root)
            .map_err(ManagedAnyStatus::from)?
            .get_any(&key)
            .map_err(|error| {
                ManagedAnyStatus::from(OrdinaryMapSnapshotRuntimeError::from(error))
            })?;
        drop(root);
        let (found, owner) = match snapshot {
            Some(snapshot) => (true, snapshot),
            None => {
                let zero = 0i32;
                let zero_owner =
                    OwnedAnyV2::from_borrowed(miss, glue, (&zero as *const i32).cast::<u8>())
                        .map_err(ManagedAnyStatus::from)?;
                (false, zero_owner)
            }
        };
        ordinary_map_owned_any_v2_get_any_publish_checkpoint()?;
        let owner = owner.publish_managed()?;
        Ok::<_, ManagedAnyStatus>((found, owner))
    })();
    match result {
        Ok((found, owner)) => {
            unsafe {
                *out_found = i32::from(found);
                *out_token = owner;
            }
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(status) => map_get_status(status),
    }
}

/// Create an immutable, key-byte-sorted snapshot list. The returned positive
/// `i64` is an opaque values-list owner handle in its own namespace; it is not
/// a MimiList pointer, a Map handle, or a ManagedAny token. Every list element
/// owns a fresh clone of the exact source descriptor. The list remains valid
/// after source Map updates or destruction.
///
/// # Safety
/// `out_values` points to one writable `i64`. `handle` must be a live V2 Map
/// root created by this profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_values(
    handle: i64,
    out_values: *mut i64,
) -> i32 {
    if !out_values.is_null() {
        unsafe { *out_values = 0 };
    }
    if out_values.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let root = acquire_owned_any_v2_root(handle).map_err(ManagedAnyStatus::from)?;
        let values = snapshot_any_v2_from_map(&root)
            .map_err(ManagedAnyStatus::from)?
            .values()
            .map_err(|error| {
                ManagedAnyStatus::from(OrdinaryMapSnapshotRuntimeError::from(error))
            })?;
        drop(root);
        publish_v2_values_snapshot(OrdinaryMapAnyV2ValuesSnapshot { values })
    })();
    match result {
        Ok(values) => {
            unsafe { *out_values = values };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(status) => map_get_status(status),
    }
}

/// Read the length of a V2 values snapshot. Overflow is a status error, never
/// a wrapped or truncated element count.
///
/// # Safety
/// `out_len` points to one writable `i32`; `values` is a live opaque values
/// snapshot handle returned by `mimi_ordinary_map_owned_any_v2_values`.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_values_len(
    values: i64,
    out_len: *mut i32,
) -> i32 {
    if !out_len.is_null() {
        unsafe { *out_len = 0 };
    }
    if out_len.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = acquire_v2_values_snapshot(values).and_then(|snapshot| {
        i32::try_from(snapshot.values.len()).map_err(|_| ManagedAnyStatus::SizeOverflow)
    });
    match result {
        Ok(len) => {
            unsafe { *out_len = len };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(status) => map_get_status(status),
    }
}

/// Return an independently published owner clone at `index`. An out-of-range
/// index is status 0 with `found=0` and `out_owner=0`; a present zero/empty
/// payload remains `found=1` with a nonzero owner carrying its exact descriptor.
///
/// # Safety
/// Both outputs are writable and non-overlapping. `values` is a live opaque
/// values snapshot handle. Negative indices are rejected as InvalidInput.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_values_get(
    values: i64,
    index: i64,
    out_found: *mut i32,
    out_owner: *mut i64,
) -> i32 {
    if !out_found.is_null() {
        unsafe { *out_found = 0 };
    }
    if !out_owner.is_null() {
        unsafe { *out_owner = 0 };
    }
    if out_found.is_null() || out_owner.is_null() || index < 0 {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let snapshot = acquire_v2_values_snapshot(values)?;
        let index = usize::try_from(index).map_err(|_| ManagedAnyStatus::InvalidInput)?;
        let Some(value) = snapshot.values.get(index) else {
            return Ok::<_, ManagedAnyStatus>(None);
        };
        let owner = value.try_clone().map_err(ManagedAnyStatus::from)?;
        owner.publish_managed().map(Some)
    })();
    match result {
        Ok(Some(owner)) => {
            unsafe {
                *out_found = 1;
                *out_owner = owner;
            }
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Ok(None) => ORDINARY_MAP_OWNED_ANY_V2_OK,
        Err(status) => map_get_status(status),
    }
}

/// Clone a values snapshot into a new independent snapshot-list owner. All
/// element clone stages complete before the output handle is published.
///
/// # Safety
/// `out_values` points to a writable `i64`; `values` is a live V2 values
/// snapshot handle.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_values_clone(
    values: i64,
    out_values: *mut i64,
) -> i32 {
    if !out_values.is_null() {
        unsafe { *out_values = 0 };
    }
    if out_values.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = acquire_v2_values_snapshot(values)
        .map_err(|status| status)
        .and_then(|snapshot| {
            snapshot.try_clone().map_err(|error| {
                ManagedAnyStatus::from(OrdinaryMapSnapshotRuntimeError::from(error))
            })
        })
        .and_then(publish_v2_values_snapshot);
    match result {
        Ok(clone) => {
            unsafe { *out_values = clone };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(status) => map_get_status(status),
    }
}

/// Consume exactly one values snapshot owner. Element Drop callbacks run after
/// detaching the snapshot from the handle registry.
#[no_mangle]
pub extern "C" fn mimi_ordinary_map_owned_any_v2_values_drop(values: i64) -> i32 {
    match retire_v2_values_snapshot(values) {
        Ok(()) => ORDINARY_MAP_OWNED_ANY_V2_OK,
        Err(status) => map_get_status(status),
    }
}

struct StagedManagedAnyV2List(*mut super::MimiList);

impl StagedManagedAnyV2List {
    fn new() -> Result<Self, ManagedAnyStatus> {
        snapshot_allocation_checkpoint().map_err(|_| ManagedAnyStatus::Capacity)?;
        let list = super::mimi_mir_list_new_managed_any_v2();
        if list.is_null() {
            return Err(ManagedAnyStatus::Capacity);
        }
        Ok(Self(list))
    }

    fn push_owner(&mut self, owner: i64) -> Result<(), ManagedAnyStatus> {
        // Safety: this guard owns the list exclusively. A successful push
        // transfers this already-published V2 owner to the list.
        if unsafe { super::mimi_mir_list_push_managed_any_v2(self.0, owner) } != 0 {
            return Ok(());
        }
        let cleanup_status = super::mimi_any_payload_v2_drop(owner);
        if cleanup_status != ORDINARY_MAP_OWNED_ANY_V2_OK {
            return Err(managed_any_status_from_code(cleanup_status));
        }
        Err(ManagedAnyStatus::Capacity)
    }

    fn publish(mut self) -> *mut super::MimiList {
        let list = self.0;
        self.0 = std::ptr::null_mut();
        list
    }
}

impl Drop for StagedManagedAnyV2List {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this guard is the unique owner of a valid V2 list. The
            // V2 drop preflights every prefix token before releasing any.
            unsafe { super::mimi_mir_list_drop_managed_any_v2(self.0) };
        }
    }
}

/// Materialize the Map's key-sorted V2 Values snapshot as an independently
/// owned `MimiList<ManagedAnyV2>`. The canonical Core selection frame/hash is
/// checked before any element token is published. Each unpublished snapshot
/// value remains RAII-owned until its successful list append transfers the
/// obligation; any failure drops the list prefix, current token, and remaining
/// snapshot values before returning with `out_list == null`.
///
/// The selection schema records `(tag, type_id, descriptor_hash)`. Each value
/// is first checked against its complete runtime V2 receipt, including
/// `(size, align)`, then matched against that Core-selected record. The
/// descriptor hash is the Core TypeDesc-closure authority for layout,
/// cloneability, and drop behavior; its closure binds size/alignment and the
/// payload's clone/drop dependencies even though the frame does not repeat
/// those fields.
///
/// # Safety
/// `selection_bytes` spans `selection_len` readable bytes, `selection_hash32`
/// spans 32 readable bytes, and `out_list` points to one writable pointer.
/// The output does not overlap either input. `handle` is a live V2 Map root.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_values_list(
    handle: i64,
    selection_bytes: *const u8,
    selection_len: i64,
    selection_hash32: *const u8,
    out_list: *mut *mut super::MimiList,
) -> i32 {
    if !out_list.is_null() {
        unsafe { *out_list = std::ptr::null_mut() };
    }
    if out_list.is_null() || selection_hash32.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let selection_bytes = unsafe { checked_abi_bytes(selection_bytes, selection_len) }
            .map_err(ManagedAnyStatus::from)?;
        let mut selection_hash = [0u8; 32];
        // SAFETY: the caller supplies the fixed 32-byte canonical registry hash.
        unsafe {
            std::ptr::copy_nonoverlapping(
                selection_hash32,
                selection_hash.as_mut_ptr(),
                selection_hash.len(),
            )
        };
        let selection = super::managed_any_payload_v2::ManagedAnyPayloadV2Selection::parse(
            selection_bytes,
            &selection_hash,
            true,
        )
        .map_err(|error| match error {
            super::managed_any_payload_v2::ManagedAnyPayloadV2SelectionError::InvalidEncoding => {
                ManagedAnyStatus::InvalidInput
            }
            super::managed_any_payload_v2::ManagedAnyPayloadV2SelectionError::RegistryHashMismatch => {
                ManagedAnyStatus::DescriptorMismatch
            }
        })?;

        let root = acquire_owned_any_v2_root(handle).map_err(ManagedAnyStatus::from)?;
        let values = snapshot_any_v2_from_map(&root)
            .map_err(ManagedAnyStatus::from)?
            .values()
            .map_err(|error| {
                ManagedAnyStatus::from(OrdinaryMapSnapshotRuntimeError::from(error))
            })?;
        // The sorted Values owners are independent clones now; releasing this
        // lease before selector checks and callbacks keeps root access short.
        drop(root);

        for value in &values {
            let identity = value.identity().map_err(ManagedAnyStatus::from)?;
            if !selection.contains(identity) {
                return Err(ManagedAnyStatus::DescriptorMismatch);
            }
        }

        let mut list = StagedManagedAnyV2List::new()?;
        for value in values {
            let owner = value.publish_managed()?;
            list.push_owner(owner)?;
        }
        ordinary_map_owned_any_v2_values_publish_checkpoint()?;
        Ok::<_, ManagedAnyStatus>(list.publish())
    })();
    match result {
        Ok(list) => {
            unsafe { *out_list = list };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(status) => map_get_status(status),
    }
}

/// Return a persistent root with the selected key removed. The source stays
/// live and unchanged; a missing key still yields a distinct empty-equivalent
/// snapshot root.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_remove(
    handle: i64,
    key: *const u8,
    key_len: i64,
    out_root: *mut i64,
) -> i32 {
    if !out_root.is_null() {
        unsafe { *out_root = 0 };
    }
    if out_root.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let root = acquire_owned_any_v2_root(handle).map_err(ManagedAnyStatus::from)?;
        let key_bytes =
            unsafe { checked_abi_bytes(key, key_len) }.map_err(ManagedAnyStatus::from)?;
        let key = checked_key(key_bytes).map_err(ManagedAnyStatus::from)?;
        let snapshot = snapshot_any_v2_from_map(&root)
            .map_err(ManagedAnyStatus::from)?
            .remove(&key)
            .map_err(|error| {
                ManagedAnyStatus::from(OrdinaryMapSnapshotRuntimeError::from(error))
            })?;
        drop(root);
        new_owned_any_v2_root(snapshot).map_err(ManagedAnyStatus::from)
    })();
    match result {
        Ok(root) => {
            unsafe { *out_root = root };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(status) => map_get_status(status),
    }
}

#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_clone(
    handle: i64,
    out_root: *mut i64,
) -> i32 {
    if !out_root.is_null() {
        unsafe { *out_root = 0 };
    }
    if out_root.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    match clone_owned_any_v2_root(handle) {
        Ok(root) => {
            unsafe { *out_root = root };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(error) => map_get_status(ManagedAnyStatus::from(error)),
    }
}

#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_size(
    handle: i64,
    out_size: *mut i32,
) -> i32 {
    if !out_size.is_null() {
        unsafe { *out_size = 0 };
    }
    if out_size.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let root = acquire_owned_any_v2_root(handle)?;
        snapshot_any_v2_from_map(&root)?
            .size_i32()
            .map_err(|_| OrdinaryMapSnapshotRuntimeError::SizeOverflow)
    })();
    match result {
        Ok(size) => {
            unsafe { *out_size = size };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(error) => map_get_status(ManagedAnyStatus::from(error)),
    }
}

#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_contains(
    handle: i64,
    key: *const u8,
    key_len: i64,
    out_contains: *mut i32,
) -> i32 {
    if !out_contains.is_null() {
        unsafe { *out_contains = 0 };
    }
    if out_contains.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let root = acquire_owned_any_v2_root(handle)?;
        let key_bytes = unsafe { checked_abi_bytes(key, key_len) }?;
        let key = std::str::from_utf8(key_bytes)
            .map_err(|_| OrdinaryMapSnapshotRuntimeError::InvalidInput)?;
        Ok::<_, OrdinaryMapSnapshotRuntimeError>(snapshot_any_v2_from_map(&root)?.contains(key))
    })();
    match result {
        Ok(contains) => {
            unsafe { *out_contains = i32::from(contains) };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(error) => map_get_status(ManagedAnyStatus::from(error)),
    }
}

/// Read whether an exact V2 root is empty. The root remains live and
/// unchanged; errors never become `true` or `false` because the status is
/// checked separately by the compiler-native adapter.
///
/// # Safety
/// `out_is_empty` points to one writable `i32`. `root` must be a live root
/// created by this profile.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_is_empty(
    handle: i64,
    out_is_empty: *mut i32,
) -> i32 {
    if !out_is_empty.is_null() {
        unsafe { *out_is_empty = 0 };
    }
    if out_is_empty.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let root = acquire_owned_any_v2_root(handle)?;
        Ok::<_, OrdinaryMapSnapshotRuntimeError>(snapshot_any_v2_from_map(&root)?.is_empty())
    })();
    match result {
        Ok(is_empty) => {
            unsafe { *out_is_empty = i32::from(is_empty) };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(error) => map_get_status(ManagedAnyStatus::from(error)),
    }
}

#[no_mangle]
pub extern "C" fn mimi_ordinary_map_owned_any_v2_drop(handle: i64) -> i32 {
    match drop_owned_any_v2_root(handle) {
        Ok(()) => ORDINARY_MAP_OWNED_ANY_V2_OK,
        Err(error) => map_get_status(ManagedAnyStatus::from(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::blake3::Blake3Hasher;
    use super::{ManagedAnyPayloadV2Identity, ManagedAnyStatus, OrdinaryMapSnapshot, OwnedAny};
    use crate::runtime::any_owner::{self, AnyOwnerKind, AnyOwnerToken};

    const V2_I32_TYPE_ID: &[u8; 35] = b"rt:11111111111111111111111111111111";
    const V2_BOOL_TYPE_ID: &[u8; 35] = b"rt:0123456789abcdef0123456789abcdef";
    const V2_AGGREGATE_TYPE_ID: &[u8; 35] = b"rt:fedcba9876543210fedcba9876543210";
    const V2_I32_HASH: [u8; 32] = [11; 32];
    const V2_BOOL_HASH: [u8; 32] = [51; 32];
    const V2_TEST_FAILURE_TYPE_ID: &[u8; 35] = b"rt:22222222222222222222222222222222";
    const V2_TEST_FAILURE_HASH: [u8; 32] = [191; 32];
    const V2_GET_ANY_TEST_TYPE_ID: &[u8; 35] = b"rt:33333333333333333333333333333333";
    const V2_GET_ANY_TEST_HASH: [u8; 32] = [203; 32];

    thread_local! {
        static V2_GET_ANY_COUNTED_DROPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    static V2_VALUES_CLONE_FAIL: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    static V2_VALUES_DROP_CALLS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "C" fn clone_v2_byte(destination: *mut u8, source: *const u8) -> i32 {
        unsafe { *destination = *source };
        0
    }

    unsafe extern "C" fn drop_v2_byte(_payload: *mut u8) {}

    unsafe extern "C" fn drop_v2_get_any_counted(_payload: *mut u8) {
        V2_GET_ANY_COUNTED_DROPS.with(|drops| drops.set(drops.get().saturating_add(1)));
    }

    unsafe extern "C" fn clone_v2_i32(destination: *mut u8, source: *const u8) -> i32 {
        unsafe {
            std::ptr::copy_nonoverlapping(source.cast::<i32>(), destination.cast::<i32>(), 1)
        };
        0
    }

    unsafe extern "C" fn drop_v2_i32(_payload: *mut u8) {}

    unsafe extern "C" fn clone_v2_toggle_failure(destination: *mut u8, source: *const u8) -> i32 {
        if V2_VALUES_CLONE_FAIL.load(std::sync::atomic::Ordering::SeqCst) {
            return 91;
        }
        unsafe { *destination = *source };
        0
    }

    unsafe extern "C" fn drop_v2_values_counted(_payload: *mut u8) {
        V2_VALUES_DROP_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn new_v2_map() -> i64 {
        let mut root = 0;
        assert_eq!(
            unsafe { super::mimi_ordinary_map_owned_any_v2_new(&mut root) },
            0
        );
        assert_ne!(root, 0);
        root
    }

    fn new_v2_bool_owner(value: u8, hash: &[u8; 32]) -> i64 {
        let mut owner = 0;
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_new(
                    3,
                    V2_BOOL_TYPE_ID.as_ptr(),
                    V2_BOOL_TYPE_ID.len() as i64,
                    hash.as_ptr(),
                    &value,
                    1,
                    1,
                    Some(clone_v2_byte),
                    Some(drop_v2_byte),
                    &mut owner,
                )
            },
            0
        );
        assert_ne!(owner, 0);
        owner
    }

    fn new_v2_aggregate_owner(value: u8, hash: &[u8; 32]) -> i64 {
        let mut owner = 0;
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_new(
                    5,
                    V2_AGGREGATE_TYPE_ID.as_ptr(),
                    V2_AGGREGATE_TYPE_ID.len() as i64,
                    hash.as_ptr(),
                    &value,
                    1,
                    1,
                    Some(clone_v2_byte),
                    Some(drop_v2_byte),
                    &mut owner,
                )
            },
            0
        );
        assert_ne!(owner, 0);
        owner
    }

    fn new_v2_i32_owner(value: i32) -> i64 {
        let mut owner = 0;
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_new(
                    1,
                    V2_I32_TYPE_ID.as_ptr(),
                    V2_I32_TYPE_ID.len() as i64,
                    V2_I32_HASH.as_ptr(),
                    (&value as *const i32).cast::<u8>(),
                    std::mem::size_of::<i32>() as i64,
                    std::mem::align_of::<i32>() as i64,
                    Some(clone_v2_i32),
                    Some(drop_v2_i32),
                    &mut owner,
                )
            },
            0
        );
        assert_ne!(owner, 0);
        owner
    }

    fn new_v2_get_any_record_owner(value: u8) -> i64 {
        let mut owner = 0;
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_new(
                    5,
                    V2_GET_ANY_TEST_TYPE_ID.as_ptr(),
                    V2_GET_ANY_TEST_TYPE_ID.len() as i64,
                    V2_GET_ANY_TEST_HASH.as_ptr(),
                    &value,
                    1,
                    1,
                    Some(clone_v2_byte),
                    Some(drop_v2_get_any_counted),
                    &mut owner,
                )
            },
            0
        );
        assert_ne!(owner, 0);
        owner
    }

    fn set_v2_owner(
        root: i64,
        key: &[u8],
        owner: i64,
        tag: i32,
        type_id: &[u8; 35],
        hash: &[u8; 32],
    ) -> i64 {
        let (selection_bytes, selection_hash) = selection(tag, type_id, hash);
        let mut new_root = 0;
        assert_eq!(
            unsafe {
                super::mimi_ordinary_map_owned_any_v2_set(
                    root,
                    key.as_ptr(),
                    key.len() as i64,
                    owner,
                    selection_bytes.as_ptr(),
                    selection_bytes.len() as i64,
                    selection_hash.as_ptr(),
                    &mut new_root,
                )
            },
            0
        );
        assert_ne!(new_root, 0);
        new_root
    }

    fn values_v2(root: i64) -> (i32, i64) {
        let mut values = -1;
        let status = unsafe { super::mimi_ordinary_map_owned_any_v2_values(root, &mut values) };
        (status, values)
    }

    fn values_len_v2(values: i64) -> (i32, i32) {
        let mut len = -1;
        let status = unsafe { super::mimi_ordinary_map_owned_any_v2_values_len(values, &mut len) };
        (status, len)
    }

    fn values_get_v2(values: i64, index: i64) -> (i32, i32, i64) {
        let mut found = -1;
        let mut owner = -1;
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_values_get(values, index, &mut found, &mut owner)
        };
        (status, found, owner)
    }

    fn values_list_v2(
        root: i64,
        selection_bytes: &[u8],
        selection_hash: &[u8; 32],
    ) -> (i32, *mut super::super::MimiList) {
        let mut list = std::ptr::null_mut();
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_values_list(
                root,
                selection_bytes.as_ptr(),
                selection_bytes.len() as i64,
                selection_hash.as_ptr(),
                &mut list,
            )
        };
        (status, list)
    }

    fn unpack_v2_byte(owner: i64, expected: ManagedAnyPayloadV2Identity) -> u8 {
        let mut output = std::mem::MaybeUninit::<u8>::uninit();
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_unpack(
                    owner,
                    expected.tag as i32,
                    expected.type_id.as_bytes().as_ptr(),
                    expected.type_id.as_bytes().len() as i64,
                    expected.descriptor_hash.as_ptr(),
                    output.as_mut_ptr(),
                    expected.size as i64,
                    expected.align as i64,
                )
            },
            0
        );
        unsafe { output.assume_init() }
    }

    fn unpack_v2_i32(owner: i64) -> i32 {
        let mut output = std::mem::MaybeUninit::<i32>::uninit();
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_unpack(
                    owner,
                    1,
                    V2_I32_TYPE_ID.as_ptr(),
                    V2_I32_TYPE_ID.len() as i64,
                    V2_I32_HASH.as_ptr(),
                    output.as_mut_ptr().cast::<u8>(),
                    std::mem::size_of::<i32>() as i64,
                    std::mem::align_of::<i32>() as i64,
                )
            },
            0
        );
        unsafe { output.assume_init() }
    }

    #[test]
    fn owned_any_v2_unpack_capacity_keeps_destination_and_source_owner_unchanged() {
        let owner = new_v2_i32_owner(37);
        let sentinel = 0x1357_2468i32;
        let wrong_tag_sentinel = 0xa5u8;
        let mut wrong_tag_output = wrong_tag_sentinel;
        let mut output = sentinel;
        let (mismatch_status, attempts_after_mismatch, status, attempts) =
            super::with_ordinary_map_owned_any_v2_unpack_failure_for_test(0, || {
                let mismatch_status = unsafe {
                    super::mimi_any_payload_v2_unpack(
                        owner,
                        3,
                        V2_BOOL_TYPE_ID.as_ptr(),
                        V2_BOOL_TYPE_ID.len() as i64,
                        V2_BOOL_HASH.as_ptr(),
                        &mut wrong_tag_output,
                        1,
                        1,
                    )
                };
                let attempts_after_mismatch =
                    super::ordinary_map_owned_any_v2_unpack_attempts_for_test();
                let status = unsafe {
                    super::mimi_any_payload_v2_unpack(
                        owner,
                        1,
                        V2_I32_TYPE_ID.as_ptr(),
                        V2_I32_TYPE_ID.len() as i64,
                        V2_I32_HASH.as_ptr(),
                        (&mut output as *mut i32).cast::<u8>(),
                        std::mem::size_of::<i32>() as i64,
                        std::mem::align_of::<i32>() as i64,
                    )
                };
                (
                    mismatch_status,
                    attempts_after_mismatch,
                    status,
                    super::ordinary_map_owned_any_v2_unpack_attempts_for_test(),
                )
            });

        assert_eq!(mismatch_status, ManagedAnyStatus::TypeMismatch as i32);
        assert_eq!(wrong_tag_output, wrong_tag_sentinel);
        assert_eq!(
            attempts_after_mismatch, 0,
            "a wrong TypeDesc must fail before the Capacity boundary"
        );
        assert_eq!(status, ManagedAnyStatus::Capacity as i32);
        assert_eq!(
            output, sentinel,
            "failure must leave destination bytes intact"
        );
        assert_eq!(
            attempts, 1,
            "the exact typed Unpack boundary was reached once"
        );
        assert_eq!(
            unpack_v2_i32(owner),
            37,
            "the borrowed source owner survives"
        );
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
    }

    #[test]
    fn v2_values_unpack_selected_owner_capacity_keeps_outputs_empty_and_source_live() {
        let empty = new_v2_map();
        let owner = new_v2_i32_owner(37);
        let root = set_v2_owner(empty, b"number", owner, 1, V2_I32_TYPE_ID, &V2_I32_HASH);
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
        let (selection_bytes, selection_hash) = selection(1, V2_I32_TYPE_ID, &V2_I32_HASH);
        let (values_status, list) = values_list_v2(root, &selection_bytes, &selection_hash);
        assert_eq!(values_status, super::ORDINARY_MAP_OWNED_ANY_V2_OK);
        assert!(!list.is_null());

        let (status, found, selected, attempts) =
            super::with_ordinary_map_owned_any_v2_values_unpack_owner_failure_for_test(0, || {
                let mut found = -1;
                let mut selected = -1;
                let status = unsafe {
                    super::super::mimi_mir_list_get_managed_any_v2(
                        list,
                        0,
                        &mut found,
                        &mut selected,
                    )
                };
                (
                    status,
                    found,
                    selected,
                    super::ordinary_map_owned_any_v2_values_unpack_owner_attempts_for_test(),
                )
            });
        assert_eq!(status, super::MIMI_MAP_ANY_OWNER_V2_CAPACITY);
        assert_eq!((found, selected), (0, 0));
        assert_eq!(attempts, 1);
        assert_eq!(size_v2(root), (super::ORDINARY_MAP_OWNED_ANY_V2_OK, 1));

        let mut found = 0;
        let mut selected = 0;
        assert_eq!(
            unsafe {
                super::super::mimi_mir_list_get_managed_any_v2(list, 0, &mut found, &mut selected)
            },
            super::ORDINARY_MAP_OWNED_ANY_V2_OK
        );
        assert_eq!(found, 1);
        assert_eq!(unpack_v2_i32(selected), 37);
        assert_eq!(super::mimi_any_payload_v2_drop(selected), 0);
        unsafe { super::super::mimi_mir_list_drop_managed_any_v2(list) };
        drop_v2_map(empty);
        drop_v2_map(root);
    }

    #[test]
    fn v2_values_unpack_string_copy_is_transactional_for_empty_nul_and_unicode() {
        let bytes = b"before\0\xe9\x9b\xaa";
        let counts_before = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains before copy");
        let mut pointer = std::ptr::null_mut();
        let mut length = -1;
        let status =
            super::with_ordinary_map_owned_any_v2_values_unpack_string_clone_failure_for_test(
                || unsafe {
                    super::mimi_mir_ordinary_map_owned_any_v2_values_unpack_copy_string(
                        bytes.as_ptr().cast(),
                        bytes.len() as i64,
                        &mut pointer,
                        &mut length,
                    )
                },
            );
        assert_eq!(status, ManagedAnyStatus::Capacity as i32);
        assert!(pointer.is_null());
        assert_eq!(length, 0);

        let status =
            super::with_ordinary_map_owned_any_v2_values_unpack_string_publish_failure_for_test(
                || unsafe {
                    super::mimi_mir_ordinary_map_owned_any_v2_values_unpack_copy_string(
                        bytes.as_ptr().cast(),
                        bytes.len() as i64,
                        &mut pointer,
                        &mut length,
                    )
                },
            );
        assert_eq!(status, ManagedAnyStatus::Capacity as i32);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after failed copy")
                .owned_strings,
            counts_before.owned_strings,
            "failed publication retires the fresh allocation"
        );

        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_any_v2_values_unpack_copy_string(
                    bytes.as_ptr().cast(),
                    bytes.len() as i64,
                    &mut pointer,
                    &mut length,
                )
            },
            ManagedAnyStatus::Ok as i32
        );
        assert!(!pointer.is_null());
        assert_eq!(length, bytes.len() as i64);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) },
            bytes
        );
        unsafe { super::super::mimi_string_free(pointer) };

        pointer = std::ptr::null_mut();
        length = -1;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_any_v2_values_unpack_copy_string(
                    std::ptr::null(),
                    0,
                    &mut pointer,
                    &mut length,
                )
            },
            ManagedAnyStatus::Ok as i32
        );
        assert!(
            !pointer.is_null(),
            "empty output has explicit allocation status"
        );
        assert_eq!(length, 0);
        unsafe { super::super::mimi_string_free(pointer) };
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after successful copies")
                .owned_strings,
            counts_before.owned_strings
        );
    }

    fn selection(tag: i32, type_id: &[u8; 35], hash: &[u8; 32]) -> (Vec<u8>, [u8; 32]) {
        let mut bytes = Vec::with_capacity(8 + 8 + type_id.len() + 4 + hash.len());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&(type_id.len() as u64).to_be_bytes());
        bytes.extend_from_slice(type_id);
        bytes.extend_from_slice(&tag.to_be_bytes());
        bytes.extend_from_slice(hash);
        let mut hasher = Blake3Hasher::new();
        hasher.update(b"mimi-managed-any-payload-v2:admitted-descriptor-set:v1\n");
        hasher.update(&bytes[8..]);
        (bytes, hasher.finalize())
    }

    fn selection_records(records: &[(i32, &[u8; 35], &[u8; 32])]) -> (Vec<u8>, [u8; 32]) {
        let mut sorted = records.to_vec();
        sorted.sort_unstable_by(|left, right| left.1.cmp(right.1));
        let mut bytes = Vec::with_capacity(8 + sorted.len() * (8 + 35 + 4 + 32));
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&(sorted.len() as u32).to_be_bytes());
        let mut hasher = Blake3Hasher::new();
        hasher.update(b"mimi-managed-any-payload-v2:admitted-descriptor-set:v1\n");
        for (tag, type_id, hash) in sorted {
            bytes.extend_from_slice(&(type_id.len() as u64).to_be_bytes());
            bytes.extend_from_slice(type_id);
            bytes.extend_from_slice(&tag.to_be_bytes());
            bytes.extend_from_slice(hash);
            hasher.update(&(type_id.len() as u64).to_be_bytes());
            hasher.update(type_id);
            hasher.update(&tag.to_be_bytes());
            hasher.update(hash);
        }
        (bytes, hasher.finalize())
    }

    fn bool_selection(hash: &[u8; 32]) -> (Vec<u8>, [u8; 32]) {
        selection(3, V2_BOOL_TYPE_ID, hash)
    }

    fn set_v2_bool(root: i64, key: &[u8], owner: i64, hash: &[u8; 32]) -> i64 {
        let (selection, selection_hash) = bool_selection(hash);
        let mut new_root = 0;
        assert_eq!(
            unsafe {
                super::mimi_ordinary_map_owned_any_v2_set(
                    root,
                    key.as_ptr(),
                    key.len() as i64,
                    owner,
                    selection.as_ptr(),
                    selection.len() as i64,
                    selection_hash.as_ptr(),
                    &mut new_root,
                )
            },
            0
        );
        assert_ne!(new_root, 0);
        new_root
    }

    #[test]
    fn set_requires_owner_descriptor_in_hashed_selection_before_mutation() {
        let root = new_v2_map();
        let owner = new_v2_bool_owner(0, &[51; 32]);
        let (mut selection, selection_hash) = bool_selection(&[52; 32]);
        let mut out_root = -1;
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_set(
                root,
                b"flag".as_ptr(),
                4,
                owner,
                selection.as_mut_ptr(),
                selection.len() as i64,
                selection_hash.as_ptr(),
                &mut out_root,
            )
        };
        assert_eq!(status, 7);
        assert_eq!(out_root, 0);
        assert_eq!(
            any_owner::validate(AnyOwnerToken::from_wire_bits(owner as u64).unwrap()),
            Ok(AnyOwnerKind::ManagedV2)
        );
        assert_eq!(size_v2(root), (0, 0));
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
        assert_eq!(super::mimi_ordinary_map_owned_any_v2_drop(root), 0);
    }

    fn get_v2(root: i64, key: &[u8]) -> (i32, i32, i64) {
        let mut found = -1;
        let mut owner = -1;
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_get(
                root,
                key.as_ptr(),
                key.len() as i64,
                &mut found,
                &mut owner,
                1,
                V2_I32_TYPE_ID.as_ptr(),
                V2_I32_TYPE_ID.len() as i64,
                V2_I32_HASH.as_ptr(),
                std::mem::size_of::<i32>() as i64,
                std::mem::align_of::<i32>() as i64,
                Some(clone_v2_i32),
                Some(drop_v2_i32),
            )
        };
        (status, found, owner)
    }

    fn size_v2(root: i64) -> (i32, i32) {
        let mut size = -1;
        let status = unsafe { super::mimi_ordinary_map_owned_any_v2_size(root, &mut size) };
        (status, size)
    }

    fn is_empty_v2(root: i64) -> (i32, i32) {
        let mut is_empty = -1;
        let status = unsafe { super::mimi_ordinary_map_owned_any_v2_is_empty(root, &mut is_empty) };
        (status, is_empty)
    }

    fn contains_v2(root: i64, key: &[u8]) -> (i32, i32) {
        let mut contains = -1;
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_contains(
                root,
                key.as_ptr(),
                key.len() as i64,
                &mut contains,
            )
        };
        (status, contains)
    }

    fn drop_v2_map(root: i64) {
        assert_eq!(super::mimi_ordinary_map_owned_any_v2_drop(root), 0);
    }

    #[test]
    fn owned_any_v2_root_publication_capacity_preserves_sources_for_set_remove_and_clone() {
        let owner_baseline = any_owner::live_count().expect("owner registry baseline");
        let empty = new_v2_map();
        let original_owner = new_v2_i32_owner(7);
        let populated = set_v2_owner(
            empty,
            b"key",
            original_owner,
            1,
            V2_I32_TYPE_ID,
            &V2_I32_HASH,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(original_owner), 0);
        assert_eq!(size_v2(empty), (0, 0));
        assert_eq!(size_v2(populated), (0, 1));
        assert_eq!(contains_v2(populated, b"key"), (0, 1));

        let replacement_owner = new_v2_i32_owner(9);
        let (selection_bytes, selection_hash) = selection(1, V2_I32_TYPE_ID, &V2_I32_HASH);
        let mut set_output = i64::MAX;
        let (set_status, set_attempts) =
            super::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
                let status = unsafe {
                    super::mimi_ordinary_map_owned_any_v2_set(
                        populated,
                        b"next".as_ptr(),
                        4,
                        replacement_owner,
                        selection_bytes.as_ptr(),
                        selection_bytes.len() as i64,
                        selection_hash.as_ptr(),
                        &mut set_output,
                    )
                };
                (
                    status,
                    super::ordinary_map_owned_any_v2_root_publish_attempts_for_test(),
                )
            });
        assert_eq!(set_status, ManagedAnyStatus::Capacity as i32);
        assert_eq!(set_output, 0, "failed Set must not publish a root handle");
        assert_eq!(set_attempts, 1);
        assert_eq!(size_v2(populated), (0, 1));
        assert_eq!(contains_v2(populated, b"key"), (0, 1));
        assert_eq!(contains_v2(populated, b"next"), (0, 0));
        assert_eq!(super::mimi_any_payload_v2_drop(replacement_owner), 0);

        let mut remove_output = i64::MAX;
        let (remove_status, remove_attempts) =
            super::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
                let status = unsafe {
                    super::mimi_ordinary_map_owned_any_v2_remove(
                        populated,
                        b"key".as_ptr(),
                        3,
                        &mut remove_output,
                    )
                };
                (
                    status,
                    super::ordinary_map_owned_any_v2_root_publish_attempts_for_test(),
                )
            });
        assert_eq!(remove_status, ManagedAnyStatus::Capacity as i32);
        assert_eq!(
            remove_output, 0,
            "failed Remove must not publish a root handle"
        );
        assert_eq!(remove_attempts, 1);
        assert_eq!(size_v2(populated), (0, 1));
        assert_eq!(contains_v2(populated, b"key"), (0, 1));

        let mut clone_output = i64::MAX;
        let (clone_status, clone_attempts) =
            super::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
                let status = unsafe {
                    super::mimi_ordinary_map_owned_any_v2_clone(populated, &mut clone_output)
                };
                (
                    status,
                    super::ordinary_map_owned_any_v2_root_publish_attempts_for_test(),
                )
            });
        assert_eq!(clone_status, ManagedAnyStatus::Capacity as i32);
        assert_eq!(
            clone_output, 0,
            "failed Clone must not publish a root handle"
        );
        assert_eq!(clone_attempts, 1);
        assert_eq!(size_v2(populated), (0, 1));
        assert_eq!(contains_v2(populated, b"key"), (0, 1));

        drop_v2_map(empty);
        drop_v2_map(populated);
        assert_eq!(any_owner::live_count(), Ok(owner_baseline));
    }

    #[test]
    fn owned_any_v2_get_any_publication_capacity_preserves_source_and_outputs() {
        let empty = new_v2_map();
        let short_key = b"k";
        let long_key = "k\0雪".as_bytes();
        let i32_payload = new_v2_i32_owner(37);
        let with_i32 = set_v2_owner(
            empty,
            short_key,
            i32_payload,
            1,
            V2_I32_TYPE_ID,
            &V2_I32_HASH,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(i32_payload), 0);
        let record_payload = new_v2_get_any_record_owner(99);
        let populated = set_v2_owner(
            with_i32,
            long_key,
            record_payload,
            5,
            V2_GET_ANY_TEST_TYPE_ID,
            &V2_GET_ANY_TEST_HASH,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(record_payload), 0);
        let record_drops_before_failure = V2_GET_ANY_COUNTED_DROPS.with(std::cell::Cell::get);
        assert_eq!(size_v2(populated), (0, 2));
        assert_eq!(contains_v2(populated, short_key), (0, 1));
        assert_eq!(contains_v2(populated, long_key), (0, 1));
        assert_eq!(contains_v2(populated, b"k\0"), (0, 0));

        for query in [short_key, long_key, "k\0🧪".as_bytes()] {
            let ((status, found, owner), attempts) =
                super::with_ordinary_map_owned_any_v2_get_any_publish_failure_for_test(0, || {
                    let result = get_v2(populated, query);
                    let attempts =
                        super::ordinary_map_owned_any_v2_get_any_publish_attempts_for_test();
                    (result, attempts)
                });
            assert_eq!(status, ManagedAnyStatus::Capacity as i32);
            assert_eq!(found, 0, "failure must not publish the found flag");
            assert_eq!(owner, 0, "failure must not publish an owner handle");
            assert_eq!(attempts, 1);
            assert_eq!(size_v2(populated), (0, 2));
            assert_eq!(contains_v2(populated, short_key), (0, 1));
            assert_eq!(contains_v2(populated, long_key), (0, 1));
        }
        assert_eq!(
            V2_GET_ANY_COUNTED_DROPS.with(std::cell::Cell::get),
            record_drops_before_failure + 1,
            "the failed Record hit drops its private staged owner exactly once"
        );

        let ((registration_status, registration_found, registration_owner), record_drops_after) =
            super::super::ordinary_map_domain::with_registration_failure_for_test(|| {
                (
                    get_v2(populated, long_key),
                    V2_GET_ANY_COUNTED_DROPS.with(std::cell::Cell::get),
                )
            });
        assert_eq!(registration_status, ManagedAnyStatus::Capacity as i32);
        assert_eq!(registration_found, 0);
        assert_eq!(registration_owner, 0);
        assert_eq!(record_drops_after, record_drops_before_failure + 2);
        assert_eq!(size_v2(populated), (0, 2));

        let (hit_status, hit_found, hit_owner) = get_v2(populated, short_key);
        assert_eq!((hit_status, hit_found), (0, 1));
        assert_eq!(
            any_owner::validate(AnyOwnerToken::from_wire_bits(hit_owner as u64).unwrap()),
            Ok(AnyOwnerKind::ManagedV2)
        );
        assert_eq!(contains_v2(populated, short_key), (0, 1));
        assert_eq!(contains_v2(populated, long_key), (0, 1));

        let (record_status, record_found, record_owner) = get_v2(populated, long_key);
        assert_eq!((record_status, record_found), (0, 1));

        let (miss_status, miss_found, miss_owner) = get_v2(populated, "k\0🧪".as_bytes());
        assert_eq!((miss_status, miss_found), (0, 0));
        assert_ne!(
            miss_owner, 0,
            "a successful miss publishes its i32-zero owner"
        );
        assert_eq!(unpack_v2_i32(miss_owner), 0);
        assert_eq!(size_v2(populated), (0, 2));

        drop_v2_map(populated);
        drop_v2_map(with_i32);
        drop_v2_map(empty);
        assert_eq!(
            unpack_v2_i32(hit_owner),
            37,
            "the hit owner outlives its Map"
        );
        assert_eq!(super::mimi_any_payload_v2_drop(hit_owner), 0);
        assert_eq!(
            super::mimi_any_payload_v2_drop(record_owner),
            0,
            "a successful Record hit publishes an independently owned payload"
        );
        assert_eq!(super::mimi_any_payload_v2_drop(miss_owner), 0);
    }

    fn read_i32(value: &OwnedAny) -> i32 {
        value.read_i32().expect("value is an i32 owner")
    }

    fn read_string(value: &OwnedAny) -> String {
        value.copy_string().expect("value is a string owner")
    }

    #[test]
    fn v2_map_keeps_bool_false_distinct_and_returns_fresh_i32_zero_owner_on_miss() {
        let original = new_v2_map();
        let hash = [121u8; 32];
        let source_owner = new_v2_bool_owner(0, &hash);
        let updated = set_v2_bool(original, b"flag", source_owner, &hash);
        assert_eq!(size_v2(original), (0, 0));
        assert_eq!(size_v2(updated), (0, 1));
        assert_eq!(super::mimi_any_payload_v2_drop(source_owner), 0);

        let (status, found, owner) = get_v2(updated, b"flag");
        assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_OK);
        assert_eq!(found, 1);
        assert_ne!(owner, 0);
        let (_, identity) = super::managed_v2_token_receipt(owner).unwrap();
        assert_eq!(
            identity.tag,
            super::super::managed_any_payload_v2::ManagedAnyPayloadV2Tag::Bool
        );
        assert_eq!(identity.descriptor_hash, hash);

        let mut unpacked = std::mem::MaybeUninit::<u8>::uninit();
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_unpack(
                    owner,
                    3,
                    V2_BOOL_TYPE_ID.as_ptr(),
                    V2_BOOL_TYPE_ID.len() as i64,
                    hash.as_ptr(),
                    unpacked.as_mut_ptr(),
                    1,
                    1,
                )
            },
            0
        );
        assert_eq!(unsafe { unpacked.assume_init() }, 0);
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);

        let (status, found, zero_owner) = get_v2(updated, b"missing");
        assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_OK);
        assert_eq!(found, 0);
        assert_ne!(zero_owner, 0);
        let mut zero = std::mem::MaybeUninit::<i32>::uninit();
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_unpack(
                    zero_owner,
                    1,
                    V2_I32_TYPE_ID.as_ptr(),
                    V2_I32_TYPE_ID.len() as i64,
                    V2_I32_HASH.as_ptr(),
                    zero.as_mut_ptr().cast::<u8>(),
                    std::mem::size_of::<i32>() as i64,
                    std::mem::align_of::<i32>() as i64,
                )
            },
            0
        );
        assert_eq!(unsafe { zero.assume_init() }, 0);
        assert_eq!(super::mimi_any_payload_v2_drop(zero_owner), 0);

        let legacy_v1 = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let (status, found, owner) = get_v2(legacy_v1, b"flag");
        assert_ne!(status, 0, "cross-profile errors cannot look like Get miss");
        assert_eq!((found, owner), (0, 0));

        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(legacy_v1);
        drop_v2_map(original);
        drop_v2_map(updated);
    }

    #[test]
    fn v2_get_distinguishes_present_i32_zero_from_missing_and_survives_source_drop() {
        let empty = new_v2_map();
        let source = new_v2_i32_owner(0);
        let root = set_v2_owner(empty, b"zero", source, 1, V2_I32_TYPE_ID, &V2_I32_HASH);
        assert_eq!(super::mimi_any_payload_v2_drop(source), 0);

        let (present_status, found, present) = get_v2(root, b"zero");
        let (missing_status, missing_found, missing) = get_v2(root, b"absent");
        assert_eq!((present_status, found), (0, 1));
        assert_eq!((missing_status, missing_found), (0, 0));
        assert_ne!(present, 0);
        assert_ne!(missing, 0);
        assert_ne!(present, missing);
        let (_, present_identity) = super::managed_v2_token_receipt(present).unwrap();
        let (_, missing_identity) = super::managed_v2_token_receipt(missing).unwrap();
        assert_eq!(present_identity, missing_identity);

        // A Get result is a standalone owner. Destroy every source Map before
        // reading either result; the explicit found bit preserves zero-vs-miss.
        drop_v2_map(empty);
        drop_v2_map(root);
        for owner in [present, missing] {
            let mut output = std::mem::MaybeUninit::<i32>::uninit();
            assert_eq!(
                unsafe {
                    super::mimi_any_payload_v2_unpack(
                        owner,
                        1,
                        V2_I32_TYPE_ID.as_ptr(),
                        V2_I32_TYPE_ID.len() as i64,
                        V2_I32_HASH.as_ptr(),
                        output.as_mut_ptr().cast::<u8>(),
                        std::mem::size_of::<i32>() as i64,
                        std::mem::align_of::<i32>() as i64,
                    )
                },
                0
            );
            assert_eq!(unsafe { output.assume_init() }, 0);
        }
        assert_eq!(super::mimi_any_payload_v2_drop(present), 0);
        assert_eq!(super::mimi_any_payload_v2_drop(present), 5);
        assert_eq!(super::mimi_any_payload_v2_drop(missing), 0);
    }

    #[test]
    fn v2_values_is_sorted_mixed_descriptor_snapshot_with_independent_clone_and_element_owners() {
        let empty = new_v2_map();
        let bool_owner = new_v2_bool_owner(0, &[201; 32]);
        let with_z = set_v2_owner(empty, b"z", bool_owner, 3, V2_BOOL_TYPE_ID, &[201; 32]);
        assert_eq!(super::mimi_any_payload_v2_drop(bool_owner), 0);

        let aggregate_owner = new_v2_aggregate_owner(0x5a, &[202; 32]);
        let root = set_v2_owner(
            with_z,
            b"a\0",
            aggregate_owner,
            5,
            V2_AGGREGATE_TYPE_ID,
            &[202; 32],
        );
        assert_eq!(super::mimi_any_payload_v2_drop(aggregate_owner), 0);

        let (status, values) = values_v2(root);
        assert_eq!(status, 0);
        assert_ne!(values, 0);
        let mut cloned = 0;
        assert_eq!(
            unsafe { super::mimi_ordinary_map_owned_any_v2_values_clone(values, &mut cloned) },
            0
        );
        assert_ne!(cloned, 0);
        assert_ne!(values, cloned);

        // Values survives every source Map owner, including earlier persistent
        // roots. Key ordering is raw UTF-8 bytes, so "a\0" precedes "z".
        drop_v2_map(empty);
        drop_v2_map(with_z);
        drop_v2_map(root);
        assert_eq!(values_len_v2(values), (0, 2));
        assert_eq!(values_len_v2(cloned), (0, 2));

        let (first_status, first_found, first) = values_get_v2(values, 0);
        let (second_status, second_found, second) = values_get_v2(values, 1);
        assert_eq!((first_status, first_found), (0, 1));
        assert_eq!((second_status, second_found), (0, 1));
        assert_ne!(first, 0);
        assert_ne!(second, 0);
        assert_ne!(first, second);
        let (_, first_identity) = super::managed_v2_token_receipt(first).unwrap();
        let (_, second_identity) = super::managed_v2_token_receipt(second).unwrap();
        assert_eq!(first_identity.tag as i32, 5);
        assert_eq!(first_identity.descriptor_hash, [202; 32]);
        assert_eq!(unpack_v2_byte(first, first_identity), 0x5a);
        assert_eq!(second_identity.tag as i32, 3);
        assert_eq!(second_identity.descriptor_hash, [201; 32]);
        assert_eq!(unpack_v2_byte(second, second_identity), 0);

        assert_eq!(values_get_v2(values, 2), (0, 0, 0));
        assert_eq!(values_get_v2(values, -1), (1, 0, 0));
        let mut forged_len = -1;
        assert_eq!(
            unsafe { super::mimi_ordinary_map_owned_any_v2_values_len(i64::MAX, &mut forged_len) },
            4
        );
        assert_eq!(forged_len, 0);

        assert_eq!(super::mimi_ordinary_map_owned_any_v2_values_drop(values), 0);
        assert_eq!(super::mimi_ordinary_map_owned_any_v2_values_drop(values), 5);
        assert_eq!(values_len_v2(values), (5, 0));
        // The independently cloned list still owns its descriptors after the
        // first list and all source roots have been retired.
        let (clone_status, clone_found, clone_first) = values_get_v2(cloned, 0);
        assert_eq!((clone_status, clone_found), (0, 1));
        let (_, clone_identity) = super::managed_v2_token_receipt(clone_first).unwrap();
        assert_eq!(clone_identity, first_identity);
        assert_eq!(unpack_v2_byte(clone_first, clone_identity), 0x5a);

        for owner in [first, second, clone_first] {
            assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
        }
        assert_eq!(super::mimi_ordinary_map_owned_any_v2_values_drop(cloned), 0);
        assert_eq!(super::mimi_ordinary_map_owned_any_v2_values_drop(cloned), 5);
    }

    #[test]
    fn v2_values_snapshot_cross_thread_get_drop_keeps_creator_metadata_consistent() {
        let empty = new_v2_map();
        let hash = [207; 32];
        let source = new_v2_bool_owner(1, &hash);
        let root = set_v2_owner(empty, b"flag", source, 3, V2_BOOL_TYPE_ID, &hash);
        assert_eq!(super::mimi_any_payload_v2_drop(source), 0);
        let (status, values) = values_v2(root);
        assert_eq!(status, 0);
        let creator = super::super::handle::current_lease_thread_id();
        assert_eq!(
            super::v2_values_handles()
                .lock()
                .unwrap()
                .get(&values)
                .map(|published| published.creator),
            Some(creator)
        );

        let worker = std::thread::spawn(move || {
            let worker_creator = super::super::handle::current_lease_thread_id();
            assert_ne!(creator, worker_creator);
            assert_eq!(values_len_v2(values), (0, 1));

            let mut cloned = 0;
            assert_eq!(
                unsafe { super::mimi_ordinary_map_owned_any_v2_values_clone(values, &mut cloned) },
                0
            );
            assert_eq!(
                super::v2_values_handles()
                    .lock()
                    .unwrap()
                    .get(&cloned)
                    .map(|published| published.creator),
                Some(worker_creator)
            );

            // Get publishes a fresh owner in the executing thread's domain.
            let (get_status, found, owner) = values_get_v2(values, 0);
            assert_eq!((get_status, found), (0, 1));
            let token = any_owner::AnyOwnerToken::from_wire_bits(owner as u64).unwrap();
            assert_eq!(
                super::any_owner::validate(token),
                Ok(any_owner::AnyOwnerKind::ManagedV2)
            );

            // A foreign trap drain retires the worker-created clone and Get
            // result. The source Values snapshot remains associated with its
            // original creator.
            super::super::ordinary_map_domain::drain_current_thread_roots();
            assert_eq!(
                super::any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken)
            );
            assert_eq!(values_len_v2(cloned), (5, 0));
            assert_eq!(values_len_v2(values), (0, 1));

            // Normal cross-thread Drop retires the opaque handle and its
            // private owner graph; no creator-domain receipt may remain.
            assert_eq!(super::mimi_ordinary_map_owned_any_v2_values_drop(values), 0);
            assert_eq!(values_len_v2(values), (5, 0));
        });
        worker.join().unwrap();

        assert!(!super::v2_values_handles()
            .lock()
            .unwrap()
            .contains_key(&values));
        assert_eq!(size_v2(root), (0, 1));
        drop_v2_map(empty);
        drop_v2_map(root);
    }

    #[test]
    fn v2_values_list_is_sorted_dedicated_and_independently_owned() {
        let empty = new_v2_map();
        let bool_hash = [211; 32];
        let bool_owner = new_v2_bool_owner(0, &bool_hash);
        let with_bool = set_v2_owner(empty, b"z", bool_owner, 3, V2_BOOL_TYPE_ID, &bool_hash);
        assert_eq!(super::mimi_any_payload_v2_drop(bool_owner), 0);

        let i32_owner = new_v2_i32_owner(-37);
        let with_i32 = set_v2_owner(with_bool, b"m", i32_owner, 1, V2_I32_TYPE_ID, &V2_I32_HASH);
        assert_eq!(super::mimi_any_payload_v2_drop(i32_owner), 0);

        let aggregate_hash = [212; 32];
        let aggregate_owner = new_v2_aggregate_owner(0x5a, &aggregate_hash);
        let root = set_v2_owner(
            with_i32,
            b"a\0",
            aggregate_owner,
            5,
            V2_AGGREGATE_TYPE_ID,
            &aggregate_hash,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(aggregate_owner), 0);

        let (selection_bytes, selection_hash) = selection_records(&[
            (3, V2_BOOL_TYPE_ID, &bool_hash),
            (1, V2_I32_TYPE_ID, &V2_I32_HASH),
            (5, V2_AGGREGATE_TYPE_ID, &aggregate_hash),
        ]);
        let (status, list) = values_list_v2(root, &selection_bytes, &selection_hash);
        assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_OK);
        assert!(!list.is_null());
        assert!(super::managed_any_v2_list_registered_for_test(list));
        assert!(!super::managed_any_v2_list_registered_in_legacy_domain_for_test(list));
        assert!(
            !super::register_managed_any_v2_list(list),
            "duplicate list pointer registration must be rejected"
        );
        assert_eq!(
            unsafe { (*list).element_kind },
            super::super::ListElementKind::ManagedAnyV2
        );
        assert_eq!(
            unsafe { super::super::mimi_mir_list_len_managed_any_v2(list) },
            3
        );

        let list_tokens: Vec<i64> = (0..3)
            .map(|index| unsafe { *((*list).data as *const i64).add(index) })
            .collect();
        let identities: Vec<_> = list_tokens
            .iter()
            .map(|token| super::managed_v2_token_receipt(*token).unwrap().1)
            .collect();
        // The Map Values ordering is key-byte order: "a\0", "m", then "z".
        assert_eq!(identities[0].tag as i32, 5);
        assert_eq!(identities[0].type_id.as_bytes(), V2_AGGREGATE_TYPE_ID);
        assert_eq!(identities[0].descriptor_hash, aggregate_hash);
        assert_eq!((identities[0].size, identities[0].align), (1, 1));
        assert_eq!(identities[1].tag as i32, 1);
        assert_eq!(identities[1].type_id.as_bytes(), V2_I32_TYPE_ID);
        assert_eq!(identities[1].descriptor_hash, V2_I32_HASH);
        assert_eq!(
            (identities[1].size, identities[1].align),
            (std::mem::size_of::<i32>(), std::mem::align_of::<i32>())
        );
        assert_eq!(identities[2].tag as i32, 3);
        assert_eq!(identities[2].type_id.as_bytes(), V2_BOOL_TYPE_ID);
        assert_eq!(identities[2].descriptor_hash, bool_hash);
        assert_eq!((identities[2].size, identities[2].align), (1, 1));

        let mut found = -1;
        let mut indexed_owner = -1;
        assert_eq!(
            unsafe {
                super::super::mimi_mir_list_get_managed_any_v2(
                    list,
                    -1,
                    &mut found,
                    &mut indexed_owner,
                )
            },
            0
        );
        assert_eq!(found, 1);
        assert_ne!(indexed_owner, list_tokens[2]);
        let independently_indexed_owner = indexed_owner;
        assert_eq!(
            super::managed_v2_token_receipt(independently_indexed_owner)
                .unwrap()
                .1,
            identities[2]
        );
        found = -1;
        indexed_owner = -1;
        assert_eq!(
            unsafe {
                super::super::mimi_mir_list_get_managed_any_v2(
                    list,
                    i64::MIN,
                    &mut found,
                    &mut indexed_owner,
                )
            },
            0
        );
        assert_eq!((found, indexed_owner), (0, 0));

        let clone = unsafe { super::super::mimi_mir_list_clone_managed_any_v2(list) };
        assert!(!clone.is_null());
        assert_eq!(
            unsafe { super::super::mimi_mir_list_len_managed_any_v2(clone) },
            3
        );
        let cloned_tokens: Vec<i64> = (0..3)
            .map(|index| unsafe { *((*clone).data as *const i64).add(index) })
            .collect();
        for (source, cloned) in list_tokens.iter().zip(&cloned_tokens) {
            assert_ne!(source, cloned);
            assert_eq!(
                super::managed_v2_token_receipt(*cloned).unwrap().1,
                super::managed_v2_token_receipt(*source).unwrap().1
            );
        }

        // Every list element remains usable after all source Map roots die.
        for root in [empty, with_bool, with_i32, root] {
            drop_v2_map(root);
        }
        for (owner, identity) in list_tokens.iter().zip(&identities) {
            match identity.tag as i32 {
                5 => assert_eq!(unpack_v2_byte(*owner, *identity), 0x5a),
                3 => assert_eq!(unpack_v2_byte(*owner, *identity), 0),
                1 => {}
                _ => unreachable!(),
            }
        }
        let mut unpacked_i32 = std::mem::MaybeUninit::<i32>::uninit();
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_unpack(
                    list_tokens[1],
                    1,
                    V2_I32_TYPE_ID.as_ptr(),
                    V2_I32_TYPE_ID.len() as i64,
                    V2_I32_HASH.as_ptr(),
                    unpacked_i32.as_mut_ptr().cast::<u8>(),
                    std::mem::size_of::<i32>() as i64,
                    std::mem::align_of::<i32>() as i64,
                )
            },
            0
        );
        assert_eq!(unsafe { unpacked_i32.assume_init() }, -37);

        unsafe { super::super::mimi_mir_list_drop_managed_any_v2(list) };
        assert!(!super::managed_any_v2_list_registered_for_test(list));
        for owner in &list_tokens {
            assert!(super::managed_v2_token_receipt(*owner).is_err());
        }
        assert_eq!(unpack_v2_byte(cloned_tokens[0], identities[0]), 0x5a);
        unsafe { super::super::mimi_mir_list_drop_managed_any_v2(clone) };
        for owner in cloned_tokens {
            assert!(super::managed_v2_token_receipt(owner).is_err());
        }
        assert_eq!(
            super::mimi_any_payload_v2_drop(independently_indexed_owner),
            0
        );

        // Empty Map Values accepts the canonical empty selector and yields a
        // real, zero-length V2 owner list.
        let empty_map = new_v2_map();
        let (empty_selection, empty_hash) = selection_records(&[]);
        let (empty_status, empty_list) = values_list_v2(empty_map, &empty_selection, &empty_hash);
        assert_eq!(empty_status, 0);
        assert!(!empty_list.is_null());
        assert!(super::managed_any_v2_list_registered_for_test(empty_list));
        assert_eq!(
            unsafe { super::super::mimi_mir_list_len_managed_any_v2(empty_list) },
            0
        );
        unsafe { super::super::mimi_mir_list_drop_managed_any_v2(empty_list) };
        assert!(!super::managed_any_v2_list_registered_for_test(empty_list));
        drop_v2_map(empty_map);
    }

    #[test]
    fn v2_values_list_registration_failure_and_trap_drain_reclaim_the_list() {
        let empty = new_v2_map();
        let hash = [216; 32];
        let source = new_v2_bool_owner(1, &hash);
        let root = set_v2_owner(empty, b"flag", source, 3, V2_BOOL_TYPE_ID, &hash);
        assert_eq!(super::mimi_any_payload_v2_drop(source), 0);
        let (selection_bytes, selection_hash) = selection_records(&[(3, V2_BOOL_TYPE_ID, &hash)]);

        // A failed registration returns null and frees the freshly allocated
        // empty header. Values-list construction propagates that failure and
        // releases every cloned owner before exposing an output pointer.
        let unregistered = super::with_managed_any_v2_list_registration_failure_for_test(|| {
            super::super::mimi_mir_list_new_managed_any_v2()
        });
        assert!(unregistered.is_null());
        super::begin_failure_injection(100);
        let (status, failed_list) =
            super::with_managed_any_v2_list_registration_failure_for_test(|| {
                values_list_v2(root, &selection_bytes, &selection_hash)
            });
        let staged = super::end_failure_injection();
        assert_eq!(status, 3, "registration capacity failure must fail closed");
        assert!(failed_list.is_null());
        assert_eq!(staged.len(), 1);
        for token in staged {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken),
                "registration failure leaked a staged Values token"
            );
        }
        assert_eq!(size_v2(root), (0, 1));

        let (status, list) = values_list_v2(root, &selection_bytes, &selection_hash);
        assert_eq!(status, 0);
        assert!(super::managed_any_v2_list_registered_for_test(list));
        let list_owner = unsafe { *((*list).data as *const i64) };

        // Trap cleanup uses the normal V2 list destructor. This must retire
        // the list receipt before its element Drop glue runs, then the shared
        // owner sweep must find no duplicate token left to release.
        super::super::ordinary_map_domain::drain_current_thread_roots();
        assert!(!super::managed_any_v2_list_registered_for_test(list));
        assert!(super::managed_v2_token_receipt(list_owner).is_err());
        assert_ne!(size_v2(root).0, 0, "the root was retired by trap cleanup");
    }

    #[test]
    fn v2_values_list_rejects_unselected_descriptors_and_rolls_back_failures() {
        let empty = new_v2_map();
        let hash = [213; 32];
        let owner = new_v2_bool_owner(1, &hash);
        let root = set_v2_owner(empty, b"flag", owner, 3, V2_BOOL_TYPE_ID, &hash);
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);

        let (wrong_selection, wrong_selection_hash) =
            selection(5, V2_AGGREGATE_TYPE_ID, &[214; 32]);
        super::begin_failure_injection(100);
        let (wrong_status, wrong_list) =
            values_list_v2(root, &wrong_selection, &wrong_selection_hash);
        let staged = super::end_failure_injection();
        assert_eq!(wrong_status, 7, "unselected descriptor must fail closed");
        assert!(wrong_list.is_null());
        assert!(!staged.is_empty());
        for token in staged {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken),
                "selector rejection leaked a staged Values owner"
            );
        }

        let (selected, selected_hash) = selection(3, V2_BOOL_TYPE_ID, &hash);
        let wrong_type_desc_hash = [215; 32];
        let (wrong_type_desc_selection, wrong_type_desc_registry_hash) =
            selection_records(&[(3, V2_BOOL_TYPE_ID, &wrong_type_desc_hash)]);
        let (type_desc_status, type_desc_list) = values_list_v2(
            root,
            &wrong_type_desc_selection,
            &wrong_type_desc_registry_hash,
        );
        assert_eq!(
            type_desc_status, 7,
            "same tag/type with a different Core TypeDesc closure digest must fail closed"
        );
        assert!(type_desc_list.is_null());

        // The valid canonical selector hash is separate from each selected
        // TypeDesc digest. Corrupting it must be rejected before publication.
        let mut incorrect_hash = selected_hash;
        incorrect_hash[0] ^= 0x80;
        let (hash_status, hash_list) = values_list_v2(root, &selected, &incorrect_hash);
        assert_eq!(
            hash_status, 7,
            "wrong selector hash is a descriptor mismatch"
        );
        assert!(hash_list.is_null());
        let malformed = [0u8; 8];
        let (encoding_status, encoding_list) = values_list_v2(root, &malformed, &selected_hash);
        assert_eq!(encoding_status, 1, "malformed selector is invalid input");
        assert!(encoding_list.is_null());

        assert_eq!(size_v2(root), (0, 1));
        let (get_status, found, surviving) = get_v2(root, b"flag");
        assert_eq!((get_status, found), (0, 1));
        assert_eq!(
            unpack_v2_byte(
                surviving,
                super::managed_v2_token_receipt(surviving).unwrap().1
            ),
            1
        );
        assert_eq!(super::mimi_any_payload_v2_drop(surviving), 0);

        let second = new_v2_bool_owner(0, &hash);
        let with_second = set_v2_owner(root, b"other", second, 3, V2_BOOL_TYPE_ID, &hash);
        assert_eq!(super::mimi_any_payload_v2_drop(second), 0);
        let (selected, selected_hash) = selection(3, V2_BOOL_TYPE_ID, &hash);
        // The helper has five observable allocation checkpoints for two
        // entries: sorted index, result Vec, two owner clones, and list
        // allocation. Each injected failure must leave only the Map's source
        // owners live; the final admitted stage must publish a list cleanly.
        for failure_stage in 0..=5 {
            super::begin_failure_injection(failure_stage);
            let (status, staged_list) = values_list_v2(with_second, &selected, &selected_hash);
            let staged = super::end_failure_injection();
            if failure_stage < 5 {
                assert_eq!(status, 3, "allocation stage {failure_stage}");
                assert!(staged_list.is_null(), "stage {failure_stage}");
            } else {
                assert_eq!(status, 0);
                assert!(!staged_list.is_null());
                assert_eq!(
                    unsafe { super::super::mimi_mir_list_len_managed_any_v2(staged_list) },
                    2
                );
                unsafe { super::super::mimi_mir_list_drop_managed_any_v2(staged_list) };
            }
            for token in staged {
                assert_eq!(
                    any_owner::validate(token),
                    Err(any_owner::AnyOwnerError::StaleToken),
                    "allocation stage {failure_stage} leaked or retained a Values owner"
                );
            }
            assert_eq!(size_v2(with_second), (0, 2));
        }

        let (status, source_list) = values_list_v2(with_second, &selected, &selected_hash);
        assert_eq!(status, 0);
        assert!(!source_list.is_null());

        // Fail the second append: first published token is owned by the list
        // guard, the second remains with the helper and is explicitly dropped.
        super::begin_failure_injection(100);
        super::super::set_managed_any_v2_list_push_fail_after_for_test(Some(1));
        let (push_status, failed_list) = values_list_v2(with_second, &selected, &selected_hash);
        super::super::set_managed_any_v2_list_push_fail_after_for_test(None);
        let staged_push = super::end_failure_injection();
        assert_eq!(push_status, 3, "append failure reports Capacity");
        assert!(failed_list.is_null());
        assert_eq!(staged_push.len(), 2);
        for token in staged_push {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken),
                "failed append leaked a staged or published prefix owner"
            );
        }
        assert_eq!(
            unsafe { super::super::mimi_mir_list_len_managed_any_v2(source_list) },
            2
        );

        // A list clone's partial prefix and current failed append also roll
        // back using only the V2 owner clone/drop ABI.
        super::begin_failure_injection(100);
        super::super::set_managed_any_v2_list_push_fail_after_for_test(Some(1));
        let failed_clone = unsafe { super::super::mimi_mir_list_clone_managed_any_v2(source_list) };
        super::super::set_managed_any_v2_list_push_fail_after_for_test(None);
        let staged_clone = super::end_failure_injection();
        assert!(failed_clone.is_null());
        assert_eq!(staged_clone.len(), 2);
        for token in staged_clone {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken),
                "failed List clone leaked a V2 owner"
            );
        }

        unsafe { super::super::mimi_mir_list_drop_managed_any_v2(source_list) };
        drop_v2_map(empty);
        drop_v2_map(root);
        drop_v2_map(with_second);
    }

    #[test]
    fn v2_values_failures_are_atomic_and_staged_payloads_are_dropped_once() {
        use std::sync::atomic::Ordering;

        V2_VALUES_CLONE_FAIL.store(false, Ordering::SeqCst);
        V2_VALUES_DROP_CALLS.store(0, Ordering::SeqCst);
        let value = 0x71u8;
        let mut source = 0;
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_new(
                    3,
                    V2_TEST_FAILURE_TYPE_ID.as_ptr(),
                    V2_TEST_FAILURE_TYPE_ID.len() as i64,
                    V2_TEST_FAILURE_HASH.as_ptr(),
                    &value,
                    1,
                    1,
                    Some(clone_v2_toggle_failure),
                    Some(drop_v2_values_counted),
                    &mut source,
                )
            },
            0
        );
        let empty = new_v2_map();
        let root = set_v2_owner(
            empty,
            b"x",
            source,
            3,
            V2_TEST_FAILURE_TYPE_ID,
            &V2_TEST_FAILURE_HASH,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(source), 0);

        // A descriptor callback failure while cloning a values element leaves
        // the output empty and the source Map intact.
        V2_VALUES_CLONE_FAIL.store(true, Ordering::SeqCst);
        let mut failed = -1;
        assert_eq!(
            unsafe { super::mimi_ordinary_map_owned_any_v2_values(root, &mut failed) },
            14
        );
        assert_eq!(failed, 0);
        V2_VALUES_CLONE_FAIL.store(false, Ordering::SeqCst);
        assert_eq!(size_v2(root), (0, 1));

        // Exercise every fallible allocation checkpoint, including failure
        // after an element clone but before the opaque list handle is published.
        for failure_stage in 0..4 {
            super::begin_failure_injection(failure_stage);
            let mut output = -1;
            let status = unsafe { super::mimi_ordinary_map_owned_any_v2_values(root, &mut output) };
            let staged = super::end_failure_injection();
            assert_eq!(status, 3, "stage {failure_stage}");
            assert_eq!(output, 0, "stage {failure_stage}");
            for token in staged {
                assert_eq!(
                    any_owner::validate(token),
                    Err(any_owner::AnyOwnerError::StaleToken),
                    "stage {failure_stage} leaked an element clone"
                );
            }
            assert_eq!(size_v2(root), (0, 1));
        }
        assert_eq!(V2_VALUES_DROP_CALLS.load(Ordering::SeqCst), 2);

        let (status, values) = values_v2(root);
        assert_eq!(status, 0);
        drop_v2_map(empty);
        drop_v2_map(root);
        let (get_status, found, element) = values_get_v2(values, 0);
        assert_eq!((get_status, found), (0, 1));
        assert_eq!(super::mimi_ordinary_map_owned_any_v2_values_drop(values), 0);
        assert_eq!(super::mimi_any_payload_v2_drop(element), 0);
        assert_eq!(V2_VALUES_DROP_CALLS.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn v2_is_empty_uses_status_out_and_preserves_independent_persistent_roots() {
        let original = new_v2_map();
        assert_eq!(is_empty_v2(original), (0, 1));

        let hash = [131u8; 32];
        let owner = new_v2_bool_owner(0, &hash);
        let updated = set_v2_bool(original, b"flag", owner, &hash);
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
        assert_eq!(is_empty_v2(original), (0, 1));
        assert_eq!(is_empty_v2(updated), (0, 0));

        let mut failed_output = -1;
        let legacy = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        let status =
            unsafe { super::mimi_ordinary_map_owned_any_v2_is_empty(legacy, &mut failed_output) };
        assert_ne!(status, 0);
        assert_eq!(failed_output, 0, "error must not be coerced to a boolean");

        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(legacy),
            ()
        );
        drop_v2_map(original);
        assert_eq!(is_empty_v2(updated), (0, 0));
        drop_v2_map(updated);
    }

    #[test]
    fn v2_contains_borrows_utf8_key_without_consuming_allocation_checkpoint() {
        let empty = new_v2_map();
        let owner = new_v2_i32_owner(42);
        let root = set_v2_owner(
            empty,
            "键\0🧪".as_bytes(),
            owner,
            1,
            V2_I32_TYPE_ID,
            &V2_I32_HASH,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);

        super::begin_failure_injection(0);
        assert_eq!(contains_v2(root, "键\0🧪".as_bytes()), (0, 1));

        // Contains must leave the pending failure for the next operation that
        // actually reaches an allocation checkpoint.
        let mut cloned = -1;
        let clone_status =
            unsafe { super::mimi_ordinary_map_owned_any_v2_clone(root, &mut cloned) };
        let staged = super::end_failure_injection();
        assert_eq!(clone_status, 3);
        assert_eq!(cloned, 0);
        assert!(staged.is_empty());

        assert_eq!(contains_v2(root, "键\0🧪".as_bytes()), (0, 1));
        assert_eq!(contains_v2(root, b"missing"), (0, 0));
        let invalid_utf8 = contains_v2(root, &[0xff]);
        assert_ne!(invalid_utf8.0, 0);
        assert_eq!(invalid_utf8.1, 0);

        drop_v2_map(empty);
        drop_v2_map(root);
    }

    #[test]
    fn v2_values_list_publication_capacity_keeps_output_empty_and_source_live() {
        let empty = new_v2_map();
        let bool_hash = [231; 32];
        let bool_owner = new_v2_bool_owner(1, &bool_hash);
        let with_bool = set_v2_owner(empty, b"z", bool_owner, 3, V2_BOOL_TYPE_ID, &bool_hash);
        assert_eq!(super::mimi_any_payload_v2_drop(bool_owner), 0);

        let i32_owner = new_v2_i32_owner(23);
        let root = set_v2_owner(with_bool, b"a", i32_owner, 1, V2_I32_TYPE_ID, &V2_I32_HASH);
        assert_eq!(super::mimi_any_payload_v2_drop(i32_owner), 0);
        let (selection_bytes, selection_hash) = selection_records(&[
            (1, V2_I32_TYPE_ID, &V2_I32_HASH),
            (3, V2_BOOL_TYPE_ID, &bool_hash),
        ]);

        // Fail only after the sorted list and both independently owned
        // payloads have been staged. The ABI must leave its output null and
        // the RAII list guard must retire every unpublished token.
        super::begin_failure_injection(100);
        let (status, list, attempts) =
            super::with_ordinary_map_owned_any_v2_values_publish_failure_for_test(0, || {
                let (status, list) = values_list_v2(root, &selection_bytes, &selection_hash);
                let attempts = super::ordinary_map_owned_any_v2_values_publish_attempts_for_test();
                (status, list, attempts)
            });
        let staged = super::end_failure_injection();
        assert_eq!(status, 3, "Capacity is the stable ABI status code");
        assert!(list.is_null(), "failed Values publication exposes no List");
        assert_eq!(attempts, 1);
        assert!(staged.len() >= 2, "both payload owners were staged");
        for token in staged {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken),
                "failed Values publication leaked a staged element owner"
            );
        }
        assert_eq!(size_v2(root), (0, 2), "the source remains usable");
        drop_v2_map(empty);
        drop_v2_map(with_bool);
        drop_v2_map(root);
    }

    #[test]
    fn aggregate_tag5_owner_is_pinned_through_set_get_clone_and_drop() {
        let original = new_v2_map();
        let hash = [132u8; 32];
        let source_owner = new_v2_aggregate_owner(0x5a, &hash);
        let (selection_bytes, selection_hash) = selection(5, V2_AGGREGATE_TYPE_ID, &hash);
        let mut updated = 0;
        assert_eq!(
            unsafe {
                super::mimi_ordinary_map_owned_any_v2_set(
                    original,
                    b"record".as_ptr(),
                    6,
                    source_owner,
                    selection_bytes.as_ptr(),
                    selection_bytes.len() as i64,
                    selection_hash.as_ptr(),
                    &mut updated,
                )
            },
            0,
            "tag5 is admitted by exact pinned recursive glue, not a scalar-only gate"
        );
        assert_ne!(updated, 0);
        assert_eq!(super::mimi_any_payload_v2_drop(source_owner), 0);
        assert_eq!(size_v2(original), (0, 0));
        assert_eq!(size_v2(updated), (0, 1));

        let mut cloned = 0;
        assert_eq!(
            unsafe { super::mimi_ordinary_map_owned_any_v2_clone(updated, &mut cloned) },
            0
        );
        assert_ne!(cloned, 0);
        drop_v2_map(updated);

        let (status, found, owner) = get_v2(cloned, b"record");
        assert_eq!((status, found), (0, 1));
        let mut payload = std::mem::MaybeUninit::<u8>::uninit();
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_unpack(
                    owner,
                    5,
                    V2_AGGREGATE_TYPE_ID.as_ptr(),
                    V2_AGGREGATE_TYPE_ID.len() as i64,
                    hash.as_ptr(),
                    payload.as_mut_ptr(),
                    1,
                    1,
                )
            },
            0
        );
        assert_eq!(unsafe { payload.assume_init() }, 0x5a);
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
        drop_v2_map(original);
        drop_v2_map(cloned);
    }

    #[test]
    fn v2_map_creator_trap_cleanup_retires_roots_and_published_any_snapshot() {
        let root = new_v2_map();
        let hash = [122u8; 32];
        let source_owner = new_v2_bool_owner(1, &hash);
        let root = set_v2_bool(root, b"b", source_owner, &hash);
        assert_eq!(super::mimi_any_payload_v2_drop(source_owner), 0);
        let (status, found, owner) = get_v2(root, b"b");
        assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_OK);
        assert_eq!(found, 1);
        let token = any_owner::AnyOwnerToken::from_wire_bits(owner as u64).unwrap();
        super::super::ordinary_map_domain::drain_current_thread_roots();
        assert_eq!(
            any_owner::validate(token),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        assert_ne!(size_v2(root).0, 0);
        assert_eq!(size_v2(root).1, 0);
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

#[cfg(test)]
mod v2_runtime_tests {
    use super::*;

    fn set_label(root: i64, key: &str, text: &str) -> i64 {
        unsafe {
            mimi_mir_ordinary_map_any_owner_v2_record_set_label(
                root,
                key.as_ptr(),
                key.len() as i64,
                text.as_ptr(),
                text.len() as i64,
            )
        }
    }

    fn set_i32(root: i64, key: &str, value: i32) -> i64 {
        unsafe {
            mimi_mir_ordinary_map_any_owner_v2_record_set_i32(
                root,
                key.as_ptr(),
                key.len() as i64,
                value,
            )
        }
    }

    fn remove(root: i64, key: &str) -> i64 {
        unsafe {
            mimi_mir_ordinary_map_any_owner_v2_record_remove(root, key.as_ptr(), key.len() as i64)
        }
    }

    fn contains(root: i64, key: &str) -> i32 {
        unsafe {
            mimi_mir_ordinary_map_any_owner_v2_record_contains(root, key.as_ptr(), key.len() as i64)
        }
    }

    fn inspect(root: i64, key: &str) -> (usize, bool, Option<String>) {
        let lease = v2_acquire_root(root).expect("V2 root is live");
        let snapshot = v2_snapshot_from_map(&lease).expect("V2 snapshot is installed");
        (
            snapshot.size(),
            snapshot.contains(key),
            snapshot.label_text(key).map(str::to_owned),
        )
    }

    #[test]
    fn v2_record_roots_deep_copy_persist_overwrite_remove_and_drop() {
        let mut key = b"label".to_vec();
        let mut text = "owned label".as_bytes().to_vec();
        let root = mimi_mir_ordinary_map_any_owner_v2_record_new();
        assert_ne!(root, 0);
        let first = unsafe {
            mimi_mir_ordinary_map_any_owner_v2_record_set_label(
                root,
                key.as_ptr(),
                key.len() as i64,
                text.as_ptr(),
                text.len() as i64,
            )
        };
        assert_ne!(first, 0);
        // The root owns private copies, so caller buffers can be reused as
        // soon as Set returns.
        key.fill(b'x');
        text.fill(b'z');
        assert_eq!(
            inspect(first, "label"),
            (1, true, Some("owned label".into()))
        );

        let mixed = set_i32(first, "answer", 42);
        assert_ne!(mixed, 0);
        let overwritten = set_label(mixed, "label", "new value");
        assert_ne!(overwritten, 0);
        let removed = remove(overwritten, "answer");
        assert_ne!(removed, 0);

        assert_eq!(inspect(root, "label"), (0, false, None));
        assert_eq!(
            inspect(first, "label"),
            (1, true, Some("owned label".into()))
        );
        assert_eq!(inspect(mixed, "answer"), (2, true, None));
        assert_eq!(
            inspect(overwritten, "label"),
            (2, true, Some("new value".into()))
        );
        assert_eq!(inspect(removed, "answer"), (1, false, None));
        assert_eq!(contains(removed, "label"), 1);

        for handle in [root, first, mixed, overwritten, removed] {
            assert_eq!(
                mimi_mir_ordinary_map_any_owner_v2_record_drop(handle),
                MIMI_MAP_ANY_OWNER_V2_OK
            );
        }
        assert_eq!(mimi_mir_ordinary_map_any_owner_v2_record_size(removed), -1);
    }

    #[test]
    fn v2_stale_generation_and_registration_failure_are_fail_closed() {
        let first = mimi_mir_ordinary_map_any_owner_v2_record_new();
        assert_ne!(first, 0);
        assert_eq!(
            mimi_mir_ordinary_map_any_owner_v2_record_drop(first),
            MIMI_MAP_ANY_OWNER_V2_OK
        );

        let second = mimi_mir_ordinary_map_any_owner_v2_record_new();
        assert_ne!(second, 0);
        assert_ne!(first, second, "slot reuse must advance its generation");
        assert_eq!(mimi_mir_ordinary_map_any_owner_v2_record_size(first), -1);
        assert_eq!(
            mimi_mir_ordinary_map_any_owner_v2_record_drop(first),
            MIMI_MAP_ANY_OWNER_V2_STALE
        );
        assert_eq!(
            super::super::ordinary_map_domain::with_registration_failure_for_test(|| {
                mimi_mir_ordinary_map_any_owner_v2_record_new()
            },),
            0
        );
        assert_eq!(mimi_mir_ordinary_map_any_owner_v2_record_size(second), 0);
        assert_eq!(
            mimi_mir_ordinary_map_any_owner_v2_record_drop(second),
            MIMI_MAP_ANY_OWNER_V2_OK
        );
    }

    #[test]
    fn v2_invalid_set_is_atomic_and_profile_handles_do_not_cross() {
        let root = mimi_mir_ordinary_map_any_owner_v2_record_new();
        assert_ne!(root, 0);
        let invalid_utf8 = [0xff_u8];
        assert_eq!(
            unsafe {
                mimi_mir_ordinary_map_any_owner_v2_record_set_label(
                    root,
                    b"bad".as_ptr(),
                    3,
                    invalid_utf8.as_ptr(),
                    invalid_utf8.len() as i64,
                )
            },
            0
        );
        assert_eq!(mimi_mir_ordinary_map_any_owner_v2_record_size(root), 0);
        assert_eq!(contains(root, "bad"), 0);

        let v1 = mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new();
        assert_ne!(v1, 0);
        assert_eq!(set_i32(v1, "wrong", 7), 0);
        assert_eq!(mimi_mir_ordinary_map_any_owner_v2_record_size(v1), -1);
        assert_eq!(
            mimi_mir_ordinary_map_any_owner_v2_record_drop(v1),
            MIMI_MAP_ANY_OWNER_V2_INVALID_HANDLE
        );

        assert_eq!(
            mimi_mir_ordinary_map_any_owner_v2_record_drop(root),
            MIMI_MAP_ANY_OWNER_V2_OK
        );
        super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop(v1);
    }

    #[test]
    fn owned_any_v2_root_publication_capacity_returns_e0800_status_without_output() {
        let mut root = i64::MAX;
        super::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
            let status = unsafe { mimi_ordinary_map_owned_any_v2_new(&mut root) };
            assert_eq!(status, ManagedAnyStatus::Capacity as i32);
            assert_eq!(root, 0, "failed publication must not expose a handle");
            assert_eq!(
                super::ordinary_map_owned_any_v2_root_publish_attempts_for_test(),
                1
            );
        });
    }
}
