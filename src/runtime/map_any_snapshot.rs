//! Persistent ordinary-map snapshots whose entries own typed `Any` values.
//!
//! This module contains the existing distinct v1 profiles and the staged
//! `ordinary_map_owned_any_v2` profile. The v2 snapshot is isolated by its
//! MapStorageMode and stores AnyOwnerTokens only in private non-copyable RAII
//! values; none of these tokens are legacy ValueHandles.
//!
//! The V2 ordinary-Map ABI uses status/out for every fallible operation.
//! Status zero means success. Existing consumers route nonzero statuses to
//! the compiler-native trap adapter; the dedicated Result-valued GetAny
//! instruction translates only raw capacity status 3 into source `Err(4)`.
//! V1 exports retain their historical ABI. Rust staging locals are dropped
//! before an error status is returned.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

#[cfg(standalone)]
use super::libc as system_libc;
#[cfg(not(standalone))]
use libc as system_libc;

#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
use std::cell::Cell;
#[cfg(test)]
use std::cell::RefCell;

use super::any_owner::{self, AnyOwnerError, AnyOwnerKind, AnyOwnerReceiptError, AnyOwnerToken};
use super::managed_any_payload_v2::{
    ManagedAnyPayloadV2, ManagedAnyPayloadV2Glue, ManagedAnyPayloadV2Identity,
    ManagedAnyPayloadV2Tag, ManagedAnyPayloadV2TypeId,
};

pub(crate) const ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_CLONE_STAGE: i32 = 0;
pub(crate) const ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_PUBLISH_STAGE: i32 = 1;

/// C-compatible observation frame used only by the standalone L3 Memcheck
/// executable. It records the source lease checks and ownership domains at
/// the SetResult boundary, then samples final settlement after generated main
/// has completed its explicit drops.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OrdinaryMapSetResultL3Observation {
    pub set_calls: u64,
    pub source_before_status: i32,
    pub source_before_size: i32,
    pub source_after_status: i32,
    pub source_after_size: i32,
    pub set_status: i32,
    pub output_root_zero: i32,
    pub roots_after_set: u64,
    pub managed_any_after_set: u64,
    pub any_owners_after_set: u64,
    pub root_publish_attempts: u64,
    pub shutdown_registered: i32,
    pub final_roots: u64,
    pub final_ordinary_map_handles: u64,
    pub final_owned_get_handles: u64,
    pub final_runtime_map_handles: u64,
    pub final_managed_any: u64,
    pub final_owned_strings: u64,
    pub final_any_owner_tokens: u64,
    pub final_lists: u64,
}

/// Independent observation record for the exact String Set/Get owner route.
/// This intentionally has a separate C ABI from the historical i32 Set
/// observer so adding String-specific fields cannot change its layout.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OrdinaryMapSetGetStringL3Observation {
    pub set_calls: u64,
    pub get_calls: u64,
    pub set_source_before_status: i32,
    pub set_source_before_size: i32,
    pub set_source_after_status: i32,
    pub set_source_after_size: i32,
    pub set_status: i32,
    pub set_output_root_zero: i32,
    pub set_roots_after: u64,
    pub set_ordinary_map_handles_after: u64,
    pub set_owned_get_handles_after: u64,
    pub set_managed_any_after: u64,
    pub set_any_owner_tokens_after: u64,
    pub set_owned_strings_after: u64,
    pub set_runtime_map_handles_after: u64,
    pub set_lists_after: u64,
    pub set_root_publish_attempts: u64,
    pub get_status: i32,
    pub get_outputs_zero: i32,
    pub get_output_length: i64,
    pub get_roots_after: u64,
    pub get_ordinary_map_handles_after: u64,
    pub get_owned_get_handles_after: u64,
    pub get_managed_any_after: u64,
    pub get_any_owner_tokens_after: u64,
    pub get_owned_strings_after: u64,
    pub get_runtime_map_handles_after: u64,
    pub get_lists_after: u64,
    pub get_copy_clone_attempts: u64,
    pub get_copy_publish_attempts: u64,
    pub get_registry_registration_attempts: u64,
    pub get_registry_capacity_failures: u64,
    pub get_temporary_string_drops: u64,
    pub shutdown_registered: i32,
    pub final_roots: u64,
    pub final_ordinary_map_handles: u64,
    pub final_owned_get_handles: u64,
    pub final_runtime_map_handles: u64,
    pub final_managed_any: u64,
    pub final_owned_strings: u64,
    pub final_any_owner_tokens: u64,
    pub final_lists: u64,
}

/// Independent observation ABI for the fallible New/Set/Result-valued GetAny
/// i32 lineage. Each boundary records live ownership domains immediately
/// after the corresponding status/out runtime call. This is separate from
/// both the legacy SetResult and tuple-valued GetAny observers.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OrdinaryMapSetGetAnyResultI32L3Boundary {
    pub calls: u64,
    pub status: i32,
    pub output_zero: i32,
    pub source_before_status: i32,
    pub source_before_size: i32,
    pub source_after_status: i32,
    pub source_after_size: i32,
    pub output_found: i32,
    pub output_owner_zero: i32,
    pub roots: u64,
    pub ordinary_map_handles: u64,
    pub owned_get_handles: u64,
    pub runtime_map_handles: u64,
    pub managed_any: u64,
    pub owned_strings: u64,
    pub any_owner_tokens: u64,
    pub lists: u64,
    pub root_publish_attempts: u64,
    pub get_publish_attempts: u64,
}

#[cfg(feature = "ordinary-map-set-result-l3")]
impl OrdinaryMapSetGetAnyResultI32L3Boundary {
    const EMPTY: Self = Self {
        calls: 0,
        status: i32::MIN,
        output_zero: -1,
        source_before_status: i32::MIN,
        source_before_size: i32::MIN,
        source_after_status: i32::MIN,
        source_after_size: i32::MIN,
        output_found: i32::MIN,
        output_owner_zero: -1,
        roots: u64::MAX,
        ordinary_map_handles: u64::MAX,
        owned_get_handles: u64::MAX,
        runtime_map_handles: u64::MAX,
        managed_any: u64::MAX,
        owned_strings: u64::MAX,
        any_owner_tokens: u64::MAX,
        lists: u64::MAX,
        root_publish_attempts: u64::MAX,
        get_publish_attempts: u64::MAX,
    };
}

/// Full C-layout record for the dedicated fallible GetAny-i32 native L3
/// artifact. The final census is sampled after generated main returns; the
/// per-operation boundaries above prove that staged owners were either
/// published or reclaimed before that final check.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OrdinaryMapSetGetAnyResultI32L3Observation {
    pub new: OrdinaryMapSetGetAnyResultI32L3Boundary,
    pub set: OrdinaryMapSetGetAnyResultI32L3Boundary,
    pub get: OrdinaryMapSetGetAnyResultI32L3Boundary,
    pub shutdown_registered: i32,
    pub final_roots: u64,
    pub final_ordinary_map_handles: u64,
    pub final_owned_get_handles: u64,
    pub final_runtime_map_handles: u64,
    pub final_managed_any: u64,
    pub final_owned_strings: u64,
    pub final_any_owner_tokens: u64,
    pub final_lists: u64,
}

#[cfg(feature = "ordinary-map-set-result-l3")]
impl OrdinaryMapSetGetAnyResultI32L3Observation {
    const EMPTY: Self = Self {
        new: OrdinaryMapSetGetAnyResultI32L3Boundary::EMPTY,
        set: OrdinaryMapSetGetAnyResultI32L3Boundary::EMPTY,
        get: OrdinaryMapSetGetAnyResultI32L3Boundary::EMPTY,
        shutdown_registered: 0,
        final_roots: u64::MAX,
        final_ordinary_map_handles: u64::MAX,
        final_owned_get_handles: u64::MAX,
        final_runtime_map_handles: u64::MAX,
        final_managed_any: u64::MAX,
        final_owned_strings: u64::MAX,
        final_any_owner_tokens: u64::MAX,
        final_lists: u64::MAX,
    };
}

#[cfg(feature = "ordinary-map-set-result-l3")]
const _: [(); 120] = [(); std::mem::size_of::<OrdinaryMapSetGetAnyResultI32L3Boundary>()];
#[cfg(feature = "ordinary-map-set-result-l3")]
const _: [(); 432] = [(); std::mem::size_of::<OrdinaryMapSetGetAnyResultI32L3Observation>()];

#[cfg(feature = "ordinary-map-set-result-l3")]
impl OrdinaryMapSetGetStringL3Observation {
    const EMPTY: Self = Self {
        set_calls: 0,
        get_calls: 0,
        set_source_before_status: i32::MIN,
        set_source_before_size: i32::MIN,
        set_source_after_status: i32::MIN,
        set_source_after_size: i32::MIN,
        set_status: i32::MIN,
        set_output_root_zero: -1,
        set_roots_after: u64::MAX,
        set_ordinary_map_handles_after: u64::MAX,
        set_owned_get_handles_after: u64::MAX,
        set_managed_any_after: u64::MAX,
        set_any_owner_tokens_after: u64::MAX,
        set_owned_strings_after: u64::MAX,
        set_runtime_map_handles_after: u64::MAX,
        set_lists_after: u64::MAX,
        set_root_publish_attempts: u64::MAX,
        get_status: i32::MIN,
        get_outputs_zero: -1,
        get_output_length: i64::MIN,
        get_roots_after: u64::MAX,
        get_ordinary_map_handles_after: u64::MAX,
        get_owned_get_handles_after: u64::MAX,
        get_managed_any_after: u64::MAX,
        get_any_owner_tokens_after: u64::MAX,
        get_owned_strings_after: u64::MAX,
        get_runtime_map_handles_after: u64::MAX,
        get_lists_after: u64::MAX,
        get_copy_clone_attempts: u64::MAX,
        get_copy_publish_attempts: u64::MAX,
        get_registry_registration_attempts: u64::MAX,
        get_registry_capacity_failures: u64::MAX,
        get_temporary_string_drops: u64::MAX,
        shutdown_registered: 0,
        final_roots: u64::MAX,
        final_ordinary_map_handles: u64::MAX,
        final_owned_get_handles: u64::MAX,
        final_runtime_map_handles: u64::MAX,
        final_managed_any: u64::MAX,
        final_owned_strings: u64::MAX,
        final_any_owner_tokens: u64::MAX,
        final_lists: u64::MAX,
    };
}

#[cfg(feature = "ordinary-map-set-result-l3")]
impl OrdinaryMapSetResultL3Observation {
    const EMPTY: Self = Self {
        set_calls: 0,
        source_before_status: i32::MIN,
        source_before_size: i32::MIN,
        source_after_status: i32::MIN,
        source_after_size: i32::MIN,
        set_status: i32::MIN,
        output_root_zero: -1,
        roots_after_set: u64::MAX,
        managed_any_after_set: u64::MAX,
        any_owners_after_set: u64::MAX,
        root_publish_attempts: u64::MAX,
        shutdown_registered: 0,
        final_roots: u64::MAX,
        final_ordinary_map_handles: u64::MAX,
        final_owned_get_handles: u64::MAX,
        final_runtime_map_handles: u64::MAX,
        final_managed_any: u64::MAX,
        final_owned_strings: u64::MAX,
        final_any_owner_tokens: u64::MAX,
        final_lists: u64::MAX,
    };
}

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

    /// Read a V2 entry as an i32 only when its complete pinned descriptor is
    /// the canonical Core scalar identity. This borrows the entry owner and
    /// writes into stack storage; it does not clone or publish another token.
    fn read_canonical_i32(&self) -> Result<Option<i32>, AnyOwnerError> {
        let identity = self.identity()?;
        if !super::managed_any_payload_v2::is_canonical_i32_identity(identity) {
            return Ok(None);
        }
        let mut output = std::mem::MaybeUninit::<i32>::uninit();
        // SAFETY: identity was checked against the canonical i32 TypeDesc and
        // layout, and `output` is correctly aligned writable storage. The
        // pinned runtime descriptor supplies the clone glue.
        unsafe {
            any_owner::unpack_managed_v2_owner(
                self.token,
                identity,
                output.as_mut_ptr().cast::<u8>(),
                std::mem::size_of::<i32>(),
                std::mem::align_of::<i32>(),
            )?
        };
        // SAFETY: successful unpack initialized the complete i32 output.
        Ok(Some(unsafe { output.assume_init() }))
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

    /// Borrow a stored V2 payload and read it only as the exact canonical
    /// Core i32. A missing key and a different descriptor remain distinct
    /// ordinary Result outcomes; the entry owner and root stay untouched.
    fn get_i32_typed(&self, key: &str) -> Result<OrdinaryMapLookup<i32>, OrdinaryMapSnapshotError> {
        match self.entries.get(key) {
            None => Ok(OrdinaryMapLookup::Missing),
            Some(value) => match value.read_canonical_i32()? {
                Some(value) => Ok(OrdinaryMapLookup::Value(value)),
                None => Ok(OrdinaryMapLookup::TypeMismatch),
            },
        }
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

    /// Copy the Map keys in deterministic UTF-8 byte order. The values are
    /// never cloned or inspected; a failure drops the staged String prefix.
    fn keys(&self) -> Result<Vec<String>, OrdinaryMapSnapshotError> {
        let ordered = self.sorted_entries()?;
        let mut keys = Vec::new();
        snapshot_allocation_checkpoint()?;
        keys.try_reserve_exact(ordered.len())
            .map_err(|_| OrdinaryMapSnapshotError::Capacity)?;
        for (key, _) in ordered {
            keys.push(try_copy_string(key)?);
        }
        Ok(keys)
    }

    /// Reject values outside the selected descriptor set before any payload
    /// Clone glue runs. The identity lookup reads only the owner-table receipt
    /// and keeps a rejected Values request from invoking callbacks for a type
    /// the caller did not select.
    fn validate_values_selection(
        &self,
        selection: &super::managed_any_payload_v2::ManagedAnyPayloadV2Selection<'_>,
    ) -> Result<(), ManagedAnyStatus> {
        for value in self.entries.values() {
            let identity = value.identity().map_err(ManagedAnyStatus::from)?;
            if !selection.contains(identity) {
                return Err(ManagedAnyStatus::DescriptorMismatch);
            }
        }
        Ok(())
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
    receipt_id: u64,
    creator: super::handle::LeaseThreadId,
    domain: ManagedAnyV2ListDomain,
    // A Retiring receipt keeps its count after the pointer is detached, so a
    // newly allocated MimiList may safely reuse the address before the old
    // receipt is finalized by id.
    list: Option<usize>,
    // Keep the original address to reject a second external free only while
    // the header is still attached. After detachment, the allocator may reuse
    // that address for a non-V2 list before this receipt finishes; finish is
    // keyed by receipt id, so the detached receipt must not block that list.
    retirement_pointer: usize,
    lifecycle: ManagedAnyV2ListLifecycle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedAnyV2ListLifecycle {
    Active,
    Retiring {
        claimant: super::handle::LeaseThreadId,
        origin: ManagedAnyV2ListRetirementOrigin,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedAnyV2ListRetirementOrigin {
    ExplicitDrop,
    TrapDrain,
}

// This registry owns identities only. List destruction and V2 payload Drop
// glue always run after its mutex has been released.
static MANAGED_ANY_V2_LISTS: Mutex<Vec<RegisteredManagedAnyV2List>> = Mutex::new(Vec::new());
static MANAGED_ANY_V2_LISTS_CHANGED: Condvar = Condvar::new();
static NEXT_MANAGED_ANY_V2_LIST_RECEIPT: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
thread_local! {
    static MANAGED_ANY_V2_LIST_REGISTRATION_FAIL: Cell<bool> = const { Cell::new(false) };
    static MANAGED_ANY_V2_LIST_DRAIN_WAIT_HOOK: RefCell<Option<ManagedAnyV2ListDrainWaitTestHook>> = const { RefCell::new(None) };
}

#[cfg(test)]
struct ManagedAnyV2ListDrainWaitTestHook {
    entered: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
fn with_managed_any_v2_list_drain_wait_hook_for_test<T>(
    entered: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<ManagedAnyV2ListDrainWaitTestHook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            MANAGED_ANY_V2_LIST_DRAIN_WAIT_HOOK.with(|hook| {
                *hook.borrow_mut() = self.0.take();
            });
        }
    }

    let previous = MANAGED_ANY_V2_LIST_DRAIN_WAIT_HOOK
        .with(|hook| hook.replace(Some(ManagedAnyV2ListDrainWaitTestHook { entered, resume })));
    let _restore = Restore(previous);
    operation()
}

#[cfg(test)]
fn pause_managed_any_v2_list_drain_wait_for_test() {
    MANAGED_ANY_V2_LIST_DRAIN_WAIT_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            let _ = hook.entered.send(());
            let _ = hook.resume.recv();
        }
    });
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
    let Ok(receipt_id) = NEXT_MANAGED_ANY_V2_LIST_RECEIPT.fetch_update(
        Ordering::Relaxed,
        Ordering::Relaxed,
        |current| current.checked_add(1),
    ) else {
        return false;
    };
    let mut lists = MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let list_identity = list as usize;
    if lists.iter().any(|registered| {
        registered.list == Some(list_identity)
            && registered.domain == ManagedAnyV2ListDomain::CanonicalOwnerListV2
            && registered.lifecycle == ManagedAnyV2ListLifecycle::Active
    }) || lists.try_reserve(1).is_err()
    {
        return false;
    }
    lists.push(RegisteredManagedAnyV2List {
        receipt_id,
        creator: super::handle::current_lease_thread_id(),
        domain: ManagedAnyV2ListDomain::CanonicalOwnerListV2,
        list: Some(list_identity),
        retirement_pointer: list_identity,
        lifecycle: ManagedAnyV2ListLifecycle::Active,
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
        .any(|registered| registered.list == Some(list as usize) && registered.domain == domain)
}

/// Atomically claim an active list for destruction. `None` means that the
/// pointer has no canonical V2 receipt; `Err(())` means another destructor
/// already owns a Retiring receipt and the caller must not touch the list.
pub(super) fn begin_managed_any_v2_list_retirement(
    list: *mut super::MimiList,
    domain: ManagedAnyV2ListDomain,
) -> Result<Option<u64>, ()> {
    if list.is_null() {
        return Ok(None);
    }
    let mut lists = MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(registered) = lists.iter_mut().rev().find(|registered| {
        registered.list == Some(list as usize)
            && registered.domain == domain
            && registered.lifecycle == ManagedAnyV2ListLifecycle::Active
    }) {
        let claimant = super::handle::current_lease_thread_id();
        let receipt_id = registered.receipt_id;
        registered.lifecycle = ManagedAnyV2ListLifecycle::Retiring {
            claimant,
            origin: ManagedAnyV2ListRetirementOrigin::ExplicitDrop,
        };
        return Ok(Some(receipt_id));
    }
    if lists.iter().any(|registered| {
        registered.retirement_pointer == list as usize
            && registered.domain == domain
            && registered.list.is_some()
            && matches!(
                registered.lifecycle,
                ManagedAnyV2ListLifecycle::Retiring { .. }
            )
    }) {
        return Err(());
    }
    Ok(None)
}

enum ManagedAnyV2ListDrainClaim {
    Claimed(*mut super::MimiList, u64),
    Complete,
    Reentrant,
}

/// Claim one Active creator-owned receipt, or wait while a foreign explicit
/// destructor owns one. This serializes the list's element drops with the
/// later creator-domain token sweep, which must not detach those same owners.
fn claim_or_wait_current_thread_managed_any_v2_list_for_drain(
    creator: super::handle::LeaseThreadId,
    claimant: super::handle::LeaseThreadId,
) -> ManagedAnyV2ListDrainClaim {
    let mut lists = MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        if lists.iter().any(|registered| {
            registered.creator == creator
                && registered.domain == ManagedAnyV2ListDomain::CanonicalOwnerListV2
                && matches!(
                    registered.lifecycle,
                    ManagedAnyV2ListLifecycle::Retiring {
                        claimant: retiring_claimant,
                        ..
                    } if retiring_claimant == claimant
                )
        }) {
            // Check every receipt before claiming an Active one: dropping it
            // from this thread would re-enter the creator drain while another
            // receipt is already being destroyed by the same claimant.
            return ManagedAnyV2ListDrainClaim::Reentrant;
        }

        if let Some(index) = lists.iter().rposition(|registered| {
            registered.creator == creator
                && registered.domain == ManagedAnyV2ListDomain::CanonicalOwnerListV2
                && registered.lifecycle == ManagedAnyV2ListLifecycle::Active
        }) {
            let registered = &mut lists[index];
            let receipt_id = registered.receipt_id;
            let Some(list) = registered.list else {
                return ManagedAnyV2ListDrainClaim::Reentrant;
            };
            registered.lifecycle = ManagedAnyV2ListLifecycle::Retiring {
                claimant,
                origin: ManagedAnyV2ListRetirementOrigin::TrapDrain,
            };
            return ManagedAnyV2ListDrainClaim::Claimed(list as *mut super::MimiList, receipt_id);
        }

        if !lists.iter().any(|registered| {
            registered.creator == creator
                && registered.domain == ManagedAnyV2ListDomain::CanonicalOwnerListV2
                && matches!(
                    registered.lifecycle,
                    ManagedAnyV2ListLifecycle::Retiring { .. }
                )
        }) {
            return ManagedAnyV2ListDrainClaim::Complete;
        }

        #[cfg(test)]
        pause_managed_any_v2_list_drain_wait_for_test();

        lists = MANAGED_ANY_V2_LISTS_CHANGED
            .wait(lists)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

/// Verify that a private destructor is acting on the exact receipt claimed by
/// the current thread. This check happens before dereferencing the pointer.
pub(super) fn managed_any_v2_list_retirement_matches(
    list: *const super::MimiList,
    receipt_id: u64,
) -> bool {
    let claimant = super::handle::current_lease_thread_id();
    MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .any(|registered| {
            registered.receipt_id == receipt_id
                && registered.list == Some(list as usize)
                && registered.domain == ManagedAnyV2ListDomain::CanonicalOwnerListV2
                && matches!(
                    registered.lifecycle,
                    ManagedAnyV2ListLifecycle::Retiring {
                        claimant: current_claimant,
                        ..
                    } if current_claimant == claimant
                )
        })
}

/// Detach the pointer only after element and backing-buffer destruction is
/// complete, immediately before freeing the boxed header. The Retiring
/// receipt continues contributing to creator counts until finish-by-id.
pub(super) fn detach_managed_any_v2_list_pointer(receipt_id: u64) -> bool {
    let claimant = super::handle::current_lease_thread_id();
    let mut lists = MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(registered) = lists.iter_mut().find(|registered| {
        registered.receipt_id == receipt_id
            && matches!(
                registered.lifecycle,
                ManagedAnyV2ListLifecycle::Retiring {
                    claimant: current_claimant,
                    ..
                } if current_claimant == claimant
            )
    }) {
        if registered.list.is_some() {
            registered.list = None;
            return true;
        }
    }
    false
}

/// Complete destruction by the unique monotonic receipt. The list pointer is
/// deliberately not the removal key: malloc may reuse it while an older
/// Retiring receipt is still being finalized.
pub(super) fn finish_managed_any_v2_list_retirement(receipt_id: u64) -> bool {
    let claimant = super::handle::current_lease_thread_id();
    let mut lists = MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(index) = lists.iter().position(|registered| {
        registered.receipt_id == receipt_id
            && registered.list.is_none()
            && matches!(
                registered.lifecycle,
                ManagedAnyV2ListLifecycle::Retiring {
                    claimant: current_claimant,
                    ..
                } if current_claimant == claimant
            )
    }) {
        let released_empty_registry = remove_finished_list_receipt(&mut lists, index);
        MANAGED_ANY_V2_LISTS_CHANGED.notify_all();
        drop(lists);
        // The registry is process-global, but its backing allocation has no
        // owner after the last list is retired. Drop it outside the mutex so
        // an idle runtime does not retain list-registry capacity until exit.
        drop(released_empty_registry);
        return true;
    }
    false
}

fn remove_finished_list_receipt(
    lists: &mut Vec<RegisteredManagedAnyV2List>,
    index: usize,
) -> Option<Vec<RegisteredManagedAnyV2List>> {
    lists.remove(index);
    lists.is_empty().then(|| std::mem::take(lists))
}

/// Cancel an explicit claim before destructive work starts. This lets the
/// normal trap drain reclaim an otherwise valid V2 list when the generic
/// deallocator rejects a shallow drop request.
pub(super) fn cancel_managed_any_v2_list_retirement(receipt_id: u64) -> bool {
    let claimant = super::handle::current_lease_thread_id();
    let mut lists = MANAGED_ANY_V2_LISTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(registered) = lists.iter_mut().find(|registered| {
        registered.receipt_id == receipt_id
            && registered.list.is_some()
            && registered.lifecycle
                == (ManagedAnyV2ListLifecycle::Retiring {
                    claimant,
                    origin: ManagedAnyV2ListRetirementOrigin::ExplicitDrop,
                })
    }) {
        registered.lifecycle = ManagedAnyV2ListLifecycle::Active;
        MANAGED_ANY_V2_LISTS_CHANGED.notify_all();
        return true;
    }
    false
}

/// Number of receipts created by the current native thread. Retiring entries
/// remain visible until the complete MimiList teardown finishes. A missing
/// identity is an empty creator domain; a poisoned lock is unknown so runtime
/// guards can fail closed.
pub(super) fn current_thread_managed_any_v2_list_count() -> Option<usize> {
    let Some(creator) = super::handle::cached_lease_thread_id() else {
        return Some(0);
    };
    let lists = MANAGED_ANY_V2_LISTS.lock().ok()?;
    Some(
        lists
            .iter()
            .filter(|registered| {
                registered.creator == creator
                    && registered.domain == ManagedAnyV2ListDomain::CanonicalOwnerListV2
            })
            .count(),
    )
}

/// Retire every V2 owner list created by the trapping native thread. Claim a
/// receipt under the registry lock, then invoke the private typed destructor
/// outside the lock. Remaining independently published tokens are drained by
/// `ordinary_map_domain` after this function.
pub(super) fn drain_current_thread_managed_any_v2_lists() {
    let Some(creator) = super::handle::cached_lease_thread_id() else {
        return;
    };
    let claimant = super::handle::current_lease_thread_id();
    loop {
        let (list, receipt_id) =
            match claim_or_wait_current_thread_managed_any_v2_list_for_drain(creator, claimant) {
                ManagedAnyV2ListDrainClaim::Claimed(list, receipt_id) => (list, receipt_id),
                ManagedAnyV2ListDrainClaim::Complete => return,
                ManagedAnyV2ListDrainClaim::Reentrant => {
                    super::managed_any_v2_list_lifecycle_fail_closed()
                }
            };
        // SAFETY: the exact domain/creator receipt was atomically claimed
        // under the registry lock and is passed through to final removal.
        unsafe { super::mimi_mir_list_drop_managed_any_v2_claimed(list, receipt_id) };
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
    #[cfg(test)]
    ordinary_map_owned_any_v2_values_handle_publish_checkpoint()?;
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

    /// Render this immutable v1 root as a deterministic JSON object. Values
    /// are observed only through their typed Any owner descriptors; no token
    /// or payload address escapes this method.
    fn to_json(&self) -> Result<String, OrdinaryMapSnapshotRuntimeError> {
        let ordered = self.sorted_entries()?;
        let mut output = String::new();
        reserve_json_output(&mut output, 2)?;
        output.push('{');

        for (index, (key, value)) in ordered.into_iter().enumerate() {
            if index != 0 {
                reserve_json_output(&mut output, 1)?;
                output.push(',');
            }
            append_json_string(&mut output, key)?;
            reserve_json_output(&mut output, 1)?;
            output.push(':');

            let descriptor = value.descriptor_identity()?;
            if descriptor.version != 1 {
                return Err(AnyOwnerError::DescriptorMismatch.into());
            }
            match descriptor.kind {
                AnyOwnerKind::I32 => {
                    let integer = value.read_i32()?;
                    reserve_json_output(&mut output, 11)?;
                    use std::fmt::Write as _;
                    write!(&mut output, "{integer}")
                        .map_err(|_| OrdinaryMapSnapshotRuntimeError::Capacity)?;
                }
                AnyOwnerKind::OwnedString => {
                    snapshot_allocation_checkpoint()?;
                    let string = value.copy_string()?;
                    append_json_string(&mut output, &string)?;
                }
                AnyOwnerKind::ManagedV2 => {
                    return Err(AnyOwnerError::DescriptorMismatch.into());
                }
            }
        }

        reserve_json_output(&mut output, 1)?;
        output.push('}');
        Ok(output)
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

/// Append a JSON string using serde_json's compact escaping rules. In
/// particular, embedded NUL and the remaining ASCII control characters use
/// `\u00xx`, while Unicode text stays UTF-8. The exact encoded size is
/// reserved fallibly before writing any bytes.
fn append_json_string(
    output: &mut String,
    value: &str,
) -> Result<(), OrdinaryMapSnapshotRuntimeError> {
    fn encoded_char_len(character: char) -> usize {
        match character {
            '"' | '\\' | '\u{08}' | '\u{0c}' | '\n' | '\r' | '\t' => 2,
            '\u{00}'..='\u{1f}' => 6,
            other => other.len_utf8(),
        }
    }

    let escaped_len = value
        .chars()
        .try_fold(2usize, |length, character| {
            length.checked_add(encoded_char_len(character))
        })
        .ok_or(OrdinaryMapSnapshotRuntimeError::Capacity)?;
    reserve_json_output(output, escaped_len)?;

    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{00}'..='\u{1f}' => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let code = character as u8;
                output.push_str("\\u00");
                output.push(HEX[(code >> 4) as usize] as char);
                output.push(HEX[(code & 0x0f) as usize] as char);
            }
            other => output.push(other),
        }
    }
    output.push('"');
    Ok(())
}

fn reserve_json_output(
    output: &mut String,
    additional: usize,
) -> Result<(), OrdinaryMapSnapshotRuntimeError> {
    snapshot_allocation_checkpoint()?;
    output
        .try_reserve(additional)
        .map_err(|_| OrdinaryMapSnapshotRuntimeError::Capacity)
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
    static ORDINARY_MAP_JSON_SCRATCH_SCOPES: Cell<usize> = const { Cell::new(0) };
    static OWNED_GET_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_GET_ALLOCATION_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_GET_FAILURE_TRIGGERED: Cell<bool> = const { Cell::new(false) };
    // V1 Result-aware New/Set fault injection is deliberately separate from
    // the shared snapshot oracle and the V2 publication failpoint. It fails
    // only after the replacement snapshot has been fully staged.
    static OWNED_GET_RESULT_ROOT_PUBLISH_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_GET_RESULT_ROOT_PUBLISH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_GET_RESULT_ROOT_PUBLISH_FAILURE_TRIGGERED: Cell<bool> = const { Cell::new(false) };
    static OWNED_ANY_V2_VALUES_PUBLISH_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_VALUES_PUBLISH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_VALUES_HANDLE_PUBLISH_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_VALUES_HANDLE_PUBLISH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_UNPACK_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_UNPACK_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_VALUES_UNPACK_OWNER_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_VALUES_UNPACK_OWNER_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_VALUES_CLONE_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_VALUES_CLONE_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_VALUES_CLONE_FAILURE_TRIGGERED: Cell<bool> = const { Cell::new(false) };
    static STRING_CONCAT_FAIL_STAGE: Cell<Option<i32>> = const { Cell::new(None) };
    static STRING_CONCAT_ALLOCATION_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static STRING_CONCAT_PUBLICATION_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
thread_local! {
    // Isolated L3 support for the ordinary Map SetResult publication
    // failpoint. These counters are thread-local so JIT execution can install
    // the failure on its calling thread without changing production behavior.
    static OWNED_ANY_V2_ROOT_PUBLISH_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    // The Result-valued Set/GetAny L3 observer also needs the total number of
    // root publication checkpoints, including successful runs with no active
    // failpoint. Keep that semantic separate from the older failpoint-only
    // counter above, whose contract is consumed by SetResult L3.
    static OWNED_ANY_V2_ROOT_PUBLICATION_CALLS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_GET_ANY_PUBLISH_FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static OWNED_ANY_V2_GET_ANY_PUBLISH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_STRING_COPY_FAIL_STAGE: Cell<Option<i32>> = const { Cell::new(None) };
    static OWNED_ANY_V2_STRING_COPY_CLONE_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_STRING_COPY_PUBLISH_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
    static OWNED_ANY_V2_GET_STRING_TEMP_DROPS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(feature = "ordinary-map-set-result-l3")]
thread_local! {
    static ORDINARY_MAP_SET_RESULT_L3_OBSERVATION: Cell<OrdinaryMapSetResultL3Observation> =
        const { Cell::new(OrdinaryMapSetResultL3Observation::EMPTY) };
    static ORDINARY_MAP_SET_GET_STRING_L3_OBSERVATION: Cell<OrdinaryMapSetGetStringL3Observation> =
        const { Cell::new(OrdinaryMapSetGetStringL3Observation::EMPTY) };
    static ORDINARY_MAP_SET_GET_ANY_RESULT_I32_L3_OBSERVATION:
        Cell<OrdinaryMapSetGetAnyResultI32L3Observation> =
        const { Cell::new(OrdinaryMapSetGetAnyResultI32L3Observation::EMPTY) };
}

/// Tracks the lifetime of temporary serialization storage across the FFI
/// bridge. In test builds this exposes a narrow per-thread balance check for
/// error paths; it does not affect production allocation or ownership.
struct OrdinaryMapJsonScratchScope;

impl OrdinaryMapJsonScratchScope {
    fn enter() -> Self {
        #[cfg(test)]
        ORDINARY_MAP_JSON_SCRATCH_SCOPES.with(|scopes| {
            scopes.set(scopes.get().saturating_add(1));
        });
        Self
    }
}

impl Drop for OrdinaryMapJsonScratchScope {
    fn drop(&mut self) {
        #[cfg(test)]
        ORDINARY_MAP_JSON_SCRATCH_SCOPES.with(|scopes| {
            scopes.set(
                scopes
                    .get()
                    .checked_sub(1)
                    .expect("JSON scratch scope must be balanced"),
            );
        });
    }
}

#[cfg(test)]
fn ordinary_map_json_scratch_scopes_for_test() -> usize {
    ORDINARY_MAP_JSON_SCRATCH_SCOPES.with(Cell::get)
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

/// Inject one failure at a deterministic stage of the canonical Values-owner
/// Clone boundary, independent of allocations used to construct the source
/// Map and snapshot. Stage zero is output-list reservation; each subsequent
/// stage is one payload deep clone in canonical Values order. Reference,
/// bytecode, and native all call the same checkpoint at those boundaries.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_values_clone_capacity_checkpoint_for_test() -> Result<(), ()>
{
    OWNED_ANY_V2_VALUES_CLONE_FAIL_AFTER.with(|fail_after| match fail_after.get() {
        Some(remaining) => {
            OWNED_ANY_V2_VALUES_CLONE_ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
            if remaining == 0 {
                OWNED_ANY_V2_VALUES_CLONE_FAILURE_TRIGGERED.with(|triggered| triggered.set(true));
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
pub(crate) fn ordinary_map_owned_any_v2_values_clone_capacity_attempts_for_test() -> usize {
    OWNED_ANY_V2_VALUES_CLONE_ATTEMPTS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_values_clone_capacity_failure_triggered_for_test() -> bool {
    OWNED_ANY_V2_VALUES_CLONE_FAILURE_TRIGGERED.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_values_clone_failure_at_for_test<T>(
    failure_stage: usize,
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint {
        fail_after: Option<usize>,
        attempts: usize,
        triggered: bool,
    }
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_ANY_V2_VALUES_CLONE_FAIL_AFTER.with(|value| value.set(self.fail_after));
            OWNED_ANY_V2_VALUES_CLONE_ATTEMPTS.with(|value| value.set(self.attempts));
            OWNED_ANY_V2_VALUES_CLONE_FAILURE_TRIGGERED.with(|value| value.set(self.triggered));
        }
    }

    let _restore = RestoreFailpoint {
        fail_after: OWNED_ANY_V2_VALUES_CLONE_FAIL_AFTER
            .with(|value| value.replace(Some(failure_stage))),
        attempts: OWNED_ANY_V2_VALUES_CLONE_ATTEMPTS.with(|value| value.replace(0)),
        triggered: OWNED_ANY_V2_VALUES_CLONE_FAILURE_TRIGGERED.with(|value| value.replace(false)),
    };
    operation()
}

#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_values_clone_capacity_failure_for_test<T>(
    operation: impl FnOnce() -> T,
) -> T {
    with_ordinary_map_owned_any_v2_values_clone_failure_at_for_test(0, operation)
}

#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
pub fn managed_any_v2_list_count_for_test() -> Option<usize> {
    current_thread_managed_any_v2_list_count()
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

/// Private test-only failpoint at the V1 Result root-publication boundary.
/// Unlike the allocation oracle above, it is reached only after a complete
/// replacement snapshot exists, so a failed Set proves that staged owners
/// are released while the source root remains registered and usable.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_get_result_root_publish_checkpoint_for_test() -> Result<(), ()> {
    OWNED_GET_RESULT_ROOT_PUBLISH_FAIL_AFTER.with(|fail_after| match fail_after.get() {
        Some(remaining) => {
            OWNED_GET_RESULT_ROOT_PUBLISH_ATTEMPTS.with(|attempts| {
                attempts.set(attempts.get().saturating_add(1));
            });
            if remaining == 0 {
                OWNED_GET_RESULT_ROOT_PUBLISH_FAILURE_TRIGGERED
                    .with(|triggered| triggered.set(true));
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
fn ordinary_map_owned_get_result_root_publish_attempts_for_test() -> usize {
    OWNED_GET_RESULT_ROOT_PUBLISH_ATTEMPTS.with(Cell::get)
}

#[cfg(test)]
fn ordinary_map_owned_get_result_root_publish_failure_triggered_for_test() -> bool {
    OWNED_GET_RESULT_ROOT_PUBLISH_FAILURE_TRIGGERED.with(Cell::get)
}

#[cfg(test)]
fn with_ordinary_map_owned_get_result_root_publish_failure_for_test<T>(
    successful_publications_before_failure: usize,
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint {
        fail_after: Option<usize>,
        attempts: usize,
        triggered: bool,
    }
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_GET_RESULT_ROOT_PUBLISH_FAIL_AFTER.with(|value| value.set(self.fail_after));
            OWNED_GET_RESULT_ROOT_PUBLISH_ATTEMPTS.with(|value| value.set(self.attempts));
            OWNED_GET_RESULT_ROOT_PUBLISH_FAILURE_TRIGGERED.with(|value| value.set(self.triggered));
        }
    }

    let _restore = RestoreFailpoint {
        fail_after: OWNED_GET_RESULT_ROOT_PUBLISH_FAIL_AFTER
            .with(|value| value.replace(Some(successful_publications_before_failure))),
        attempts: OWNED_GET_RESULT_ROOT_PUBLISH_ATTEMPTS.with(|value| value.replace(0)),
        triggered: OWNED_GET_RESULT_ROOT_PUBLISH_FAILURE_TRIGGERED
            .with(|value| value.replace(false)),
    };
    operation()
}

/// A shared logical failpoint at the V2 root publication boundary. Native,
/// reference, and bytecode consumers invoke this only after staging a complete
/// replacement root and immediately before publishing it. The failed outcome
/// therefore leaves the old root and every input owner unchanged.
#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
pub(crate) fn ordinary_map_owned_any_v2_root_publish_checkpoint_for_test() -> Result<(), ()> {
    OWNED_ANY_V2_ROOT_PUBLICATION_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
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

#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
pub fn ordinary_map_owned_any_v2_root_publish_attempts_for_test() -> usize {
    OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS.with(Cell::get)
}

#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
pub(crate) fn ordinary_map_owned_any_v2_root_publication_calls_for_test() -> usize {
    OWNED_ANY_V2_ROOT_PUBLICATION_CALLS.with(Cell::get)
}

/// Install one thread-local root-publication failure and restore the prior
/// oracle state even if the operation unwinds.
#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
pub fn with_ordinary_map_owned_any_v2_root_publish_failure_for_test<T>(
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
#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
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

#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
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

/// Fail after the opaque V2 Values snapshot is staged and just before its
/// handle is inserted into the publication registry.
#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_values_handle_publish_checkpoint_for_test() -> Result<(), ()>
{
    OWNED_ANY_V2_VALUES_HANDLE_PUBLISH_FAIL_AFTER.with(|fail_after| match fail_after.get() {
        Some(remaining) => {
            OWNED_ANY_V2_VALUES_HANDLE_PUBLISH_ATTEMPTS
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
pub(crate) fn ordinary_map_owned_any_v2_values_handle_publish_attempts_for_test() -> usize {
    OWNED_ANY_V2_VALUES_HANDLE_PUBLISH_ATTEMPTS.with(Cell::get)
}

/// Install one thread-local opaque Values-handle publication failure and
/// restore the prior oracle state even if the operation unwinds.
#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_values_handle_publish_failure_for_test<T>(
    stages_before_failure: usize,
    operation: impl FnOnce() -> T,
) -> T {
    struct RestoreFailpoint {
        fail_after: Option<usize>,
        attempts: usize,
    }
    impl Drop for RestoreFailpoint {
        fn drop(&mut self) {
            OWNED_ANY_V2_VALUES_HANDLE_PUBLISH_FAIL_AFTER.with(|value| value.set(self.fail_after));
            OWNED_ANY_V2_VALUES_HANDLE_PUBLISH_ATTEMPTS.with(|value| value.set(self.attempts));
        }
    }

    let _restore = RestoreFailpoint {
        fail_after: OWNED_ANY_V2_VALUES_HANDLE_PUBLISH_FAIL_AFTER
            .with(|value| value.replace(Some(stages_before_failure))),
        attempts: OWNED_ANY_V2_VALUES_HANDLE_PUBLISH_ATTEMPTS.with(|value| value.replace(0)),
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

/// Shared test oracle for the final canonical String copy used by V2 Unpack
/// and ValuesUnpack. Stage 0 is allocation; stage 1 is publication after the
/// complete byte copy exists but before it becomes an owned language value.
#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
pub(crate) fn ordinary_map_owned_any_v2_string_copy_checkpoint_for_test(
    stage: i32,
) -> Result<(), ()> {
    let attempts = match stage {
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_CLONE_STAGE => {
            &OWNED_ANY_V2_STRING_COPY_CLONE_ATTEMPTS
        }
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_PUBLISH_STAGE => {
            &OWNED_ANY_V2_STRING_COPY_PUBLISH_ATTEMPTS
        }
        _ => return Ok(()),
    };
    attempts.with(|count| count.set(count.get().saturating_add(1)));
    if OWNED_ANY_V2_STRING_COPY_FAIL_STAGE.with(Cell::get) == Some(stage) {
        Err(())
    } else {
        Ok(())
    }
}

#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
pub(crate) fn ordinary_map_owned_any_v2_string_copy_attempts_for_test(stage: i32) -> usize {
    match stage {
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_CLONE_STAGE => {
            OWNED_ANY_V2_STRING_COPY_CLONE_ATTEMPTS.with(Cell::get)
        }
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_PUBLISH_STAGE => {
            OWNED_ANY_V2_STRING_COPY_PUBLISH_ATTEMPTS.with(Cell::get)
        }
        _ => 0,
    }
}

#[cfg(test)]
fn ordinary_map_owned_any_v2_get_string_temp_drops_for_test() -> usize {
    OWNED_ANY_V2_GET_STRING_TEMP_DROPS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_string_copy_failure_for_test<T>(
    stage: i32,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore {
        fail_stage: Option<i32>,
        clone_attempts: usize,
        publish_attempts: usize,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            OWNED_ANY_V2_STRING_COPY_FAIL_STAGE.with(|value| value.set(self.fail_stage));
            OWNED_ANY_V2_STRING_COPY_CLONE_ATTEMPTS.with(|value| value.set(self.clone_attempts));
            OWNED_ANY_V2_STRING_COPY_PUBLISH_ATTEMPTS
                .with(|value| value.set(self.publish_attempts));
        }
    }
    let _restore = Restore {
        fail_stage: OWNED_ANY_V2_STRING_COPY_FAIL_STAGE.with(|value| value.replace(Some(stage))),
        clone_attempts: OWNED_ANY_V2_STRING_COPY_CLONE_ATTEMPTS.with(|value| value.replace(0)),
        publish_attempts: OWNED_ANY_V2_STRING_COPY_PUBLISH_ATTEMPTS.with(|value| value.replace(0)),
    };
    operation()
}

#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_values_unpack_string_clone_checkpoint_for_test(
) -> Result<(), ()> {
    ordinary_map_owned_any_v2_string_copy_checkpoint_for_test(
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_CLONE_STAGE,
    )
}

#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_values_unpack_string_publish_checkpoint_for_test(
) -> Result<(), ()> {
    ordinary_map_owned_any_v2_string_copy_checkpoint_for_test(
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_PUBLISH_STAGE,
    )
}

#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_values_unpack_string_clone_failure_for_test<T>(
    operation: impl FnOnce() -> T,
) -> T {
    with_ordinary_map_owned_any_v2_string_copy_failure_for_test(
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_CLONE_STAGE,
        operation,
    )
}

#[cfg(test)]
pub(crate) fn with_ordinary_map_owned_any_v2_values_unpack_string_publish_failure_for_test<T>(
    operation: impl FnOnce() -> T,
) -> T {
    with_ordinary_map_owned_any_v2_string_copy_failure_for_test(
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_PUBLISH_STAGE,
        operation,
    )
}

pub(super) fn ordinary_map_owned_any_v2_values_unpack_owner_checkpoint() -> Result<(), ()> {
    #[cfg(test)]
    {
        return ordinary_map_owned_any_v2_values_unpack_owner_checkpoint_for_test();
    }
    #[cfg(not(test))]
    Ok(())
}

fn ordinary_map_owned_any_v2_string_copy_checkpoint(stage: i32) -> Result<(), ManagedAnyStatus> {
    #[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
    {
        return ordinary_map_owned_any_v2_string_copy_checkpoint_for_test(stage)
            .map_err(|_| ManagedAnyStatus::Capacity);
    }
    #[cfg(not(any(test, feature = "ordinary-map-set-result-l3")))]
    {
        let _ = stage;
        Ok(())
    }
}

fn ordinary_map_owned_any_v2_unpack_string_clone_checkpoint() -> Result<(), ManagedAnyStatus> {
    ordinary_map_owned_any_v2_string_copy_checkpoint(
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_CLONE_STAGE,
    )
}

fn ordinary_map_owned_any_v2_unpack_string_publish_checkpoint() -> Result<(), ManagedAnyStatus> {
    ordinary_map_owned_any_v2_string_copy_checkpoint(
        ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_PUBLISH_STAGE,
    )
}

fn ordinary_map_owned_any_v2_values_unpack_string_clone_checkpoint() -> Result<(), ManagedAnyStatus>
{
    ordinary_map_owned_any_v2_unpack_string_clone_checkpoint()
}

fn ordinary_map_owned_any_v2_values_unpack_string_publish_checkpoint(
) -> Result<(), ManagedAnyStatus> {
    ordinary_map_owned_any_v2_unpack_string_publish_checkpoint()
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
    #[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
    {
        return ordinary_map_owned_any_v2_get_any_publish_checkpoint_for_test()
            .map_err(|_| ManagedAnyStatus::Capacity);
    }
    #[cfg(not(any(test, feature = "ordinary-map-set-result-l3")))]
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

#[cfg(test)]
fn ordinary_map_owned_any_v2_values_handle_publish_checkpoint() -> Result<(), ManagedAnyStatus> {
    ordinary_map_owned_any_v2_values_handle_publish_checkpoint_for_test()
        .map_err(|_| ManagedAnyStatus::Capacity)
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

fn ordinary_map_owner_lifecycle_i32_string_v1_to_json(
    handle: i64,
) -> Result<String, OrdinaryMapSnapshotRuntimeError> {
    let root = acquire_root(handle)?;
    snapshot_from_map(&root)?.to_json()
}

fn record_json_failure(error: OrdinaryMapSnapshotRuntimeError) {
    let handle_error = match error {
        OrdinaryMapSnapshotRuntimeError::Handle(error) => error,
        OrdinaryMapSnapshotRuntimeError::InvalidInput
        | OrdinaryMapSnapshotRuntimeError::Capacity
        | OrdinaryMapSnapshotRuntimeError::SizeOverflow
        | OrdinaryMapSnapshotRuntimeError::Owner(_) => super::handle::HandleError::Invalid,
    };
    super::handle::set_handle_error(handle_error);
}

/// Render a lifecycle-v1 ordinary Map root as a detached JSON C string.
///
/// Success returns a fresh allocation from Mimi's allocator. The caller owns
/// it and must release it exactly once with `mimi_string_free`. On an invalid,
/// unavailable, or unformattable root this returns null and records the
/// failure in `mimi_handle_last_error`; it never substitutes an empty object.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_to_json(
    handle: i64,
) -> *mut std::ffi::c_char {
    let _scratch_scope = OrdinaryMapJsonScratchScope::enter();
    super::handle::clear_handle_error();
    let rendered = match ordinary_map_owner_lifecycle_i32_string_v1_to_json(handle) {
        Ok(rendered) => rendered,
        Err(error) => {
            record_json_failure(error);
            return std::ptr::null_mut();
        }
    };

    // Keep detached-output capacity in the same deterministic allocation
    // oracle as sorting and serialization. If this checkpoint fails, `rendered`
    // is dropped here and no output pointer has been published.
    if snapshot_allocation_checkpoint().is_err() {
        record_json_failure(OrdinaryMapSnapshotRuntimeError::Capacity);
        return std::ptr::null_mut();
    }

    let detached = super::alloc_c_string_from_bytes(rendered.as_bytes());
    if detached.is_null() {
        record_json_failure(OrdinaryMapSnapshotRuntimeError::Capacity);
    }
    detached
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

const ORDINARY_MAP_OWNER_LIFECYCLE_SIZE_OK: i32 = 0;
const ORDINARY_MAP_OWNER_LIFECYCLE_SIZE_OVERFLOW: i32 = 1;
const ORDINARY_MAP_OWNER_LIFECYCLE_SIZE_FAILURE: i32 = -1;

unsafe fn ordinary_map_owner_lifecycle_size_status_from_result(
    result: Result<i32, OrdinaryMapSnapshotRuntimeError>,
    out_size: *mut i32,
) -> i32 {
    match result {
        Ok(size) => {
            if out_size.is_null() {
                return ORDINARY_MAP_OWNER_LIFECYCLE_SIZE_FAILURE;
            }
            unsafe { out_size.write(size) };
            ORDINARY_MAP_OWNER_LIFECYCLE_SIZE_OK
        }
        Err(OrdinaryMapSnapshotRuntimeError::SizeOverflow) => {
            ORDINARY_MAP_OWNER_LIFECYCLE_SIZE_OVERFLOW
        }
        Err(_) => ORDINARY_MAP_OWNER_LIFECYCLE_SIZE_FAILURE,
    }
}

/// Return success=0 and write the checked i32 entry count, overflow=1 for an
/// i32 size overflow, or -1 for an invalid root or other runtime failure. The
/// output pointer is examined only after a successful size query.
///
/// # Safety
/// If `handle` names a live lifecycle-v1 root and `out_size` is non-null, the
/// pointer must be writable for one `i32`. Null is accepted as a failure
/// output; it is not dereferenced.
#[no_mangle]
pub(crate) unsafe extern "C" fn mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size_status(
    handle: i64,
    out_size: *mut i32,
) -> i32 {
    let result = size_root(handle);
    unsafe { ordinary_map_owner_lifecycle_size_status_from_result(result, out_size) }
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

/// Status codes for the owned-Get V1 Result-aware New ABI. These are the
/// source `Map::new` contract values: `Ok(root)` is 0 and a recoverable
/// capacity failure is exactly `Err(3)`.
pub const ORDINARY_MAP_OWNED_GET_NEW_RESULT_OK: i32 = 0;
pub const ORDINARY_MAP_OWNED_GET_NEW_RESULT_CAPACITY: i32 = 3;

/// Status codes for the owned-Get V1 Result-aware Set ABIs. These mirror the
/// source `Map::set` contract: `Ok(root)` is 0 and a recoverable capacity
/// failure is exactly `Err(2)`.
pub const ORDINARY_MAP_OWNED_GET_SET_RESULT_OK: i32 = 0;
pub const ORDINARY_MAP_OWNED_GET_SET_RESULT_CAPACITY: i32 = 2;

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

/// Create a fresh owned-Get V1 root for a Result-aware New/Set call. The
/// ordinary V1 profile has only one recoverable runtime error: capacity
/// exhaustion (snapshot/owner/handle/domain reservation). The owner table's
/// invalid, stale, descriptor, poison, and invariant errors are preserved as
/// non-capacity errors and must reach the trap adapter.
fn new_owned_get_result_root(
    snapshot: OrdinaryMapSnapshot,
) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    snapshot_allocation_checkpoint()?;
    #[cfg(test)]
    ordinary_map_owned_get_result_root_publish_checkpoint_for_test()
        .map_err(|_| OrdinaryMapSnapshotRuntimeError::Capacity)?;
    match super::handle::map_new_handle_fallible(
        super::MimiMap::ordinary_map_owned_get_i32_string_v1(snapshot),
    ) {
        Ok(handle) => super::ordinary_map_domain::publish_new_root(
            super::ordinary_map_domain::OrdinaryMapProfile::OwnedGetI32StringV1,
            handle,
        )
        // `publish_new_root` returns only `OrdinaryMapDomainError::Capacity`
        // and retires the unpublished handle before returning it.
        .map_err(|_| OrdinaryMapSnapshotRuntimeError::Capacity),
        Err(uninstalled_map) => {
            // The handle allocator returns failed roots after releasing its
            // table lock, so their Any owner Drop glue runs outside it.
            drop(uninstalled_map);
            Err(OrdinaryMapSnapshotRuntimeError::Capacity)
        }
    }
}

fn derive_owned_get_result_root(
    handle: i64,
    update: impl FnOnce(
        &OrdinaryMapSnapshot,
    ) -> Result<OrdinaryMapSnapshot, OrdinaryMapSnapshotRuntimeError>,
) -> Result<i64, OrdinaryMapSnapshotRuntimeError> {
    // Validate the owner before key/value copying can fail. A stale or wrong
    // profile handle therefore cannot be reported as source Err(capacity).
    let source = acquire_owned_get_root(handle)?;
    let snapshot = update(snapshot_from_map(&source)?)?;
    drop(source);
    // The handle remains published in its original profile. Only the borrowed
    // lease was released; a failed publication leaves that source root live.
    new_owned_get_result_root(snapshot)
}

fn owned_get_result_capacity_status(
    error: OrdinaryMapSnapshotRuntimeError,
    capacity_status: i32,
) -> Option<i32> {
    match error {
        OrdinaryMapSnapshotRuntimeError::Capacity => Some(capacity_status),
        OrdinaryMapSnapshotRuntimeError::InvalidInput
        | OrdinaryMapSnapshotRuntimeError::SizeOverflow
        | OrdinaryMapSnapshotRuntimeError::Owner(_)
        | OrdinaryMapSnapshotRuntimeError::Handle(_) => None,
    }
}

/// Run a Result-aware owned-Get root operation and publish exactly one valid
/// root through `out_root` on success. Only explicit capacity failure becomes
/// a source Err; all invalid inputs, owner/handle errors, and invariants trap.
unsafe fn owned_get_result_out_root(
    out_root: *mut i64,
    capacity_status: i32,
    operation: &'static [u8],
    create: impl FnOnce() -> Result<i64, OrdinaryMapSnapshotRuntimeError>,
) -> i32 {
    if out_root.is_null() {
        runtime_abort_for(OrdinaryMapSnapshotRuntimeError::InvalidInput, operation);
    }
    // Clear the output before any allocation or ownership change, so every
    // recoverable Err has the canonical `(status, out_root=0)` shape.
    unsafe { out_root.write(0) };
    match create() {
        Ok(root) if root != 0 => {
            unsafe { out_root.write(root) };
            ORDINARY_MAP_OWNED_GET_NEW_RESULT_OK
        }
        Ok(_) => runtime_abort_for(OrdinaryMapSnapshotRuntimeError::InvalidInput, operation),
        Err(error) => match owned_get_result_capacity_status(error, capacity_status) {
            Some(status) => status,
            None => runtime_abort_for(error, operation),
        },
    }
}

/// Create an empty owned-Get V1 root using the source `Map::new` Result
/// contract. Return 0 and write a nonzero root on success, or return the exact
/// source capacity error `Err(3)` with `out_root=0`. Invalid output pointers,
/// owner/invariant errors, and every non-capacity failure are nonrecoverable.
///
/// # Safety
/// `out_root` must be writable for one `i64`; null is treated as invalid
/// input and traps before creating a root.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_new_result(
    out_root: *mut i64,
) -> i32 {
    unsafe {
        owned_get_result_out_root(
            out_root,
            ORDINARY_MAP_OWNED_GET_NEW_RESULT_CAPACITY,
            b"[E0800] ordinary Map owned-Get NewResult failed\0",
            || new_owned_get_result_root(OrdinaryMapSnapshot::new()),
        )
    }
}

/// Create a persistent owned-Get V1 root with an i32 entry using the source
/// `Map::set` Result contract. Success returns 0 and writes a nonzero root;
/// capacity returns the exact source `Err(2)` with `out_root=0`. The input
/// root remains valid on both success and recoverable failure.
///
/// # Safety
/// For positive `key_len`, `key` must reference that many readable valid
/// UTF-8 bytes. `handle` must be a live owned-Get V1 root. `out_root` must be
/// writable for one `i64`; null is invalid input and traps before the update.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32_result(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: i32,
    out_root: *mut i64,
) -> i32 {
    unsafe {
        owned_get_result_out_root(
            out_root,
            ORDINARY_MAP_OWNED_GET_SET_RESULT_CAPACITY,
            b"[E0800] ordinary Map owned-Get SetI32Result failed\0",
            || {
                derive_owned_get_result_root(handle, |source| {
                    let key_bytes = checked_abi_bytes(key, key_len)?;
                    let key = checked_key(key_bytes)?;
                    source.set_i32(&key, value).map_err(Into::into)
                })
            },
        )
    }
}

/// Create a persistent owned-Get V1 root with a String entry using the source
/// `Map::set` Result contract. Success returns 0 and writes a nonzero root;
/// capacity returns the exact source `Err(2)` with `out_root=0`. The input
/// root remains valid on both success and recoverable failure.
///
/// # Safety
/// For positive lengths, both pointers must reference readable valid UTF-8
/// byte ranges of the declared lengths. `handle` must be a live owned-Get V1
/// root. `out_root` must be writable for one `i64`; null is invalid input and
/// traps before the update.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string_result(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: *const u8,
    value_len: i64,
    out_root: *mut i64,
) -> i32 {
    unsafe {
        owned_get_result_out_root(
            out_root,
            ORDINARY_MAP_OWNED_GET_SET_RESULT_CAPACITY,
            b"[E0800] ordinary Map owned-Get SetStringResult failed\0",
            || {
                derive_owned_get_result_root(handle, |source| {
                    let key_bytes = checked_abi_bytes(key, key_len)?;
                    let value_bytes = checked_abi_bytes(value, value_len)?;
                    let key = checked_key(key_bytes)?;
                    let value = std::str::from_utf8(value_bytes)
                        .map_err(|_| OrdinaryMapSnapshotRuntimeError::InvalidInput)?;
                    source.set_string(&key, value).map_err(Into::into)
                })
            },
        )
    }
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
    #[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
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

fn canonical_i32_identity() -> ManagedAnyPayloadV2Identity {
    ManagedAnyPayloadV2Identity {
        tag: ManagedAnyPayloadV2Tag::I32,
        type_id: ManagedAnyPayloadV2TypeId::parse(
            super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID,
        )
        .expect("the built-in Core i32 TypeDesc identity is canonical"),
        descriptor_hash: super::managed_any_payload_v2::CANONICAL_I32_TYPE_DESC_HASH,
        size: std::mem::size_of::<i32>(),
        align: std::mem::align_of::<i32>(),
    }
}

// These callbacks are supplied only to enter the existing typed-payload
// constructor. `register_descriptor` replaces them with the runtime's pinned
// canonical Core i32 memcpy/no-op glue before invoking any callback.
unsafe extern "C" fn clone_ordinary_map_set_i32(destination: *mut u8, source: *const u8) -> i32 {
    // SAFETY: the caller supplies the exact canonical i32 descriptor and
    // aligned four-byte payload slots.
    unsafe { std::ptr::copy_nonoverlapping(source.cast::<i32>(), destination.cast::<i32>(), 1) };
    0
}

unsafe extern "C" fn drop_ordinary_map_set_i32(_payload: *mut u8) {}

fn ordinary_map_set_i32_value(value: i32) -> Result<OwnedAnyV2, ManagedAnyStatus> {
    let identity = canonical_i32_identity();
    debug_assert!(super::managed_any_payload_v2::is_canonical_i32_identity(
        identity
    ));
    let glue = ManagedAnyPayloadV2Glue {
        clone: clone_ordinary_map_set_i32,
        drop: drop_ordinary_map_set_i32,
    };
    // `value` is a live aligned i32 for this complete call, and the exact
    // canonical Core identity selects runtime-owned scalar glue.
    OwnedAnyV2::from_borrowed(identity, glue, (&value as *const i32).cast::<u8>())
        .map_err(ManagedAnyStatus::from)
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
///
/// # Safety
/// `key` must span `key_len` readable UTF-8 bytes when non-empty.
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
///
/// # Safety
/// `label`, when non-null, must point to a readable `MimiMapAnyOwnerV2Label`,
/// and `key` / `label.text` must span their declared readable UTF-8 bytes.
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
///
/// # Safety
/// `key`/`text` must span `key_len`/`text_len` readable UTF-8 bytes when non-empty.
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

/// Forward an `i32` entry update to [`mimi_mir_ordinary_map_any_owner_v2_record_set_i32`].
///
/// # Safety
/// `key` must span `key_len` readable UTF-8 bytes when non-empty.
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
///
/// # Safety
/// `key` must span `key_len` readable UTF-8 bytes when non-empty.
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
///
/// # Safety
/// `key` must span `key_len` readable UTF-8 bytes when non-empty.
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

/// Recoverable capacity status for the exact canonical-i32 SetResult ABI.
/// This is deliberately distinct from New's status 3 and the generic V2
/// helpers' `ManagedAnyStatus::TypeMismatch` status 2.
pub const ORDINARY_MAP_OWNED_ANY_V2_SET_CAPACITY: i32 = 2;
pub const ORDINARY_MAP_OWNED_ANY_V2_SET_INVALID_INPUT: i32 = 1;
const ORDINARY_MAP_OWNED_ANY_V2_SET_TYPE_MISMATCH: i32 = 18;

/// Status protocol for the independent borrowed typed-i32 GetResult ABI.
/// Unknown/internal failures remain outside the language `Result` and must
/// fail closed in each execution adapter.
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_I32_OK: i32 = 0;
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_I32_MISSING: i32 = 1;
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_I32_TYPE_MISMATCH: i32 = 2;
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_I32_CAPACITY: i32 = 3;
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_I32_INTERNAL_ERROR: i32 = 4;

fn ordinary_map_owned_any_v2_get_i32_result_error_status(error: OrdinaryMapSnapshotError) -> i32 {
    match error {
        OrdinaryMapSnapshotError::Capacity
        | OrdinaryMapSnapshotError::Owner(AnyOwnerError::Capacity) => {
            ORDINARY_MAP_OWNED_ANY_V2_GET_I32_CAPACITY
        }
        _ => ORDINARY_MAP_OWNED_ANY_V2_GET_I32_INTERNAL_ERROR,
    }
}

fn ordinary_map_owned_any_v2_set_i32_status(status: ManagedAnyStatus) -> i32 {
    match status {
        ManagedAnyStatus::Capacity => ORDINARY_MAP_OWNED_ANY_V2_SET_CAPACITY,
        // Preserve this impossible-for-the-built-in-i32-path failure as a
        // nonrecoverable code that cannot be mistaken for Set capacity.
        ManagedAnyStatus::TypeMismatch => ORDINARY_MAP_OWNED_ANY_V2_SET_TYPE_MISMATCH,
        other => map_get_status(other),
    }
}

/// Dedicated status ABI for the canonical String SetResult path. Capacity is
/// the only recoverable Set failure; callback failures and all owner/domain
/// errors keep their nonrecoverable ManagedAny status codes.
pub const ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_OK: i32 = 0;
pub const ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_INVALID_INPUT: i32 = 1;
pub const ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_CAPACITY: i32 = 2;
pub const ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_TYPE_MISMATCH: i32 = 18;

/// Dedicated status ABI for a borrowed String GetResult. Only absence,
/// descriptor mismatch, and failure to allocate/publish the final canonical
/// String are source-language Result errors. Every other status is internal.
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_OK: i32 = 0;
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_MISSING: i32 = 1;
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_TYPE_MISMATCH: i32 = 2;
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_CAPACITY: i32 = 3;
pub const ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_INTERNAL_ERROR: i32 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct OrdinaryMapOwnedAnyV2StringBox {
    data: *mut std::ffi::c_char,
    length: i64,
}

fn ordinary_map_owned_any_v2_string_identity(
    type_id: *const u8,
    type_id_len: i64,
    descriptor_hash: *const u8,
) -> Result<ManagedAnyPayloadV2Identity, ManagedAnyStatus> {
    let identity = unsafe {
        managed_v2_identity_from_abi(
            ManagedAnyPayloadV2Tag::String as i32,
            type_id,
            type_id_len,
            descriptor_hash,
            std::mem::size_of::<OrdinaryMapOwnedAnyV2StringBox>() as i64,
            std::mem::align_of::<OrdinaryMapOwnedAnyV2StringBox>() as i64,
        )
    }?;
    if identity.size != std::mem::size_of::<OrdinaryMapOwnedAnyV2StringBox>()
        || identity.align != std::mem::align_of::<OrdinaryMapOwnedAnyV2StringBox>()
    {
        return Err(ManagedAnyStatus::TypeMismatch);
    }
    Ok(identity)
}

fn ordinary_map_owned_any_v2_set_string_status(status: ManagedAnyStatus) -> i32 {
    match status {
        ManagedAnyStatus::Ok => ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_OK,
        ManagedAnyStatus::InvalidInput => ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_INVALID_INPUT,
        ManagedAnyStatus::Capacity => ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_CAPACITY,
        ManagedAnyStatus::TypeMismatch => ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_TYPE_MISMATCH,
        other => map_get_status(other),
    }
}

struct V2UnpackedStringGuard(OrdinaryMapOwnedAnyV2StringBox);

impl Drop for V2UnpackedStringGuard {
    fn drop(&mut self) {
        // Canonical Any/String V2 clone glue allocates the String bytes with
        // C malloc and its pinned Drop glue calls C free. Use the same pair
        // for this short-lived unpack clone; it is not a Mimi-owned String.
        if !self.0.data.is_null() {
            // SAFETY: this pointer came from the exact canonical String clone
            // callback pinned in the entry's descriptor receipt.
            unsafe { system_libc::free(self.0.data.cast()) };
        }
        #[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
        OWNED_ANY_V2_GET_STRING_TEMP_DROPS.with(|drops| {
            drops.set(drops.get().saturating_add(1));
        });
    }
}

fn status_code(status: ManagedAnyStatus) -> i32 {
    status as i32
}

/// Create and publish a fresh V2 owner. Input is borrowed; generated recursive
/// clone glue creates the private payload allocation. Only compiler-native
/// declarations may call this reserved helper.
///
/// # Safety
/// `type_id` spans `type_id_len` bytes, `descriptor_hash` spans 32 raw bytes,
/// and `payload` has the declared size/alignment. Noncanonical-i32 callbacks
/// are trusted compiler-generated glue bound to that exact V2 descriptor
/// receipt. For the exact Core i32 identity/hash/layout, the runtime replaces
/// supplied callbacks with its built-in memcpy/no-op pair.
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

/// Copy callback-owned V2 String/Label bytes into Mimi's canonical String
/// domain and publish the pointer only after registration succeeds. This
/// status ABI distinguishes a failed empty-string allocation from a valid
/// empty representation and has no language-level length cap.
///
/// # Safety
/// `source` points to `length` readable bytes when length is nonzero. Each
/// non-null output points to one writable, non-overlapping value.
unsafe fn copy_ordinary_map_owned_any_v2_string(
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
        // This test-only shared checkpoint represents failure to allocate the
        // final canonical String; it is a no-op in production builds.
        managed_allocation_checkpoint()?;
        ordinary_map_owned_any_v2_unpack_string_clone_checkpoint()?;
        let pointer = super::alloc_c_string_from_bytes(bytes);
        if pointer.is_null() {
            return Err(ManagedAnyStatus::Capacity);
        }
        ordinary_map_owned_any_v2_unpack_string_publish_checkpoint().inspect_err(|_| {
            super::mimi_free(pointer.cast());
        })?;
        if let Err(error) =
            super::ordinary_map_domain::register_fresh_owned_string(pointer, length_usize)
        {
            // Registration returned after releasing its registry lock. Retire
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

/// Final owned String copy for ordinary V2 Unpack. The typed callback
/// temporary remains borrowed until the native emitter drops it after this
/// status-returning bridge completes.
///
/// # Safety
/// `source` points to `length` readable bytes when length is nonzero. Each
/// non-null output points to one writable, non-overlapping value.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_any_v2_unpack_copy_string(
    source: *const std::ffi::c_char,
    length: i64,
    out_pointer: *mut *mut std::ffi::c_char,
    out_length: *mut i64,
) -> i32 {
    // SAFETY: this is the same byte and output contract documented above.
    unsafe { copy_ordinary_map_owned_any_v2_string(source, length, out_pointer, out_length) }
}

/// Final owned String copy for indexed ValuesUnpack.
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
    // SAFETY: this is the same byte and output contract documented above.
    unsafe { copy_ordinary_map_owned_any_v2_string(source, length, out_pointer, out_length) }
}

/// Create an empty V2 ordinary Map root with its exact profile.
///
/// # Safety
/// When `out_root` is non-null, it must point to one writable `i64`.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_new(out_root: *mut i64) -> i32 {
    if !out_root.is_null() {
        unsafe { *out_root = 0 };
    }
    if out_root.is_null() {
        return map_get_status(ManagedAnyStatus::InvalidInput);
    }
    let status = match new_owned_any_v2_root(OrdinaryMapAnyV2Snapshot::new()) {
        Ok(root) => {
            unsafe { *out_root = root };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(error) => map_get_status(ManagedAnyStatus::from(error)),
    };
    #[cfg(feature = "ordinary-map-set-result-l3")]
    ordinary_map_set_get_any_result_i32_l3_record_new(status, out_root);
    status
}

/// Create a persistent V2 root with one canonical Core `i32` value. The
/// source handle is borrowed and remains live on every return path. Success
/// publishes one new root (`status=0`); capacity failure is the exact
/// recoverable SetResult (`status=2`, `out_root=0`). Every other nonzero
/// status is nonrecoverable for this ABI.
///
/// # Safety
/// `out_root` points to a writable `i64`. For positive lengths, `key` points
/// to `key_len` readable UTF-8 bytes for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_set_i32_result(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value: i32,
    out_root: *mut i64,
) -> i32 {
    #[cfg(feature = "ordinary-map-set-result-l3")]
    let (source_before_status, source_before_size) = {
        let mut size = i32::MIN;
        // SAFETY: `size` is a writable local output and `handle` is the
        // borrowed source supplied by the generated SetResult instruction.
        let status = unsafe { mimi_ordinary_map_owned_any_v2_size(handle, &mut size) };
        (status, size)
    };

    if !out_root.is_null() {
        unsafe { *out_root = 0 };
    }
    let status = if out_root.is_null() {
        ORDINARY_MAP_OWNED_ANY_V2_SET_INVALID_INPUT
    } else {
        let result = (|| {
            // Validate the exact source profile before key parsing or allocation.
            let source = acquire_owned_any_v2_root(handle).map_err(ManagedAnyStatus::from)?;
            let key_bytes =
                unsafe { checked_abi_bytes(key, key_len) }.map_err(ManagedAnyStatus::from)?;
            let key = std::str::from_utf8(key_bytes).map_err(|_| ManagedAnyStatus::InvalidInput)?;
            let value = ordinary_map_set_i32_value(value)?;
            let snapshot = snapshot_any_v2_from_map(&source)
                .map_err(ManagedAnyStatus::from)?
                .set_owned(key, value)
                .map_err(|error| {
                    ManagedAnyStatus::from(OrdinaryMapSnapshotRuntimeError::from(error))
                })?;
            // The persistent source root is never modified. Release only its
            // temporary lease before publishing the derived root.
            drop(source);
            new_owned_any_v2_root(snapshot).map_err(ManagedAnyStatus::from)
        })();
        match result {
            Ok(root) => {
                unsafe { *out_root = root };
                ORDINARY_MAP_OWNED_ANY_V2_OK
            }
            Err(status) => ordinary_map_owned_any_v2_set_i32_status(status),
        }
    };

    #[cfg(feature = "ordinary-map-set-result-l3")]
    {
        let mut source_after_size = i32::MIN;
        // SAFETY: `source_after_size` is a writable local output and SetResult
        // retains its source on both its successful and recoverable error paths.
        let source_after_status =
            unsafe { mimi_ordinary_map_owned_any_v2_size(handle, &mut source_after_size) };
        let counts = super::ordinary_map_domain::ordinary_map_domain_live_counts_for_test().ok();
        let root_publish_attempts = ordinary_map_owned_any_v2_root_publish_attempts_for_test();
        ORDINARY_MAP_SET_RESULT_L3_OBSERVATION.with(|cell| {
            let mut observation = cell.get();
            observation.set_calls = observation.set_calls.saturating_add(1);
            observation.source_before_status = source_before_status;
            observation.source_before_size = source_before_size;
            observation.source_after_status = source_after_status;
            observation.source_after_size = source_after_size;
            observation.set_status = status;
            observation.output_root_zero = if !out_root.is_null() && unsafe { *out_root == 0 } {
                1
            } else {
                0
            };
            observation.roots_after_set = counts.map_or(u64::MAX, |counts| counts.roots as u64);
            observation.managed_any_after_set =
                counts.map_or(u64::MAX, |counts| counts.managed_any as u64);
            observation.any_owners_after_set =
                counts.map_or(u64::MAX, |counts| counts.any_owner_tokens as u64);
            observation.root_publish_attempts = root_publish_attempts as u64;
            cell.set(observation);
        });
        ordinary_map_set_get_any_result_i32_l3_record_set(
            (source_before_status, source_before_size),
            (source_after_status, source_after_size),
            status,
            out_root,
        );
    }

    status
}

/// Persistent V2 Map SetResult for the canonical owned Mimi `String` value.
/// The key and source String bytes are borrowed for this call. Compiler-bound
/// String clone/drop glue creates the private ManagedAnyV2 payload; the source
/// root is retained and the replacement root is published only after the
/// complete snapshot is ready. On every failure `out_root` stays zero.
///
/// Statuses 0, 1, and 2 mean success, invalid input, and exact capacity
/// failure. Descriptor mismatch is 18. Callback, root, and domain failures
/// retain their distinct nonrecoverable ManagedAny status codes.
///
/// # Safety
/// `out_root` points to a writable `i64`; positive key/value/type-id lengths
/// refer to readable byte spans; `descriptor_hash` refers to 32 readable
/// bytes; and the callbacks implement the exact canonical `{ptr, i64}` Mimi
/// String TypeDesc passed by the compiler.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_set_string_result(
    handle: i64,
    key: *const u8,
    key_len: i64,
    value_data: *const std::ffi::c_char,
    value_len: i64,
    type_id: *const u8,
    type_id_len: i64,
    descriptor_hash: *const u8,
    clone_fn: Option<super::managed_any_payload_v2::ManagedAnyPayloadV2CloneGlue>,
    drop_fn: Option<super::managed_any_payload_v2::ManagedAnyPayloadV2DropGlue>,
    out_root: *mut i64,
) -> i32 {
    #[cfg(feature = "ordinary-map-set-result-l3")]
    let source_before = ordinary_map_set_get_string_l3_root_size(handle);
    if !out_root.is_null() {
        unsafe { out_root.write(0) };
    }
    let status = if out_root.is_null() {
        ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_INVALID_INPUT
    } else {
        let result = (|| {
            // Match the i32 SetResult ordering: reject a wrong/stale root before
            // parsing borrowed arguments or entering compiler clone callbacks.
            let source = acquire_owned_any_v2_root(handle).map_err(ManagedAnyStatus::from)?;
            let identity =
                ordinary_map_owned_any_v2_string_identity(type_id, type_id_len, descriptor_hash)?;
            let glue = ManagedAnyPayloadV2Glue {
                clone: clone_fn.ok_or(ManagedAnyStatus::InvalidInput)?,
                drop: drop_fn.ok_or(ManagedAnyStatus::InvalidInput)?,
            };
            let value_length =
                usize::try_from(value_len).map_err(|_| ManagedAnyStatus::InvalidInput)?;
            if value_len > i64::MAX - 8
                || value_length > isize::MAX as usize
                || (value_len > 0 && value_data.is_null())
            {
                return Err(ManagedAnyStatus::InvalidInput);
            }
            let value_bytes = if value_length == 0 {
                &[][..]
            } else {
                // SAFETY: the ABI contract supplies `value_len` readable bytes.
                unsafe { std::slice::from_raw_parts(value_data.cast::<u8>(), value_length) }
            };
            std::str::from_utf8(value_bytes).map_err(|_| ManagedAnyStatus::InvalidInput)?;
            let key_bytes =
                unsafe { checked_abi_bytes(key, key_len) }.map_err(ManagedAnyStatus::from)?;
            let key = std::str::from_utf8(key_bytes).map_err(|_| ManagedAnyStatus::InvalidInput)?;

            let borrowed_value = OrdinaryMapOwnedAnyV2StringBox {
                data: value_data.cast_mut(),
                length: value_len,
            };
            let value = OwnedAnyV2::from_borrowed(
                identity,
                glue,
                (&borrowed_value as *const OrdinaryMapOwnedAnyV2StringBox).cast::<u8>(),
            )
            .map_err(ManagedAnyStatus::from)?;
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
                unsafe { out_root.write(root) };
                ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_OK
            }
            Err(status) => ordinary_map_owned_any_v2_set_string_status(status),
        }
    };
    #[cfg(feature = "ordinary-map-set-result-l3")]
    ordinary_map_set_get_string_l3_record_set(handle, source_before, status, out_root);
    status
}

/// Borrow a V2 root and read one entry only when its pinned descriptor is the
/// exact canonical Core i32. This copies directly into caller storage and
/// neither consumes the root nor clones/publishes an Any owner.
///
/// Status 0 writes the value, 1 means missing, 2 means descriptor mismatch,
/// and 3 means capacity. The output is initialized to zero before lookup and
/// remains zero for every nonzero status. Status 4 is an internal failure and
/// is outside the source-language Result; backend adapters must fail closed.
///
/// # Safety
/// `out_value` points to one writable `i32`; for positive `key_len`, `key`
/// points to that many readable UTF-8 bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_get_i32_result(
    handle: i64,
    key: *const u8,
    key_len: i64,
    out_value: *mut i32,
) -> i32 {
    if out_value.is_null() {
        return ORDINARY_MAP_OWNED_ANY_V2_GET_I32_INTERNAL_ERROR;
    }
    unsafe { out_value.write(0) };

    let root = match acquire_owned_any_v2_root(handle) {
        Ok(root) => root,
        Err(_) => return ORDINARY_MAP_OWNED_ANY_V2_GET_I32_INTERNAL_ERROR,
    };
    let key_bytes = match unsafe { checked_abi_bytes(key, key_len) } {
        Ok(bytes) => bytes,
        Err(_) => return ORDINARY_MAP_OWNED_ANY_V2_GET_I32_INTERNAL_ERROR,
    };
    let key = match std::str::from_utf8(key_bytes) {
        Ok(key) => key,
        Err(_) => return ORDINARY_MAP_OWNED_ANY_V2_GET_I32_INTERNAL_ERROR,
    };
    let snapshot = match snapshot_any_v2_from_map(&root) {
        Ok(snapshot) => snapshot,
        Err(_) => return ORDINARY_MAP_OWNED_ANY_V2_GET_I32_INTERNAL_ERROR,
    };
    match snapshot.get_i32_typed(key) {
        Ok(OrdinaryMapLookup::Value(value)) => {
            unsafe { out_value.write(value) };
            ORDINARY_MAP_OWNED_ANY_V2_GET_I32_OK
        }
        Ok(OrdinaryMapLookup::Missing) => ORDINARY_MAP_OWNED_ANY_V2_GET_I32_MISSING,
        Ok(OrdinaryMapLookup::TypeMismatch) => ORDINARY_MAP_OWNED_ANY_V2_GET_I32_TYPE_MISMATCH,
        Err(error) => ordinary_map_owned_any_v2_get_i32_result_error_status(error),
    }
}

/// Borrow a V2 Map root and return a fresh canonical Mimi-owned String copy
/// only when the selected entry has the exact expected String TypeDesc. The
/// temporary value unpack is independently freed with its pinned C-allocation
/// convention; the returned `{data, length}` is registered in Mimi's owned
/// String domain and does not depend on any Map root.
///
/// Statuses 0, 1, 2, and 3 mean success, missing, descriptor mismatch, and
/// final String-copy/publication capacity failure. Every other failure is
/// returned as 4 so backend adapters must fail closed. Both outputs are
/// initialized to null/zero before any fallible work.
///
/// # Safety
/// Positive key/type-id lengths describe readable byte spans, the descriptor
/// hash addresses 32 readable bytes, and each non-null output points to one
/// writable non-overlapping slot.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_get_string_result(
    handle: i64,
    key: *const u8,
    key_len: i64,
    type_id: *const u8,
    type_id_len: i64,
    descriptor_hash: *const u8,
    out_data: *mut *mut std::ffi::c_char,
    out_len: *mut i64,
) -> i32 {
    #[cfg(feature = "ordinary-map-set-result-l3")]
    let temporary_drops_before = OWNED_ANY_V2_GET_STRING_TEMP_DROPS.with(Cell::get);
    if !out_data.is_null() {
        unsafe { out_data.write(std::ptr::null_mut()) };
    }
    if !out_len.is_null() {
        unsafe { out_len.write(0) };
    }
    let status = if out_data.is_null() || out_len.is_null() {
        ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_INTERNAL_ERROR
    } else {
        let result = (|| {
            let root = acquire_owned_any_v2_root(handle).map_err(ManagedAnyStatus::from)?;
            let expected =
                ordinary_map_owned_any_v2_string_identity(type_id, type_id_len, descriptor_hash)?;
            let key_bytes =
                unsafe { checked_abi_bytes(key, key_len) }.map_err(ManagedAnyStatus::from)?;
            let key = std::str::from_utf8(key_bytes).map_err(|_| ManagedAnyStatus::InvalidInput)?;
            let snapshot = snapshot_any_v2_from_map(&root).map_err(ManagedAnyStatus::from)?;
            let Some(value) = snapshot.entries.get(key) else {
                return Ok(None);
            };
            let actual = value.identity().map_err(ManagedAnyStatus::from)?;
            if actual != expected {
                return Err(ManagedAnyStatus::TypeMismatch);
            }

            let mut unpacked = std::mem::MaybeUninit::<OrdinaryMapOwnedAnyV2StringBox>::uninit();
            // SAFETY: the exact String descriptor and native layout were checked
            // above; `unpacked` is correctly aligned writable output storage.
            unsafe {
                any_owner::unpack_managed_v2_owner(
                    value.token,
                    expected,
                    unpacked.as_mut_ptr().cast::<u8>(),
                    expected.size,
                    expected.align,
                )
            }
            .map_err(ManagedAnyStatus::from)?;
            // SAFETY: successful descriptor-pinned unpack initialized the entire
            // String pair. The guard frees the temporary clone on every exit path.
            let unpacked = V2UnpackedStringGuard(unsafe { unpacked.assume_init() });
            let length =
                usize::try_from(unpacked.0.length).map_err(|_| ManagedAnyStatus::InvalidInput)?;
            if length > isize::MAX as usize || (length > 0 && unpacked.0.data.is_null()) {
                return Err(ManagedAnyStatus::InvalidInput);
            }
            let bytes = if length == 0 {
                &[][..]
            } else {
                // SAFETY: the successful pinned String clone produced a readable
                // byte span of the exact explicit length.
                unsafe { std::slice::from_raw_parts(unpacked.0.data.cast::<u8>(), length) }
            };
            std::str::from_utf8(bytes).map_err(|_| ManagedAnyStatus::InvalidInput)?;

            let mut owned_data = std::ptr::null_mut();
            let mut owned_len = 0;
            // SAFETY: source bytes remain alive under `unpacked`; local outputs
            // are writable and non-overlapping. This bridge registers the final
            // canonical Mimi String before exposing either output.
            let copy_status = unsafe {
                copy_ordinary_map_owned_any_v2_string(
                    unpacked.0.data,
                    unpacked.0.length,
                    &mut owned_data,
                    &mut owned_len,
                )
            };
            if copy_status != ManagedAnyStatus::Ok as i32 {
                return Err(if copy_status == ManagedAnyStatus::Capacity as i32 {
                    ManagedAnyStatus::Capacity
                } else {
                    managed_any_status_from_code(copy_status)
                });
            }
            Ok(Some((owned_data, owned_len)))
        })();

        match result {
            Ok(Some((data, length))) => {
                unsafe {
                    out_data.write(data);
                    out_len.write(length);
                }
                ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_OK
            }
            Ok(None) => ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_MISSING,
            Err(ManagedAnyStatus::TypeMismatch) => {
                ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_TYPE_MISMATCH
            }
            Err(ManagedAnyStatus::Capacity) => ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_CAPACITY,
            Err(_) => ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_INTERNAL_ERROR,
        }
    };
    #[cfg(feature = "ordinary-map-set-result-l3")]
    ordinary_map_set_get_string_l3_record_get(status, out_data, out_len, temporary_drops_before);
    status
}

/// Install a failure after one successful root-publication checkpoint. The
/// C driver calls this on its main thread before generated `main`, so New's
/// publication succeeds and Set's publication returns the exact capacity
/// status without crossing thread-local failpoint state.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub extern "C" fn mimi_ordinary_map_set_result_l3_install_capacity_failure() -> i32 {
    mimi_ordinary_map_set_result_l3_install_capacity_failure_after(1)
}

/// Install the root-publication capacity failpoint after an exact number of
/// successful checkpoints. This hook is isolated to the L3 test runtime; the
/// existing no-argument installer remains the Set-failure compatibility ABI.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub extern "C" fn mimi_ordinary_map_set_result_l3_install_capacity_failure_after(
    successful_publications: u64,
) -> i32 {
    let Ok(successful_publications) = usize::try_from(successful_publications) else {
        return 1;
    };
    OWNED_ANY_V2_ROOT_PUBLISH_FAIL_AFTER.with(|value| value.set(Some(successful_publications)));
    OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS.with(|value| value.set(0));
    ORDINARY_MAP_SET_RESULT_L3_OBSERVATION
        .with(|observation| observation.set(OrdinaryMapSetResultL3Observation::EMPTY));
    0
}

/// Copy the SetResult boundary observation and current settled owner counts
/// into a C-owned output record after generated main has returned.
///
/// # Safety
/// `out_observation` must point to one writable C-layout record.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_set_result_l3_read_observation(
    out_observation: *mut OrdinaryMapSetResultL3Observation,
) -> i32 {
    if out_observation.is_null() {
        return 1;
    }
    let Ok(counts) = super::ordinary_map_domain::ordinary_map_domain_live_counts_for_test() else {
        return 2;
    };
    let Some(final_lists) = managed_any_v2_list_count_for_test() else {
        return 3;
    };
    let mut observation = ORDINARY_MAP_SET_RESULT_L3_OBSERVATION.with(Cell::get);
    observation.shutdown_registered =
        super::ordinary_map_domain::mimi_ordinary_map_set_result_l3_shutdown_registered();
    observation.final_roots = counts.roots as u64;
    observation.final_ordinary_map_handles = counts.ordinary_map_handles as u64;
    observation.final_owned_get_handles = counts.owned_get_handles as u64;
    observation.final_runtime_map_handles = counts.runtime_map_handles as u64;
    observation.final_managed_any = counts.managed_any as u64;
    observation.final_owned_strings = counts.owned_strings as u64;
    observation.final_any_owner_tokens = counts.any_owner_tokens as u64;
    observation.final_lists = final_lists as u64;
    unsafe { out_observation.write(observation) };
    0
}

#[cfg(feature = "ordinary-map-set-result-l3")]
fn ordinary_map_set_get_string_l3_root_size(handle: i64) -> (i32, i32) {
    let mut size = i32::MIN;
    // SAFETY: `size` is writable local output and the input is an opaque
    // handle supplied by the same native process.
    let status = unsafe { mimi_ordinary_map_owned_any_v2_size(handle, &mut size) };
    (status, size)
}

#[cfg(feature = "ordinary-map-set-result-l3")]
fn ordinary_map_set_get_string_l3_record_set(
    handle: i64,
    source_before: (i32, i32),
    status: i32,
    out_root: *const i64,
) {
    let source_after = ordinary_map_set_get_string_l3_root_size(handle);
    let counts = super::ordinary_map_domain::ordinary_map_domain_live_counts_for_test().ok();
    let lists = managed_any_v2_list_count_for_test();
    ORDINARY_MAP_SET_GET_STRING_L3_OBSERVATION.with(|cell| {
        let mut observation = cell.get();
        observation.set_calls = observation.set_calls.saturating_add(1);
        observation.set_source_before_status = source_before.0;
        observation.set_source_before_size = source_before.1;
        observation.set_source_after_status = source_after.0;
        observation.set_source_after_size = source_after.1;
        observation.set_status = status;
        observation.set_output_root_zero = if out_root.is_null() {
            -1
        } else if unsafe { *out_root } == 0 {
            1
        } else {
            0
        };
        observation.set_roots_after = counts.map_or(u64::MAX, |counts| counts.roots as u64);
        observation.set_ordinary_map_handles_after =
            counts.map_or(u64::MAX, |counts| counts.ordinary_map_handles as u64);
        observation.set_owned_get_handles_after =
            counts.map_or(u64::MAX, |counts| counts.owned_get_handles as u64);
        observation.set_managed_any_after =
            counts.map_or(u64::MAX, |counts| counts.managed_any as u64);
        observation.set_any_owner_tokens_after =
            counts.map_or(u64::MAX, |counts| counts.any_owner_tokens as u64);
        observation.set_owned_strings_after =
            counts.map_or(u64::MAX, |counts| counts.owned_strings as u64);
        observation.set_runtime_map_handles_after =
            counts.map_or(u64::MAX, |counts| counts.runtime_map_handles as u64);
        observation.set_lists_after = lists.map_or(u64::MAX, |lists| lists as u64);
        observation.set_root_publish_attempts =
            ordinary_map_owned_any_v2_root_publish_attempts_for_test() as u64;
        cell.set(observation);
    });
}

#[cfg(feature = "ordinary-map-set-result-l3")]
fn ordinary_map_set_get_string_l3_record_get(
    status: i32,
    out_data: *const *mut std::ffi::c_char,
    out_len: *const i64,
    temporary_drops_before: usize,
) {
    let counts = super::ordinary_map_domain::ordinary_map_domain_live_counts_for_test().ok();
    let lists = managed_any_v2_list_count_for_test();
    let (registration_attempts, registration_capacity_failures) =
        super::ordinary_map_domain::fresh_string_registration_observation_l3();
    let (outputs_zero, output_length) = if out_data.is_null() || out_len.is_null() {
        (-1, i64::MIN)
    } else {
        // SAFETY: the getter contract requires non-null outputs to address
        // writable pointer/length slots; both are read immediately after it
        // returns on the same thread.
        let data = unsafe { *out_data };
        let length = unsafe { *out_len };
        (if data.is_null() && length == 0 { 1 } else { 0 }, length)
    };
    let temporary_drops = OWNED_ANY_V2_GET_STRING_TEMP_DROPS.with(Cell::get);
    ORDINARY_MAP_SET_GET_STRING_L3_OBSERVATION.with(|cell| {
        let mut observation = cell.get();
        observation.get_calls = observation.get_calls.saturating_add(1);
        observation.get_status = status;
        observation.get_outputs_zero = outputs_zero;
        observation.get_output_length = output_length;
        observation.get_roots_after = counts.map_or(u64::MAX, |counts| counts.roots as u64);
        observation.get_ordinary_map_handles_after =
            counts.map_or(u64::MAX, |counts| counts.ordinary_map_handles as u64);
        observation.get_owned_get_handles_after =
            counts.map_or(u64::MAX, |counts| counts.owned_get_handles as u64);
        observation.get_managed_any_after =
            counts.map_or(u64::MAX, |counts| counts.managed_any as u64);
        observation.get_any_owner_tokens_after =
            counts.map_or(u64::MAX, |counts| counts.any_owner_tokens as u64);
        observation.get_owned_strings_after =
            counts.map_or(u64::MAX, |counts| counts.owned_strings as u64);
        observation.get_runtime_map_handles_after =
            counts.map_or(u64::MAX, |counts| counts.runtime_map_handles as u64);
        observation.get_lists_after = lists.map_or(u64::MAX, |lists| lists as u64);
        observation.get_copy_clone_attempts =
            ordinary_map_owned_any_v2_string_copy_attempts_for_test(
                ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_CLONE_STAGE,
            ) as u64;
        observation.get_copy_publish_attempts =
            ordinary_map_owned_any_v2_string_copy_attempts_for_test(
                ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_PUBLISH_STAGE,
            ) as u64;
        observation.get_registry_registration_attempts = registration_attempts as u64;
        observation.get_registry_capacity_failures = registration_capacity_failures as u64;
        observation.get_temporary_string_drops =
            temporary_drops.saturating_sub(temporary_drops_before) as u64;
        cell.set(observation);
    });
}

/// Reset the dedicated String L3 observer and all of its thread-local
/// failpoints/counters. This ABI is compiled only into the isolated Memcheck
/// runtime, and it does not touch the historical i32 observer.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub extern "C" fn mimi_ordinary_map_set_get_string_l3_reset() -> i32 {
    ORDINARY_MAP_SET_GET_STRING_L3_OBSERVATION
        .with(|observation| observation.set(OrdinaryMapSetGetStringL3Observation::EMPTY));
    OWNED_ANY_V2_ROOT_PUBLISH_FAIL_AFTER.with(|value| value.set(None));
    OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS.with(|value| value.set(0));
    OWNED_ANY_V2_STRING_COPY_FAIL_STAGE.with(|value| value.set(None));
    OWNED_ANY_V2_STRING_COPY_CLONE_ATTEMPTS.with(|value| value.set(0));
    OWNED_ANY_V2_STRING_COPY_PUBLISH_ATTEMPTS.with(|value| value.set(0));
    OWNED_ANY_V2_GET_STRING_TEMP_DROPS.with(|value| value.set(0));
    super::ordinary_map_domain::reset_fresh_string_registration_observation_l3();
    0
}

/// Fail the second root-publication checkpoint in the dedicated String L3
/// child: New succeeds and SetString returns exact Capacity with a zero
/// output, while its source root remains unchanged.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub extern "C" fn mimi_ordinary_map_set_get_string_l3_install_set_capacity_failure() -> i32 {
    let _ = mimi_ordinary_map_set_get_string_l3_reset();
    OWNED_ANY_V2_ROOT_PUBLISH_FAIL_AFTER.with(|value| value.set(Some(1)));
    0
}

/// Fail the fresh owned-String registry's real `Vec::try_reserve` operation
/// after the final String copy has been allocated but before it is exposed.
/// The getter returns exact Capacity, keeps both outputs empty, and frees its
/// unregistered allocation plus unpacked temporary clone.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub extern "C" fn mimi_ordinary_map_set_get_string_l3_install_get_capacity_failure() -> i32 {
    let _ = mimi_ordinary_map_set_get_string_l3_reset();
    super::ordinary_map_domain::install_fresh_string_registration_capacity_failure_l3();
    0
}

/// Read the dedicated String observer and final ownership-domain census.
///
/// # Safety
/// `out_observation` points to one writable C-layout record.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_set_get_string_l3_read_observation(
    out_observation: *mut OrdinaryMapSetGetStringL3Observation,
) -> i32 {
    if out_observation.is_null() {
        return 1;
    }
    let Ok(counts) = super::ordinary_map_domain::ordinary_map_domain_live_counts_for_test() else {
        return 2;
    };
    let Some(final_lists) = managed_any_v2_list_count_for_test() else {
        return 3;
    };
    let mut observation = ORDINARY_MAP_SET_GET_STRING_L3_OBSERVATION.with(Cell::get);
    observation.shutdown_registered =
        super::ordinary_map_domain::mimi_ordinary_map_set_result_l3_shutdown_registered();
    observation.final_roots = counts.roots as u64;
    observation.final_ordinary_map_handles = counts.ordinary_map_handles as u64;
    observation.final_owned_get_handles = counts.owned_get_handles as u64;
    observation.final_runtime_map_handles = counts.runtime_map_handles as u64;
    observation.final_managed_any = counts.managed_any as u64;
    observation.final_owned_strings = counts.owned_strings as u64;
    observation.final_any_owner_tokens = counts.any_owner_tokens as u64;
    observation.final_lists = final_lists as u64;
    unsafe { out_observation.write(observation) };
    0
}

#[cfg(feature = "ordinary-map-set-result-l3")]
fn ordinary_map_set_get_any_result_i32_l3_record_counts(
    boundary: &mut OrdinaryMapSetGetAnyResultI32L3Boundary,
) {
    if let Ok(counts) = super::ordinary_map_domain::ordinary_map_domain_live_counts_for_test() {
        boundary.roots = counts.roots as u64;
        boundary.ordinary_map_handles = counts.ordinary_map_handles as u64;
        boundary.owned_get_handles = counts.owned_get_handles as u64;
        boundary.runtime_map_handles = counts.runtime_map_handles as u64;
        boundary.managed_any = counts.managed_any as u64;
        boundary.owned_strings = counts.owned_strings as u64;
        boundary.any_owner_tokens = counts.any_owner_tokens as u64;
    } else {
        boundary.roots = u64::MAX;
        boundary.ordinary_map_handles = u64::MAX;
        boundary.owned_get_handles = u64::MAX;
        boundary.runtime_map_handles = u64::MAX;
        boundary.managed_any = u64::MAX;
        boundary.owned_strings = u64::MAX;
        boundary.any_owner_tokens = u64::MAX;
    }
    boundary.lists = managed_any_v2_list_count_for_test().map_or(u64::MAX, |count| count as u64);
    boundary.root_publish_attempts =
        ordinary_map_owned_any_v2_root_publication_calls_for_test() as u64;
    boundary.get_publish_attempts =
        ordinary_map_owned_any_v2_get_any_publish_attempts_for_test() as u64;
}

#[cfg(feature = "ordinary-map-set-result-l3")]
fn ordinary_map_set_get_any_result_i32_l3_root_size(handle: i64) -> (i32, i32) {
    let mut size = i32::MIN;
    // SAFETY: `size` is writable local output and `handle` belongs to the
    // synchronous generated Map operation being observed.
    let status = unsafe { mimi_ordinary_map_owned_any_v2_size(handle, &mut size) };
    (status, size)
}

#[cfg(feature = "ordinary-map-set-result-l3")]
fn ordinary_map_set_get_any_result_i32_l3_record_new(status: i32, out_root: *const i64) {
    ORDINARY_MAP_SET_GET_ANY_RESULT_I32_L3_OBSERVATION.with(|cell| {
        let mut observation = cell.get();
        let mut boundary = observation.new;
        boundary.calls = boundary.calls.saturating_add(1);
        boundary.status = status;
        boundary.output_zero = if out_root.is_null() {
            -1
        } else if unsafe { *out_root } == 0 {
            1
        } else {
            0
        };
        ordinary_map_set_get_any_result_i32_l3_record_counts(&mut boundary);
        observation.new = boundary;
        cell.set(observation);
    });
}

#[cfg(feature = "ordinary-map-set-result-l3")]
fn ordinary_map_set_get_any_result_i32_l3_record_set(
    source_before: (i32, i32),
    source_after: (i32, i32),
    status: i32,
    out_root: *const i64,
) {
    ORDINARY_MAP_SET_GET_ANY_RESULT_I32_L3_OBSERVATION.with(|cell| {
        let mut observation = cell.get();
        let mut boundary = observation.set;
        boundary.calls = boundary.calls.saturating_add(1);
        boundary.status = status;
        boundary.output_zero = if out_root.is_null() {
            -1
        } else if unsafe { *out_root } == 0 {
            1
        } else {
            0
        };
        boundary.source_before_status = source_before.0;
        boundary.source_before_size = source_before.1;
        boundary.source_after_status = source_after.0;
        boundary.source_after_size = source_after.1;
        ordinary_map_set_get_any_result_i32_l3_record_counts(&mut boundary);
        observation.set = boundary;
        cell.set(observation);
    });
}

#[cfg(feature = "ordinary-map-set-result-l3")]
fn ordinary_map_set_get_any_result_i32_l3_record_get(
    source_before: (i32, i32),
    source_after: (i32, i32),
    status: i32,
    out_found: *const i32,
    out_token: *const i64,
) {
    ORDINARY_MAP_SET_GET_ANY_RESULT_I32_L3_OBSERVATION.with(|cell| {
        let mut observation = cell.get();
        let mut boundary = observation.get;
        boundary.calls = boundary.calls.saturating_add(1);
        boundary.status = status;
        boundary.source_before_status = source_before.0;
        boundary.source_before_size = source_before.1;
        boundary.source_after_status = source_after.0;
        boundary.source_after_size = source_after.1;
        boundary.output_found = if out_found.is_null() {
            i32::MIN
        } else {
            unsafe { *out_found }
        };
        boundary.output_owner_zero = if out_token.is_null() {
            -1
        } else if unsafe { *out_token } == 0 {
            1
        } else {
            0
        };
        boundary.output_zero = if boundary.output_found == 0 && boundary.output_owner_zero == 1 {
            1
        } else {
            0
        };
        ordinary_map_set_get_any_result_i32_l3_record_counts(&mut boundary);
        observation.get = boundary;
        cell.set(observation);
    });
}

/// Reset only the independent Result-valued GetAny-i32 L3 observer and the
/// publication failpoints/counters used by its native driver.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub extern "C" fn mimi_ordinary_map_set_get_any_result_i32_l3_reset() -> i32 {
    ORDINARY_MAP_SET_GET_ANY_RESULT_I32_L3_OBSERVATION
        .with(|observation| observation.set(OrdinaryMapSetGetAnyResultI32L3Observation::EMPTY));
    OWNED_ANY_V2_ROOT_PUBLISH_FAIL_AFTER.with(|value| value.set(None));
    OWNED_ANY_V2_ROOT_PUBLISH_ATTEMPTS.with(|value| value.set(0));
    OWNED_ANY_V2_ROOT_PUBLICATION_CALLS.with(|value| value.set(0));
    OWNED_ANY_V2_GET_ANY_PUBLISH_FAIL_AFTER.with(|value| value.set(None));
    OWNED_ANY_V2_GET_ANY_PUBLISH_ATTEMPTS.with(|value| value.set(0));
    0
}

/// Fail the GetAny owner-publication boundary after the payload clone has
/// been staged. The raw ABI returns Capacity with zero outputs; the exact
/// fallible native MIR instruction translates only that status to Err(4).
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub extern "C" fn mimi_ordinary_map_set_get_any_result_i32_l3_install_get_capacity_failure() -> i32
{
    let _ = mimi_ordinary_map_set_get_any_result_i32_l3_reset();
    OWNED_ANY_V2_GET_ANY_PUBLISH_FAIL_AFTER.with(|value| value.set(Some(0)));
    0
}

/// Read the independent Result-valued GetAny observer and final ownership
/// census after generated main has returned.
///
/// # Safety
/// `out_observation` points to one writable C-layout record.
#[cfg(feature = "ordinary-map-set-result-l3")]
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_set_get_any_result_i32_l3_read_observation(
    out_observation: *mut OrdinaryMapSetGetAnyResultI32L3Observation,
) -> i32 {
    if out_observation.is_null() {
        return 1;
    }
    let Ok(counts) = super::ordinary_map_domain::ordinary_map_domain_live_counts_for_test() else {
        return 2;
    };
    let Some(final_lists) = managed_any_v2_list_count_for_test() else {
        return 3;
    };
    let mut observation = ORDINARY_MAP_SET_GET_ANY_RESULT_I32_L3_OBSERVATION.with(Cell::get);
    observation.shutdown_registered =
        super::ordinary_map_domain::mimi_ordinary_map_set_result_l3_shutdown_registered();
    observation.final_roots = counts.roots as u64;
    observation.final_ordinary_map_handles = counts.ordinary_map_handles as u64;
    observation.final_owned_get_handles = counts.owned_get_handles as u64;
    observation.final_runtime_map_handles = counts.runtime_map_handles as u64;
    observation.final_managed_any = counts.managed_any as u64;
    observation.final_owned_strings = counts.owned_strings as u64;
    observation.final_any_owner_tokens = counts.any_owner_tokens as u64;
    observation.final_lists = final_lists as u64;
    unsafe { out_observation.write(observation) };
    0
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
/// `found=0` and a fresh canonical i32-zero V2 owner made from the exact Core
/// i32 receipt and runtime-owned memcpy/no-op glue.
///
/// # Safety
/// Outputs are writable and non-overlapping. Type ID and hash spans are
/// readable and key is valid UTF-8. The miss descriptor must be the exact
/// canonical i32 receipt (tag 1, Core TypeDesc identity/hash, i32 layout).
/// The callback arguments are ignored for this canonical scalar; the runtime
/// pins its built-in memcpy/no-op glue before descriptor registration.
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
    #[cfg(feature = "ordinary-map-set-result-l3")]
    let source_before = ordinary_map_set_get_any_result_i32_l3_root_size(handle);
    if !out_found.is_null() {
        unsafe { *out_found = 0 };
    }
    if !out_token.is_null() {
        unsafe { *out_token = 0 };
    }
    if out_found.is_null() || out_token.is_null() {
        let status = map_get_status(ManagedAnyStatus::InvalidInput);
        #[cfg(feature = "ordinary-map-set-result-l3")]
        ordinary_map_set_get_any_result_i32_l3_record_get(
            source_before,
            ordinary_map_set_get_any_result_i32_l3_root_size(handle),
            status,
            out_found,
            out_token,
        );
        return status;
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
        if !super::managed_any_payload_v2::is_canonical_i32_identity(miss) {
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
    let status = match result {
        Ok((found, owner)) => {
            unsafe {
                *out_found = i32::from(found);
                *out_token = owner;
            }
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(status) => map_get_status(status),
    };
    #[cfg(feature = "ordinary-map-set-result-l3")]
    ordinary_map_set_get_any_result_i32_l3_record_get(
        source_before,
        ordinary_map_set_get_any_result_i32_l3_root_size(handle),
        status,
        out_found,
        out_token,
    );
    status
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

struct StagedOwnedStringList(*mut super::MimiList);

impl StagedOwnedStringList {
    fn new() -> Result<Self, ManagedAnyStatus> {
        snapshot_allocation_checkpoint().map_err(|_| ManagedAnyStatus::Capacity)?;
        let list = super::mimi_mir_list_new_string();
        if list.is_null() {
            return Err(ManagedAnyStatus::Capacity);
        }
        Ok(Self(list))
    }

    fn push(&mut self, value: &str) -> Result<(), ManagedAnyStatus> {
        snapshot_allocation_checkpoint().map_err(|_| ManagedAnyStatus::Capacity)?;
        // SAFETY: the guard exclusively owns a valid canonical String List,
        // and the UTF-8 String remains live for the complete copying call.
        if unsafe {
            super::mimi_mir_list_push_string(
                self.0,
                value.as_ptr().cast(),
                i64::try_from(value.len()).map_err(|_| ManagedAnyStatus::Capacity)?,
            )
        } == 0
        {
            return Err(ManagedAnyStatus::Capacity);
        }
        Ok(())
    }

    fn publish(mut self) -> *mut super::MimiList {
        let list = self.0;
        self.0 = std::ptr::null_mut();
        list
    }
}

#[cfg(test)]
std::thread_local! {
    static ORDINARY_MAP_OWNED_ANY_V2_KEYS_LIST_ROLLBACKS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn ordinary_map_owned_any_v2_keys_list_rollbacks_for_test() -> usize {
    ORDINARY_MAP_OWNED_ANY_V2_KEYS_LIST_ROLLBACKS.with(Cell::get)
}

impl Drop for StagedOwnedStringList {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this guard is the unique owner of the canonical list;
            // the runtime drop glue releases every published element box.
            unsafe { super::mimi_mir_list_drop_string(self.0) };
            #[cfg(test)]
            ORDINARY_MAP_OWNED_ANY_V2_KEYS_LIST_ROLLBACKS.with(|count| {
                count.set(count.get().saturating_add(1));
            });
        }
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
        let snapshot = snapshot_any_v2_from_map(&root).map_err(ManagedAnyStatus::from)?;
        snapshot.validate_values_selection(&selection)?;
        let values = snapshot.values().map_err(|error| {
            ManagedAnyStatus::from(OrdinaryMapSnapshotRuntimeError::from(error))
        })?;
        // The sorted Values owners are independent clones now; releasing this
        // lease before publication keeps root access short.
        drop(root);

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

/// Materialize the keys of a live V2 Map as an independently owned canonical
/// `List<String>` sorted by each key's full UTF-8 bytes. The source Map stays
/// unchanged. Any failure leaves `out_list` null and releases every staged
/// key and list allocation before returning a nonzero status.
///
/// # Safety
/// `out_list` points to one writable pointer and `handle` is a live V2 Map
/// root. The output does not overlap any runtime-owned data.
#[no_mangle]
pub unsafe extern "C" fn mimi_ordinary_map_owned_any_v2_keys_list(
    handle: i64,
    out_list: *mut *mut super::MimiList,
) -> i32 {
    if !out_list.is_null() {
        unsafe { *out_list = std::ptr::null_mut() };
    }
    if out_list.is_null() {
        return status_code(ManagedAnyStatus::InvalidInput);
    }
    let result = (|| {
        let root = acquire_owned_any_v2_root(handle).map_err(ManagedAnyStatus::from)?;
        let keys = snapshot_any_v2_from_map(&root)
            .map_err(ManagedAnyStatus::from)?
            .keys()
            .map_err(|error| {
                ManagedAnyStatus::from(OrdinaryMapSnapshotRuntimeError::from(error))
            })?;
        // Keys are now independent Rust-owned copies; release the root lease
        // before the separately owned MimiList is allocated and published.
        drop(root);

        let mut list = StagedOwnedStringList::new()?;
        for key in &keys {
            list.push(key)?;
        }
        snapshot_allocation_checkpoint().map_err(|_| ManagedAnyStatus::Capacity)?;
        Ok::<_, ManagedAnyStatus>(list.publish())
    })();
    match result {
        Ok(list) => {
            unsafe { *out_list = list };
            ORDINARY_MAP_OWNED_ANY_V2_OK
        }
        Err(status) => status_code(status),
    }
}

/// Copy the index-zero String from an exact canonical String List into the
/// ordinary owned-String domain. This helper has no index parameter so it
/// cannot silently become a general List projection.
///
/// # Safety
/// `keys` is null or points to a live canonical owned `List<String>`. Each
/// non-null output points to one writable, non-overlapping value. The list is
/// immutable during the call and remains owned by the caller.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_owned_any_v2_keys_unpack_copy_string(
    keys: *const super::MimiList,
    out_pointer: *mut *mut std::ffi::c_char,
    out_length: *mut i64,
) -> i32 {
    if !out_pointer.is_null() {
        unsafe { *out_pointer = std::ptr::null_mut() };
    }
    if !out_length.is_null() {
        unsafe { *out_length = 0 };
    }
    if keys.is_null() || out_pointer.is_null() || out_length.is_null() {
        return status_code(ManagedAnyStatus::InvalidInput);
    }
    let list = unsafe { &*keys };
    if list.element_kind != super::ListElementKind::String
        || list.string_abi != super::list_string::LIST_STRING_ABI_FAT
        || !list.owns_data
        || list.len <= 0
        || list.data.is_null()
        || (list.has_header && {
            let capacity = super::list_cap(list);
            capacity <= 0 || list.len > capacity
        })
    {
        return status_code(ManagedAnyStatus::InvalidInput);
    }
    let slot = unsafe { *list.data };
    if slot.is_null() {
        return status_code(ManagedAnyStatus::InvalidInput);
    }
    let (source, length) = match unsafe { super::list_string::read_mimi_str(slot) } {
        Ok((source, length)) => (source, length),
        Err(_) => return status_code(ManagedAnyStatus::DescriptorMismatch),
    };
    if length < 0 || length > 64 * 1024 * 1024 || (source.is_null() && length != 0) {
        return status_code(ManagedAnyStatus::InvalidInput);
    }
    unsafe { copy_ordinary_map_owned_any_v2_string(source, length, out_pointer, out_length) }
}

/// Return a persistent root with the selected key removed. The source stays
/// live and unchanged; a missing key still yields a distinct empty-equivalent
/// snapshot root.
///
/// # Safety
/// `key` must span `key_len` readable UTF-8 bytes when non-empty, and when
/// `out_root` is non-null it must point to one writable `i64`.
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

/// Clone an exact V2 ordinary Map root handle into `out_root`.
///
/// # Safety
/// When `out_root` is non-null, it must point to one writable `i64`.
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

/// Read the entry count of an exact V2 ordinary Map root into `out_size`.
///
/// # Safety
/// When `out_size` is non-null, it must point to one writable `i32`.
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

/// Check whether `key` exists in an exact V2 ordinary Map root, writing to `out_contains`.
///
/// # Safety
/// `key` must span `key_len` readable UTF-8 bytes when non-empty, and when
/// `out_contains` is non-null it must point to one writable `i32`.
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
    const V2_TEST_FAILURE_TYPE_ID: &[u8; 35] = b"rt:22222222222222222222222222222222";
    const V2_TEST_FAILURE_HASH: [u8; 32] = [191; 32];
    const V2_TEST_SELECTION_TYPE_ID: &[u8; 35] = b"rt:33333333333333333333333333333333";
    const V2_TEST_SELECTION_HASH: [u8; 32] = [193; 32];
    const V2_TEST_LIFETIME_TYPE_ID: &[u8; 35] = b"rt:44444444444444444444444444444444";
    const V2_TEST_LIFETIME_HASH: [u8; 32] = [197; 32];
    const V2_TEST_FAILPOINT_TYPE_ID: &[u8; 35] = b"rt:55555555555555555555555555555555";
    const V2_TEST_FAILPOINT_HASH: [u8; 32] = [199; 32];
    const V2_TEST_STRING_TYPE_ID: &[u8; 35] = b"rt:66666666666666666666666666666666";
    const V2_TEST_STRING_HASH: [u8; 32] = [211; 32];
    pub(super) const V2_TEST_STRING_CLONE_FAILURE_TYPE_ID: &[u8; 35] =
        b"rt:77777777777777777777777777777777";
    pub(super) const V2_TEST_STRING_CLONE_FAILURE_HASH: [u8; 32] = [223; 32];
    static CANONICAL_I32_HOST_CALLBACK_CALLS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    static V2_VALUES_CLONE_FAIL: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    static V2_VALUES_DROP_CALLS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    static V2_SELECTION_CLONE_FAIL: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    static V2_SELECTION_CLONE_CALLS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    static V2_LIFETIME_DROP_CALLS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    static V2_FAILPOINT_DROP_CALLS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn finished_v2_list_receipt_releases_idle_registry_capacity() {
        let creator = super::super::handle::current_lease_thread_id();
        let mut registry = Vec::with_capacity(4);
        for receipt_id in 1..=2 {
            registry.push(super::RegisteredManagedAnyV2List {
                receipt_id,
                creator,
                domain: super::ManagedAnyV2ListDomain::CanonicalOwnerListV2,
                list: None,
                retirement_pointer: receipt_id as usize,
                lifecycle: super::ManagedAnyV2ListLifecycle::Retiring {
                    claimant: creator,
                    origin: super::ManagedAnyV2ListRetirementOrigin::ExplicitDrop,
                },
            });
        }
        assert!(registry.capacity() >= 2);

        let retained = super::remove_finished_list_receipt(&mut registry, 0);
        assert!(retained.is_none());
        assert_eq!(registry.len(), 1);
        assert!(registry.capacity() >= 1);

        let released = super::remove_finished_list_receipt(&mut registry, 0)
            .expect("the last retired receipt releases the idle registry buffer");
        assert!(registry.is_empty());
        assert_eq!(registry.capacity(), 0);
        assert!(released.capacity() >= 1);
        drop(released);
    }

    unsafe extern "C" fn clone_v2_byte(destination: *mut u8, source: *const u8) -> i32 {
        unsafe { *destination = *source };
        0
    }

    unsafe extern "C" fn drop_v2_byte(_payload: *mut u8) {}

    unsafe extern "C" fn clone_v2_i32(destination: *mut u8, source: *const u8) -> i32 {
        unsafe {
            std::ptr::copy_nonoverlapping(source.cast::<i32>(), destination.cast::<i32>(), 1)
        };
        0
    }

    unsafe extern "C" fn drop_v2_i32(_payload: *mut u8) {}

    unsafe extern "C" fn clone_v2_test_string(destination: *mut u8, source: *const u8) -> i32 {
        let source = unsafe { &*source.cast::<super::OrdinaryMapOwnedAnyV2StringBox>() };
        if source.length < 0 || source.length > isize::MAX as i64 {
            return 91;
        }
        let length = source.length as usize;
        if length > 0 && source.data.is_null() {
            return 92;
        }
        if length == 0 {
            unsafe {
                destination
                    .cast::<super::OrdinaryMapOwnedAnyV2StringBox>()
                    .write(super::OrdinaryMapOwnedAnyV2StringBox {
                        data: std::ptr::null_mut(),
                        length: 0,
                    })
            };
            return 0;
        }
        let Some(allocation_size) = length.checked_add(1) else {
            return 93;
        };
        let data = unsafe { libc::malloc(allocation_size) }.cast::<std::ffi::c_char>();
        if data.is_null() {
            return 1;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(source.data.cast::<u8>(), data.cast::<u8>(), length);
            data.add(length).write(0);
            destination
                .cast::<super::OrdinaryMapOwnedAnyV2StringBox>()
                .write(super::OrdinaryMapOwnedAnyV2StringBox {
                    data,
                    length: source.length,
                });
        }
        0
    }

    unsafe extern "C" fn drop_v2_test_string(payload: *mut u8) {
        let value = unsafe {
            payload
                .cast::<super::OrdinaryMapOwnedAnyV2StringBox>()
                .read()
        };
        if !value.data.is_null() {
            unsafe { libc::free(value.data.cast()) };
        }
    }

    pub(super) unsafe extern "C" fn clone_v2_test_string_failure(
        _destination: *mut u8,
        _source: *const u8,
    ) -> i32 {
        79
    }

    pub(super) unsafe extern "C" fn drop_v2_test_string_failure(_payload: *mut u8) {}

    unsafe extern "C" fn clone_v2_hostile_i32(_destination: *mut u8, _source: *const u8) -> i32 {
        CANONICAL_I32_HOST_CALLBACK_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        91
    }

    unsafe extern "C" fn drop_v2_hostile_i32(_payload: *mut u8) {
        CANONICAL_I32_HOST_CALLBACK_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

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

    unsafe extern "C" fn clone_v2_selection_probe(destination: *mut u8, source: *const u8) -> i32 {
        V2_SELECTION_CLONE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if V2_SELECTION_CLONE_FAIL.load(std::sync::atomic::Ordering::SeqCst) {
            return 91;
        }
        unsafe { *destination = *source };
        0
    }

    unsafe extern "C" fn drop_v2_selection_probe(_payload: *mut u8) {}

    unsafe extern "C" fn clone_v2_lifetime_byte(destination: *mut u8, source: *const u8) -> i32 {
        unsafe { *destination = *source };
        0
    }

    unsafe extern "C" fn drop_v2_lifetime_byte(_payload: *mut u8) {
        V2_LIFETIME_DROP_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    unsafe extern "C" fn clone_v2_failpoint_byte(destination: *mut u8, source: *const u8) -> i32 {
        unsafe { *destination = *source };
        0
    }

    unsafe extern "C" fn drop_v2_failpoint_byte(_payload: *mut u8) {
        V2_FAILPOINT_DROP_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) fn new_v2_map() -> i64 {
        let mut root = 0;
        assert_eq!(
            unsafe { super::mimi_ordinary_map_owned_any_v2_new(&mut root) },
            0
        );
        assert_ne!(root, 0);
        root
    }

    fn test_string_identity() -> ManagedAnyPayloadV2Identity {
        ManagedAnyPayloadV2Identity {
            tag: super::super::managed_any_payload_v2::ManagedAnyPayloadV2Tag::String,
            type_id: super::super::managed_any_payload_v2::ManagedAnyPayloadV2TypeId::parse(
                V2_TEST_STRING_TYPE_ID,
            )
            .expect("valid String test TypeId"),
            descriptor_hash: V2_TEST_STRING_HASH,
            size: std::mem::size_of::<super::OrdinaryMapOwnedAnyV2StringBox>(),
            align: std::mem::align_of::<super::OrdinaryMapOwnedAnyV2StringBox>(),
        }
    }

    pub(super) fn set_test_string(root: i64, key: &[u8], value: &[u8]) -> (i32, i64) {
        let identity = test_string_identity();
        let mut out_root = -1_i64;
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_set_string_result(
                root,
                key.as_ptr(),
                key.len() as i64,
                if value.is_empty() {
                    std::ptr::null()
                } else {
                    value.as_ptr().cast()
                },
                value.len() as i64,
                identity.type_id.as_bytes().as_ptr(),
                identity.type_id.as_bytes().len() as i64,
                identity.descriptor_hash.as_ptr(),
                Some(clone_v2_test_string),
                Some(drop_v2_test_string),
                &mut out_root,
            )
        };
        (status, out_root)
    }

    pub(super) fn get_test_string(root: i64, key: &[u8]) -> (i32, *mut std::ffi::c_char, i64) {
        let identity = test_string_identity();
        let mut out_data = std::ptr::dangling_mut::<std::ffi::c_char>();
        let mut out_len = -1_i64;
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_get_string_result(
                root,
                key.as_ptr(),
                key.len() as i64,
                identity.type_id.as_bytes().as_ptr(),
                identity.type_id.as_bytes().len() as i64,
                identity.descriptor_hash.as_ptr(),
                &mut out_data,
                &mut out_len,
            )
        };
        (status, out_data, out_len)
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

    fn new_v2_i32_owner_with_descriptor(
        value: i32,
        type_id: &[u8; 35],
        descriptor_hash: &[u8; 32],
    ) -> i64 {
        let mut owner = 0;
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_new(
                    1,
                    type_id.as_ptr(),
                    type_id.len() as i64,
                    descriptor_hash.as_ptr(),
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

    fn new_v2_i32_owner(value: i32) -> i64 {
        new_v2_i32_owner_with_descriptor(value, V2_I32_TYPE_ID, &V2_I32_HASH)
    }

    fn new_v2_lifetime_owner(value: u8) -> i64 {
        let mut owner = 0;
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_new(
                    3,
                    V2_TEST_LIFETIME_TYPE_ID.as_ptr(),
                    V2_TEST_LIFETIME_TYPE_ID.len() as i64,
                    V2_TEST_LIFETIME_HASH.as_ptr(),
                    &value,
                    1,
                    1,
                    Some(clone_v2_lifetime_byte),
                    Some(drop_v2_lifetime_byte),
                    &mut owner,
                )
            },
            0
        );
        assert_ne!(owner, 0);
        owner
    }

    fn new_v2_failpoint_owner(value: u8) -> i64 {
        let mut owner = 0;
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_new(
                    3,
                    V2_TEST_FAILPOINT_TYPE_ID.as_ptr(),
                    V2_TEST_FAILPOINT_TYPE_ID.len() as i64,
                    V2_TEST_FAILPOINT_HASH.as_ptr(),
                    &value,
                    1,
                    1,
                    Some(clone_v2_failpoint_byte),
                    Some(drop_v2_failpoint_byte),
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

    fn remove_v2(root: i64, key: &[u8]) -> i64 {
        let mut new_root = 0;
        assert_eq!(
            unsafe {
                super::mimi_ordinary_map_owned_any_v2_remove(
                    root,
                    key.as_ptr(),
                    key.len() as i64,
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

    fn keys_list_v2(root: i64) -> (i32, *mut super::super::MimiList) {
        let mut list = std::ptr::null_mut();
        let status = unsafe { super::mimi_ordinary_map_owned_any_v2_keys_list(root, &mut list) };
        (status, list)
    }

    unsafe fn keys_list_bytes(list: *const super::super::MimiList, index: usize) -> Vec<u8> {
        let slot = unsafe { *(*list).data.add(index) };
        let (payload, length) =
            unsafe { super::super::list_string::read_mimi_str(slot) }.expect("owned String box");
        unsafe { super::super::list_string::slot_bytes(payload, length) }.to_vec()
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

    fn get_v2_with_miss_descriptor(
        root: i64,
        key: &[u8],
        type_id: &[u8; 35],
        descriptor_hash: &[u8; 32],
        clone_fn: Option<super::super::managed_any_payload_v2::ManagedAnyPayloadV2CloneGlue>,
        drop_fn: Option<super::super::managed_any_payload_v2::ManagedAnyPayloadV2DropGlue>,
    ) -> (i32, i32, i64) {
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
                type_id.as_ptr(),
                type_id.len() as i64,
                descriptor_hash.as_ptr(),
                std::mem::size_of::<i32>() as i64,
                std::mem::align_of::<i32>() as i64,
                clone_fn,
                drop_fn,
            )
        };
        (status, found, owner)
    }

    fn get_v2(root: i64, key: &[u8]) -> (i32, i32, i64) {
        get_v2_with_miss_descriptor(
            root,
            key,
            super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID,
            &super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_DESC_HASH,
            Some(clone_v2_i32),
            Some(drop_v2_i32),
        )
    }

    fn get_i32_result_v2(root: i64, key: &[u8]) -> (i32, i32) {
        let mut value = i32::MIN;
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_get_i32_result(
                root,
                key.as_ptr(),
                key.len() as i64,
                &mut value,
            )
        };
        (status, value)
    }

    pub(super) fn size_v2(root: i64) -> (i32, i32) {
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

    pub(super) fn drop_v2_map(root: i64) {
        assert_eq!(super::mimi_ordinary_map_owned_any_v2_drop(root), 0);
    }

    #[test]
    fn ordinary_map_owned_any_v2_set_i32_result_keeps_source_and_publishes_canonical_roots() {
        let source = new_v2_map();
        let mut first = -1_i64;
        assert_eq!(
            unsafe {
                super::mimi_ordinary_map_owned_any_v2_set_i32_result(
                    source,
                    b"count".as_ptr(),
                    5,
                    41,
                    &mut first,
                )
            },
            super::ORDINARY_MAP_OWNED_ANY_V2_OK
        );
        assert_ne!(first, 0);
        assert_eq!(size_v2(source), (0, 0));
        assert_eq!(size_v2(first), (0, 1));

        let (status, found, owner) = get_v2(first, b"count");
        assert_eq!((status, found), (0, 1));
        let identity = super::canonical_i32_identity();
        let mut decoded = std::mem::MaybeUninit::<i32>::uninit();
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_unpack(
                    owner,
                    identity.tag as i32,
                    identity.type_id.as_bytes().as_ptr(),
                    identity.type_id.as_bytes().len() as i64,
                    identity.descriptor_hash.as_ptr(),
                    decoded.as_mut_ptr().cast::<u8>(),
                    identity.size as i64,
                    identity.align as i64,
                )
            },
            0
        );
        assert_eq!(unsafe { decoded.assume_init() }, 41);
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);

        let mut replaced = -1_i64;
        assert_eq!(
            unsafe {
                super::mimi_ordinary_map_owned_any_v2_set_i32_result(
                    first,
                    b"count".as_ptr(),
                    5,
                    -7,
                    &mut replaced,
                )
            },
            super::ORDINARY_MAP_OWNED_ANY_V2_OK
        );
        assert_ne!(replaced, 0);
        assert_eq!(size_v2(first), (0, 1));
        assert_eq!(size_v2(replaced), (0, 1));
        for (root, expected) in [(first, 41), (replaced, -7)] {
            let (status, found, owner) = get_v2(root, b"count");
            assert_eq!((status, found), (0, 1));
            let mut decoded = std::mem::MaybeUninit::<i32>::uninit();
            assert_eq!(
                unsafe {
                    super::mimi_any_payload_v2_unpack(
                        owner,
                        identity.tag as i32,
                        identity.type_id.as_bytes().as_ptr(),
                        identity.type_id.as_bytes().len() as i64,
                        identity.descriptor_hash.as_ptr(),
                        decoded.as_mut_ptr().cast::<u8>(),
                        identity.size as i64,
                        identity.align as i64,
                    )
                },
                0
            );
            assert_eq!(unsafe { decoded.assume_init() }, expected);
            assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
        }

        drop_v2_map(source);
        drop_v2_map(first);
        drop_v2_map(replaced);
    }

    #[test]
    fn ordinary_map_owned_any_v2_get_i32_result_borrows_source_without_owner_publication() {
        let empty = new_v2_map();
        let mut source = 0_i64;
        assert_eq!(
            unsafe {
                super::mimi_ordinary_map_owned_any_v2_set_i32_result(
                    empty,
                    b"answer".as_ptr(),
                    6,
                    41,
                    &mut source,
                )
            },
            super::ORDINARY_MAP_OWNED_ANY_V2_OK
        );
        drop_v2_map(empty);

        let noncanonical_i32 = new_v2_i32_owner(-19);
        let wrong_type = set_v2_owner(
            source,
            b"wrong",
            noncanonical_i32,
            1,
            V2_I32_TYPE_ID,
            &V2_I32_HASH,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(noncanonical_i32), 0);

        let owner_count_before = any_owner::live_count().expect("Any owner baseline");
        let domain_counts_before = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("Map and Any domain baseline");
        assert_eq!(get_i32_result_v2(wrong_type, b"answer"), (0, 41));
        assert_eq!(get_i32_result_v2(wrong_type, b"missing"), (1, 0));
        assert_eq!(get_i32_result_v2(wrong_type, b"wrong"), (2, 0));
        assert_eq!(get_i32_result_v2(-1, b"answer"), (4, 0));
        assert_eq!(
            any_owner::live_count().expect("Any owner count after borrowed reads"),
            owner_count_before,
            "typed GetResult must not clone or publish a temporary Any owner"
        );
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("Map and Any domains after borrowed reads"),
            domain_counts_before,
            "typed GetResult must leave root and payload ownership unchanged"
        );
        assert_eq!(size_v2(source), (0, 1));
        assert_eq!(size_v2(wrong_type), (0, 2));

        assert_eq!(
            super::ordinary_map_owned_any_v2_get_i32_result_error_status(
                super::OrdinaryMapSnapshotError::Capacity
            ),
            super::ORDINARY_MAP_OWNED_ANY_V2_GET_I32_CAPACITY
        );
        assert_eq!(
            super::ordinary_map_owned_any_v2_get_i32_result_error_status(
                super::OrdinaryMapSnapshotError::Owner(any_owner::AnyOwnerError::Capacity)
            ),
            super::ORDINARY_MAP_OWNED_ANY_V2_GET_I32_CAPACITY
        );

        drop_v2_map(source);
        drop_v2_map(wrong_type);
    }

    #[test]
    fn ordinary_map_owned_any_v2_set_i32_result_capacity_is_exact_and_atomic() {
        use crate::runtime::{OwnedAnyV2Root, OwnedAnyV2RootSetError};

        let owner_baseline = any_owner::live_count().expect("owner registry baseline");
        let roots_before = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ordinary-Map domain baseline");
        let source = OwnedAnyV2Root::try_new().expect("production New creates a source root");
        let source_counts = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("source root registration");
        assert_eq!(source_counts.roots, roots_before.roots + 1);
        let mut output = i64::MAX;
        let (status, attempts) =
            super::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
                let status = unsafe {
                    super::mimi_ordinary_map_owned_any_v2_set_i32_result(
                        source.handle(),
                        b"count".as_ptr(),
                        5,
                        9,
                        &mut output,
                    )
                };
                (
                    status,
                    super::ordinary_map_owned_any_v2_root_publish_attempts_for_test(),
                )
            });
        assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_SET_CAPACITY);
        assert_eq!(output, 0, "failed SetResult must not publish a root");
        assert_eq!(attempts, 1);
        assert_eq!(source.size(), Ok(0));
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("counts after raw capacity failure"),
            source_counts,
            "failed SetResult must not publish or leak a root"
        );

        let wrapped =
            super::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
                source.try_set_i32("count", 9).err()
            });
        assert_eq!(wrapped, Some(OwnedAnyV2RootSetError::Capacity));
        assert_eq!(source.size(), Ok(0));
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("counts after wrapped capacity failure"),
            source_counts,
            "the borrowed wrapper source stays registered and no result root leaks"
        );
        assert_eq!(any_owner::live_count(), Ok(owner_baseline));
        assert!(source.try_drop().is_ok());
    }

    #[test]
    fn ordinary_map_owned_any_v2_set_i32_result_keeps_other_errors_nonrecoverable() {
        let source = new_v2_map();
        let mut output = i64::MAX;
        let invalid_utf8 = [0xff_u8];
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_set_i32_result(
                source,
                invalid_utf8.as_ptr(),
                invalid_utf8.len() as i64,
                9,
                &mut output,
            )
        };
        assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_SET_INVALID_INPUT);
        assert_ne!(status, super::ORDINARY_MAP_OWNED_ANY_V2_SET_CAPACITY);
        assert_eq!(output, 0);
        assert_eq!(size_v2(source), (0, 0));

        output = i64::MAX;
        let status = unsafe {
            super::mimi_ordinary_map_owned_any_v2_set_i32_result(
                -1,
                b"count".as_ptr(),
                5,
                9,
                &mut output,
            )
        };
        assert_ne!(status, super::ORDINARY_MAP_OWNED_ANY_V2_SET_CAPACITY);
        assert_eq!(output, 0);
        drop_v2_map(source);
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
    fn v2_same_key_overwrite_and_remove_preserve_roots_and_release_payloads() {
        use std::sync::atomic::Ordering;

        V2_LIFETIME_DROP_CALLS.store(0, Ordering::SeqCst);
        let owner_baseline = any_owner::live_count().expect("owner registry baseline");
        let empty = new_v2_map();
        let original = new_v2_lifetime_owner(0x11);
        let original_root = set_v2_owner(
            empty,
            b"key",
            original,
            3,
            V2_TEST_LIFETIME_TYPE_ID,
            &V2_TEST_LIFETIME_HASH,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(original), 0);
        assert_eq!(V2_LIFETIME_DROP_CALLS.load(Ordering::SeqCst), 1);

        let replacement = new_v2_lifetime_owner(0x22);
        let overwritten_root = set_v2_owner(
            original_root,
            b"key",
            replacement,
            3,
            V2_TEST_LIFETIME_TYPE_ID,
            &V2_TEST_LIFETIME_HASH,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(replacement), 0);
        assert_eq!(V2_LIFETIME_DROP_CALLS.load(Ordering::SeqCst), 2);
        assert_eq!(size_v2(empty), (0, 0));
        assert_eq!(size_v2(original_root), (0, 1));
        assert_eq!(size_v2(overwritten_root), (0, 1));

        let (old_status, old_found, old_owner) = get_v2(original_root, b"key");
        assert_eq!((old_status, old_found), (0, 1));
        let (_, old_identity) = super::managed_v2_token_receipt(old_owner).unwrap();
        assert_eq!(old_identity.descriptor_hash, V2_TEST_LIFETIME_HASH);
        assert_eq!(unpack_v2_byte(old_owner, old_identity), 0x11);
        assert_eq!(super::mimi_any_payload_v2_drop(old_owner), 0);

        let removed_root = remove_v2(overwritten_root, b"key");
        assert_eq!(size_v2(removed_root), (0, 0));
        assert_eq!(contains_v2(removed_root, b"key"), (0, 0));
        assert_eq!(contains_v2(overwritten_root, b"key"), (0, 1));
        let (new_status, new_found, new_owner) = get_v2(overwritten_root, b"key");
        assert_eq!((new_status, new_found), (0, 1));
        let (_, new_identity) = super::managed_v2_token_receipt(new_owner).unwrap();
        assert_eq!(new_identity.descriptor_hash, V2_TEST_LIFETIME_HASH);
        assert_eq!(unpack_v2_byte(new_owner, new_identity), 0x22);
        assert_eq!(super::mimi_any_payload_v2_drop(new_owner), 0);
        assert_eq!(V2_LIFETIME_DROP_CALLS.load(Ordering::SeqCst), 4);

        drop_v2_map(empty);
        drop_v2_map(original_root);
        assert_eq!(V2_LIFETIME_DROP_CALLS.load(Ordering::SeqCst), 5);
        drop_v2_map(overwritten_root);
        drop_v2_map(removed_root);
        assert_eq!(V2_LIFETIME_DROP_CALLS.load(Ordering::SeqCst), 6);
        assert_eq!(any_owner::live_count(), Ok(owner_baseline));
    }

    #[test]
    fn v2_get_values_and_unpack_publication_failpoints_are_atomic() {
        use std::sync::atomic::Ordering;

        V2_FAILPOINT_DROP_CALLS.store(0, Ordering::SeqCst);
        let owner_baseline = any_owner::live_count().expect("owner registry baseline");
        let empty = new_v2_map();
        let source = new_v2_failpoint_owner(0x5a);
        let root = set_v2_owner(
            empty,
            b"key",
            source,
            3,
            V2_TEST_FAILPOINT_TYPE_ID,
            &V2_TEST_FAILPOINT_HASH,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(source), 0);
        let source_owner_count = any_owner::live_count().expect("live source owner");

        super::begin_failure_injection(100);
        let (get_status, get_found, get_owner, get_attempts) =
            super::with_ordinary_map_owned_any_v2_get_any_publish_failure_for_test(0, || {
                let (status, found, owner) = get_v2(root, b"key");
                (
                    status,
                    found,
                    owner,
                    super::ordinary_map_owned_any_v2_get_any_publish_attempts_for_test(),
                )
            });
        let get_staged = super::end_failure_injection();
        assert_eq!(get_status, ManagedAnyStatus::Capacity as i32);
        assert_eq!(get_found, 0);
        assert_eq!(get_owner, 0);
        assert_eq!(get_attempts, 1);
        assert_eq!(get_staged.len(), 1);
        for token in get_staged {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken),
                "failed Get publication leaked its staged owner"
            );
        }
        assert_eq!(any_owner::live_count(), Ok(source_owner_count));
        assert_eq!(V2_FAILPOINT_DROP_CALLS.load(Ordering::SeqCst), 2);
        assert_eq!(contains_v2(root, b"key"), (0, 1));

        let (selection_bytes, selection_hash) =
            selection(3, V2_TEST_FAILPOINT_TYPE_ID, &V2_TEST_FAILPOINT_HASH);
        super::begin_failure_injection(100);
        let (list_status, list, list_attempts) =
            super::with_ordinary_map_owned_any_v2_values_publish_failure_for_test(0, || {
                let (status, list) = values_list_v2(root, &selection_bytes, &selection_hash);
                (
                    status,
                    list,
                    super::ordinary_map_owned_any_v2_values_publish_attempts_for_test(),
                )
            });
        let list_staged = super::end_failure_injection();
        assert_eq!(list_status, ManagedAnyStatus::Capacity as i32);
        assert!(list.is_null());
        assert_eq!(list_attempts, 1);
        assert_eq!(list_staged.len(), 1);
        for token in list_staged {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken),
                "failed Values-list publication leaked its staged owner"
            );
        }
        assert_eq!(any_owner::live_count(), Ok(source_owner_count));
        assert_eq!(V2_FAILPOINT_DROP_CALLS.load(Ordering::SeqCst), 3);
        assert_eq!(contains_v2(root, b"key"), (0, 1));

        super::begin_failure_injection(100);
        let (values_status, values_handle, values_attempts) =
            super::with_ordinary_map_owned_any_v2_values_handle_publish_failure_for_test(0, || {
                let (status, handle) = values_v2(root);
                (
                    status,
                    handle,
                    super::ordinary_map_owned_any_v2_values_handle_publish_attempts_for_test(),
                )
            });
        let values_staged = super::end_failure_injection();
        assert_eq!(values_status, ManagedAnyStatus::Capacity as i32);
        assert_eq!(values_handle, 0);
        assert_eq!(values_attempts, 1);
        assert_eq!(values_staged.len(), 1);
        for token in values_staged {
            assert_eq!(
                any_owner::validate(token),
                Err(any_owner::AnyOwnerError::StaleToken),
                "failed opaque Values-handle publication leaked its staged owner"
            );
        }
        assert_eq!(any_owner::live_count(), Ok(source_owner_count));
        assert_eq!(V2_FAILPOINT_DROP_CALLS.load(Ordering::SeqCst), 4);
        assert_eq!(contains_v2(root, b"key"), (0, 1));

        let (get_status, found, owner) = get_v2(root, b"key");
        assert_eq!((get_status, found), (0, 1));
        let (_, identity) = super::managed_v2_token_receipt(owner).unwrap();
        assert_eq!(identity.descriptor_hash, V2_TEST_FAILPOINT_HASH);
        let live_before_unpack = any_owner::live_count().expect("source and Get owners");
        let mut output = 0xa5u8;
        let (unpack_status, unpack_attempts) =
            super::with_ordinary_map_owned_any_v2_unpack_failure_for_test(0, || {
                let status = unsafe {
                    super::mimi_any_payload_v2_unpack(
                        owner,
                        identity.tag as i32,
                        identity.type_id.as_bytes().as_ptr(),
                        identity.type_id.as_bytes().len() as i64,
                        identity.descriptor_hash.as_ptr(),
                        &mut output,
                        identity.size as i64,
                        identity.align as i64,
                    )
                };
                (
                    status,
                    super::ordinary_map_owned_any_v2_unpack_attempts_for_test(),
                )
            });
        assert_eq!(unpack_status, ManagedAnyStatus::Capacity as i32);
        assert_eq!(unpack_attempts, 1);
        assert_eq!(output, 0xa5, "failed unpack must leave output untouched");
        assert_eq!(
            super::managed_v2_token_receipt(owner).unwrap().1,
            identity,
            "failed unpack must leave its source owner usable"
        );
        assert_eq!(any_owner::live_count(), Ok(live_before_unpack));
        assert_eq!(contains_v2(root, b"key"), (0, 1));
        assert_eq!(unpack_v2_byte(owner, identity), 0x5a);
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
        assert_eq!(any_owner::live_count(), Ok(source_owner_count));

        drop_v2_map(empty);
        drop_v2_map(root);
        assert_eq!(V2_FAILPOINT_DROP_CALLS.load(Ordering::SeqCst), 6);
        assert_eq!(any_owner::live_count(), Ok(owner_baseline));
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
                    super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.as_ptr(),
                    super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.len() as i64,
                    super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_DESC_HASH.as_ptr(),
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
        let source = new_v2_i32_owner_with_descriptor(
            0,
            super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID,
            &super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_DESC_HASH,
        );
        let root = set_v2_owner(
            empty,
            b"zero",
            source,
            1,
            super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID,
            &super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_DESC_HASH,
        );
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
                        super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.as_ptr(),
                        super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.len() as i64,
                        super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_DESC_HASH.as_ptr(),
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
    fn v2_get_miss_requires_exact_core_i32_identity_and_canonicalizes_host_glue() {
        use super::super::managed_any_payload_v2::{
            CANONICAL_I32_TYPE_DESC_HASH, CANONICAL_I32_TYPE_ID,
        };

        CANONICAL_I32_HOST_CALLBACK_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
        let root = new_v2_map();

        let (bad_id_status, bad_id_found, bad_id_owner) = get_v2_with_miss_descriptor(
            root,
            b"missing",
            V2_I32_TYPE_ID,
            &CANONICAL_I32_TYPE_DESC_HASH,
            Some(clone_v2_hostile_i32),
            Some(drop_v2_hostile_i32),
        );
        assert_eq!(
            (bad_id_status, bad_id_found, bad_id_owner),
            (ManagedAnyStatus::TypeMismatch as i32, 0, 0),
            "outputs stay reset when a wrong TypeDesc identity is supplied"
        );

        let mut bad_hash = CANONICAL_I32_TYPE_DESC_HASH;
        bad_hash[0] ^= 1;
        let (bad_hash_status, bad_hash_found, bad_hash_owner) = get_v2_with_miss_descriptor(
            root,
            b"missing",
            CANONICAL_I32_TYPE_ID,
            &bad_hash,
            Some(clone_v2_hostile_i32),
            Some(drop_v2_hostile_i32),
        );
        assert_eq!(
            (bad_hash_status, bad_hash_found, bad_hash_owner),
            (ManagedAnyStatus::TypeMismatch as i32, 0, 0),
            "outputs stay reset when a wrong TypeDesc hash is supplied"
        );

        // The public ABI must replace these host pointers before pinning the
        // descriptor. Both Clone and Drop would be observable if arbitrary
        // callbacks entered the table.
        let value = 29_i32;
        let mut source_owner = -1;
        let create_status = unsafe {
            super::mimi_any_payload_v2_new(
                1,
                CANONICAL_I32_TYPE_ID.as_ptr(),
                CANONICAL_I32_TYPE_ID.len() as i64,
                CANONICAL_I32_TYPE_DESC_HASH.as_ptr(),
                (&value as *const i32).cast::<u8>(),
                std::mem::size_of::<i32>() as i64,
                std::mem::align_of::<i32>() as i64,
                Some(clone_v2_hostile_i32),
                Some(drop_v2_hostile_i32),
                &mut source_owner,
            )
        };
        assert_eq!(create_status, 0);
        assert_ne!(source_owner, 0);
        assert_eq!(super::mimi_any_payload_v2_drop(source_owner), 0);

        let (status, found, zero_owner) = get_v2_with_miss_descriptor(
            root,
            b"missing",
            CANONICAL_I32_TYPE_ID,
            &CANONICAL_I32_TYPE_DESC_HASH,
            Some(clone_v2_hostile_i32),
            Some(drop_v2_hostile_i32),
        );
        assert_eq!((status, found), (super::ORDINARY_MAP_OWNED_ANY_V2_OK, 0));
        assert_ne!(zero_owner, 0);
        let (_, identity) = super::managed_v2_token_receipt(zero_owner).unwrap();
        assert_eq!(identity.tag as i32, 1);
        assert_eq!(identity.type_id.as_bytes(), CANONICAL_I32_TYPE_ID);
        assert_eq!(identity.descriptor_hash, CANONICAL_I32_TYPE_DESC_HASH);
        assert_eq!(identity.size, std::mem::size_of::<i32>());
        assert_eq!(identity.align, std::mem::align_of::<i32>());

        let mut zero = std::mem::MaybeUninit::<i32>::uninit();
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_unpack(
                    zero_owner,
                    1,
                    CANONICAL_I32_TYPE_ID.as_ptr(),
                    CANONICAL_I32_TYPE_ID.len() as i64,
                    CANONICAL_I32_TYPE_DESC_HASH.as_ptr(),
                    zero.as_mut_ptr().cast::<u8>(),
                    std::mem::size_of::<i32>() as i64,
                    std::mem::align_of::<i32>() as i64,
                )
            },
            0
        );
        assert_eq!(unsafe { zero.assume_init() }, 0);
        assert_eq!(super::mimi_any_payload_v2_drop(zero_owner), 0);
        assert_eq!(
            CANONICAL_I32_HOST_CALLBACK_CALLS.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "canonical i32 glue never calls host-supplied functions"
        );
        drop_v2_map(root);
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
        let (failed_empty_clone, empty_attempts, empty_triggered) =
            super::with_ordinary_map_owned_get_allocation_failure_for_test(0, || {
                let clone = unsafe { super::super::mimi_mir_list_clone_managed_any_v2(empty_list) };
                (
                    clone.is_null(),
                    super::ordinary_map_owned_get_allocation_attempts_for_test(),
                    super::ordinary_map_owned_get_allocation_failure_triggered_for_test(),
                )
            });
        assert!(
            failed_empty_clone,
            "empty Values Clone has a fallible publication stage"
        );
        assert_eq!(empty_attempts, 1);
        assert!(empty_triggered);
        assert!(super::managed_any_v2_list_registered_for_test(empty_list));
        assert_eq!(
            unsafe { super::super::mimi_mir_list_len_managed_any_v2(empty_list) },
            0,
            "a failed empty Clone leaves its source list live and unchanged"
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
    fn values_list_rejects_unselected_payload_before_clone_glue() {
        use std::sync::atomic::Ordering;

        V2_SELECTION_CLONE_FAIL.store(false, Ordering::SeqCst);
        V2_SELECTION_CLONE_CALLS.store(0, Ordering::SeqCst);
        let payload = 0x5au8;
        let mut source = 0;
        assert_eq!(
            unsafe {
                super::mimi_any_payload_v2_new(
                    3,
                    V2_TEST_SELECTION_TYPE_ID.as_ptr(),
                    V2_TEST_SELECTION_TYPE_ID.len() as i64,
                    V2_TEST_SELECTION_HASH.as_ptr(),
                    &payload,
                    1,
                    1,
                    Some(clone_v2_selection_probe),
                    Some(drop_v2_selection_probe),
                    &mut source,
                )
            },
            0
        );
        assert_eq!(
            V2_SELECTION_CLONE_CALLS.load(Ordering::SeqCst),
            1,
            "creating the Any payload invokes its Clone glue once"
        );
        V2_SELECTION_CLONE_CALLS.store(0, Ordering::SeqCst);

        let empty = new_v2_map();
        let root = set_v2_owner(
            empty,
            b"selected-later",
            source,
            3,
            V2_TEST_SELECTION_TYPE_ID,
            &V2_TEST_SELECTION_HASH,
        );
        assert_eq!(
            V2_SELECTION_CLONE_CALLS.load(Ordering::SeqCst),
            1,
            "Set clones the stored Any payload once"
        );
        assert_eq!(super::mimi_any_payload_v2_drop(source), 0);
        V2_SELECTION_CLONE_CALLS.store(0, Ordering::SeqCst);

        let (wrong_selection, wrong_hash) = selection(5, V2_AGGREGATE_TYPE_ID, &[194; 32]);
        V2_SELECTION_CLONE_FAIL.store(true, Ordering::SeqCst);
        let (wrong_status, wrong_list) = values_list_v2(root, &wrong_selection, &wrong_hash);
        assert_eq!(wrong_status, ManagedAnyStatus::DescriptorMismatch as i32);
        assert!(wrong_list.is_null());
        assert_eq!(
            V2_SELECTION_CLONE_CALLS.load(Ordering::SeqCst),
            0,
            "an unselected payload must be rejected before user-provided Clone glue"
        );

        // The matching selection reaches Clone glue and reports its failure,
        // proving the mismatch above was rejected before this fallible stage.
        let (selected, selected_hash) =
            selection(3, V2_TEST_SELECTION_TYPE_ID, &V2_TEST_SELECTION_HASH);
        let (clone_status, failed_clone) = values_list_v2(root, &selected, &selected_hash);
        assert_eq!(clone_status, ManagedAnyStatus::CloneFailed as i32);
        assert!(failed_clone.is_null());
        assert_eq!(V2_SELECTION_CLONE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(size_v2(root), (0, 1));

        V2_SELECTION_CLONE_FAIL.store(false, Ordering::SeqCst);
        let unselected_hash = [195; 32];
        let unselected = new_v2_bool_owner(1, &unselected_hash);
        let mixed_root = set_v2_owner(
            root,
            b"z-unselected",
            unselected,
            3,
            V2_BOOL_TYPE_ID,
            &unselected_hash,
        );
        assert_eq!(super::mimi_any_payload_v2_drop(unselected), 0);
        assert_eq!(size_v2(root), (0, 1));
        assert_eq!(size_v2(mixed_root), (0, 2));

        // `selected-later` sorts before `z-unselected`. A per-entry
        // validate-then-clone implementation would call the failing Clone
        // glue for the first entry before discovering the second descriptor.
        V2_SELECTION_CLONE_CALLS.store(0, Ordering::SeqCst);
        V2_SELECTION_CLONE_FAIL.store(true, Ordering::SeqCst);
        let (mixed_status, mixed_list) = values_list_v2(mixed_root, &selected, &selected_hash);
        assert_eq!(mixed_status, ManagedAnyStatus::DescriptorMismatch as i32);
        assert!(mixed_list.is_null());
        assert_eq!(
            V2_SELECTION_CLONE_CALLS.load(Ordering::SeqCst),
            0,
            "all descriptors must be preflighted before cloning any selected value"
        );

        V2_SELECTION_CLONE_FAIL.store(false, Ordering::SeqCst);
        let (selected_status, selected_found, selected_copy) =
            get_v2(mixed_root, b"selected-later");
        assert_eq!((selected_status, selected_found), (0, 1));
        let selected_identity = super::managed_v2_token_receipt(selected_copy).unwrap().1;
        assert_eq!(unpack_v2_byte(selected_copy, selected_identity), payload);
        assert_eq!(super::mimi_any_payload_v2_drop(selected_copy), 0);
        let (unselected_status, unselected_found, unselected_copy) =
            get_v2(mixed_root, b"z-unselected");
        assert_eq!((unselected_status, unselected_found), (0, 1));
        let unselected_identity = super::managed_v2_token_receipt(unselected_copy).unwrap().1;
        assert_eq!(unpack_v2_byte(unselected_copy, unselected_identity), 1);
        assert_eq!(super::mimi_any_payload_v2_drop(unselected_copy), 0);

        drop_v2_map(empty);
        drop_v2_map(root);
        drop_v2_map(mixed_root);
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
        assert!(
            staged.is_empty(),
            "selector rejection must precede staging any Values owner"
        );

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

        // The shared Clone failpoint presents one output-list stage followed
        // by one V2 owner clone stage per selected element. Every failed
        // prefix must be reclaimed without consuming or mutating source.
        for failure_stage in 0..=3 {
            super::begin_failure_injection(100);
            let (succeeded, attempts, triggered) =
                super::with_ordinary_map_owned_get_allocation_failure_for_test(
                    failure_stage,
                    || {
                        let clone = unsafe {
                            super::super::mimi_mir_list_clone_managed_any_v2(source_list)
                        };
                        let succeeded = !clone.is_null();
                        if succeeded {
                            unsafe { super::super::mimi_mir_list_drop_managed_any_v2(clone) };
                        }
                        (
                            succeeded,
                            super::ordinary_map_owned_get_allocation_attempts_for_test(),
                            super::ordinary_map_owned_get_allocation_failure_triggered_for_test(),
                        )
                    },
                );
            let staged_clone_tokens = super::end_failure_injection();
            if failure_stage < 3 {
                assert!(!succeeded, "injected Clone stage {failure_stage}");
                assert!(triggered, "injected Clone stage {failure_stage}");
                assert_eq!(attempts, failure_stage + 1);
            } else {
                assert!(succeeded, "all fallible Clone stages admitted");
                assert!(!triggered);
                assert_eq!(attempts, 3);
            }
            for token in staged_clone_tokens {
                assert_eq!(
                    any_owner::validate(token),
                    Err(any_owner::AnyOwnerError::StaleToken),
                    "failed or settled Values Clone retained a temporary payload owner"
                );
            }
            assert_eq!(
                unsafe { super::super::mimi_mir_list_len_managed_any_v2(source_list) },
                2,
                "a failed Values Clone preserves the source list"
            );
            assert_eq!(size_v2(with_second), (0, 2));
        }

        let i32_owner = new_v2_i32_owner(73);
        let i32_only_list = super::super::mimi_mir_list_new_managed_any_v2();
        assert!(!i32_only_list.is_null());
        assert_eq!(
            unsafe { super::super::mimi_mir_list_push_managed_any_v2(i32_only_list, i32_owner) },
            1
        );
        for failure_stage in 0..=2 {
            super::begin_failure_injection(100);
            let (succeeded, attempts, triggered) =
                super::with_ordinary_map_owned_get_allocation_failure_for_test(
                    failure_stage,
                    || {
                        let clone = unsafe {
                            super::super::mimi_mir_list_clone_managed_any_v2(i32_only_list)
                        };
                        let succeeded = !clone.is_null();
                        if succeeded {
                            unsafe { super::super::mimi_mir_list_drop_managed_any_v2(clone) };
                        }
                        (
                            succeeded,
                            super::ordinary_map_owned_get_allocation_attempts_for_test(),
                            super::ordinary_map_owned_get_allocation_failure_triggered_for_test(),
                        )
                    },
                );
            let staged_clone_tokens = super::end_failure_injection();
            if failure_stage < 2 {
                assert!(!succeeded, "i32-only injected Clone stage {failure_stage}");
                assert!(triggered);
                assert_eq!(attempts, failure_stage + 1);
            } else {
                assert!(succeeded, "i32-only Clone stages all admitted");
                assert!(!triggered);
                assert_eq!(attempts, 2);
            }
            for token in staged_clone_tokens {
                assert_eq!(
                    any_owner::validate(token),
                    Err(any_owner::AnyOwnerError::StaleToken),
                    "i32-only Values Clone retained a temporary owner"
                );
            }
            assert!(super::managed_v2_token_receipt(i32_owner).is_ok());
            assert_eq!(
                unsafe { super::super::mimi_mir_list_len_managed_any_v2(i32_only_list) },
                1,
                "i32-only failure leaves the source list unchanged"
            );
        }
        unsafe { super::super::mimi_mir_list_drop_managed_any_v2(i32_only_list) };

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
    fn lifecycle_v1_json_formats_empty_root_and_returns_mimi_owned_c_string() {
        let root = super::new_empty_root().expect("empty lifecycle root");
        assert_eq!(
            super::ordinary_map_owner_lifecycle_i32_string_v1_to_json(root),
            Ok("{}".to_owned())
        );

        let detached = super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_to_json(root);
        assert!(!detached.is_null());
        super::drop_root(root).expect("drop source root while detached JSON remains live");
        // SAFETY: the serializer returned a fresh NUL-terminated Mimi string.
        let rendered = unsafe { std::ffi::CStr::from_ptr(detached) }
            .to_str()
            .expect("JSON output is UTF-8");
        assert_eq!(rendered, "{}");
        // SAFETY: this pointer is the caller-owned allocation documented by
        // the serializer FFI.
        unsafe { super::super::mimi_string_free(detached) };
        assert_eq!(
            super::super::handle::mimi_handle_last_error(),
            super::super::handle::HANDLE_OK
        );
    }

    #[test]
    fn lifecycle_v1_json_mixes_typed_entries_and_sorts_full_key_bytes() {
        let empty = super::new_empty_root().expect("empty root");
        let first = super::set_i32_root(empty, "z", -9).expect("integer root");
        let second = super::set_string_root(first, "雪", "value").expect("string root");
        let third = super::set_i32_root(second, "a\0", 2).expect("NUL-key root");
        let root = super::set_string_root(third, "a", "first").expect("mixed root");

        assert_eq!(
            super::ordinary_map_owner_lifecycle_i32_string_v1_to_json(root),
            Ok("{\"a\":\"first\",\"a\\u0000\":2,\"z\":-9,\"雪\":\"value\"}".to_owned())
        );
        assert_eq!(super::size_root(root), Ok(4));

        for old in [empty, first, second, third] {
            super::drop_root(old).expect("drop prior persistent root");
        }
        super::drop_root(root).expect("drop latest root");
    }

    #[test]
    fn lifecycle_v1_json_escapes_embedded_nul_and_json_control_characters() {
        let value = "quote\" slash\\ nul\0 backspace\u{08} formfeed\u{0c} line\n tab\t cr\r unit\u{1f} c1\u{0085} unicode雪🧭";
        let empty = super::new_empty_root().expect("empty root");
        let root = super::set_string_root(empty, "key\0雪", value).expect("escaped string root");

        let actual = super::ordinary_map_owner_lifecycle_i32_string_v1_to_json(root)
            .expect("serialized root");
        assert_eq!(actual, serde_json::json!({ "key\0雪": value }).to_string());
        assert!(serde_json::from_str::<serde_json::Value>(&actual).is_ok());
        let legacy_any_json = format!(
            "{{{}:{}}}",
            super::super::json_escape_string("key\0雪"),
            super::super::json_escape_string(value)
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&actual).unwrap(),
            serde_json::from_str::<serde_json::Value>(&legacy_any_json).unwrap()
        );
        super::drop_root(empty).expect("drop empty root");
        super::drop_root(root).expect("drop escaped root");
    }

    #[test]
    fn lifecycle_v1_json_rejects_unavailable_stale_and_wrong_profile_handles() {
        assert!(matches!(
            super::ordinary_map_owner_lifecycle_i32_string_v1_to_json(0),
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(_))
        ));
        let stale = super::new_empty_root().expect("stale root");
        super::drop_root(stale).expect("retire stale root");
        assert!(matches!(
            super::ordinary_map_owner_lifecycle_i32_string_v1_to_json(stale),
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(_))
        ));

        let wrong_profile =
            super::new_owned_get_root(OrdinaryMapSnapshot::new()).expect("owned-Get root");
        assert!(matches!(
            super::ordinary_map_owner_lifecycle_i32_string_v1_to_json(wrong_profile),
            Err(super::OrdinaryMapSnapshotRuntimeError::Handle(_))
        ));
        super::drop_owned_get_root(wrong_profile).expect("drop owned-Get root");

        let output = super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_to_json(0);
        assert!(output.is_null());
        assert_eq!(
            super::super::handle::mimi_handle_last_error(),
            super::super::handle::HANDLE_ERR_INVALID
        );
    }

    #[test]
    fn lifecycle_v1_json_capacity_failpoints_leave_root_live_and_publish_nothing() {
        let empty = super::new_empty_root().expect("empty root");
        let with_i32 = super::set_i32_root(empty, "a", 17).expect("integer root");
        let root =
            super::set_string_root(with_i32, "b", "still live").expect("mixed lifecycle root");
        let scratch_baseline = super::ordinary_map_json_scratch_scopes_for_test();
        assert_eq!(scratch_baseline, 0);

        // This root exercises every fallible observer allocation point:
        // sorted-entry storage; JSON punctuation/key/value reservations;
        // String owner copying; and the final detached C buffer. The final
        // checkpoint is deliberately inside the FFI bridge, after rendering
        // succeeded but before any pointer can be published.
        const JSON_OBSERVER_ALLOCATION_CHECKPOINTS: usize = 12;
        for fail_after in 0..JSON_OBSERVER_ALLOCATION_CHECKPOINTS {
            super::begin_failure_injection(fail_after);
            let output = super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_to_json(root);
            let runtime_error = super::super::handle::mimi_handle_last_error();
            let staged_owners = super::end_failure_injection();

            assert!(
                output.is_null(),
                "checkpoint {fail_after} must not publish a partial JSON buffer"
            );
            assert_eq!(
                runtime_error,
                super::super::handle::HANDLE_ERR_INVALID,
                "capacity failures must reach the established native E0800 null guard"
            );
            assert!(
                staged_owners.is_empty(),
                "the observer must not create or retain Any owners at checkpoint {fail_after}"
            );
            assert_eq!(
                super::ordinary_map_json_scratch_scopes_for_test(),
                scratch_baseline,
                "serializer scratch scope leaked at checkpoint {fail_after}"
            );
            assert_eq!(super::size_root(root), Ok(2), "source root was retired");
            let lease = super::acquire_root(root).expect("source root remains available");
            let snapshot = super::snapshot_from_map(&lease).expect("source snapshot remains live");
            assert_eq!(
                snapshot.entries.get("a").expect("integer owner").read_i32(),
                Ok(17)
            );
            assert_eq!(
                snapshot
                    .entries
                    .get("b")
                    .expect("string owner")
                    .copy_string(),
                Ok("still live".to_owned())
            );
        }

        // All twelve injected allocation boundaries fail above. Advancing the
        // oracle by exactly that many successful checkpoints must return one
        // complete, caller-owned output and leave no serializer scratch live.
        super::begin_failure_injection(JSON_OBSERVER_ALLOCATION_CHECKPOINTS);
        let output = super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_to_json(root);
        let runtime_error = super::super::handle::mimi_handle_last_error();
        let staged_owners = super::end_failure_injection();
        assert!(
            !output.is_null(),
            "successful observation must publish a buffer"
        );
        assert_eq!(runtime_error, super::super::handle::HANDLE_OK);
        assert!(staged_owners.is_empty());
        assert_eq!(
            super::ordinary_map_json_scratch_scopes_for_test(),
            scratch_baseline
        );
        // SAFETY: the runtime returned a complete caller-owned C string.
        let rendered = unsafe { std::ffi::CStr::from_ptr(output) }
            .to_str()
            .expect("JSON is UTF-8");
        assert_eq!(rendered, r#"{"a":17,"b":"still live"}"#);
        // SAFETY: free the exact Mimi allocation returned by this FFI call.
        unsafe { super::super::mimi_string_free(output) };

        for prior in [empty, with_i32, root] {
            super::drop_root(prior).expect("drop lifecycle root");
        }
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
    fn lifecycle_size_status_out_separates_success_overflow_and_invalid_root() {
        let root = super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new();
        let mut size = -9;
        let success = unsafe {
            super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size_status(root, &mut size)
        };
        assert_eq!(success, 0);
        assert_eq!(size, 0);

        // A synthetic SizeOverflow exercises the otherwise impractical
        // >i32::MAX entry-count case. Overflow classification precedes any
        // output-pointer check or write.
        let overflow = unsafe {
            super::ordinary_map_owner_lifecycle_size_status_from_result(
                Err(super::OrdinaryMapSnapshotRuntimeError::SizeOverflow),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(overflow, 1);

        // Other errors stay distinct from overflow; the out slot is left
        // untouched when the root is invalid.
        let invalid_root = unsafe {
            super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size_status(0, &mut size)
        };
        assert_eq!(invalid_root, -1);
        assert_eq!(size, 0);

        // A null output is only rejected after a successful size result.
        let successful_size_with_null_output = unsafe {
            super::ordinary_map_owner_lifecycle_size_status_from_result(Ok(1), std::ptr::null_mut())
        };
        assert_eq!(successful_size_with_null_output, -1);

        super::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop(root);
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
    fn owned_get_result_v1_new_and_set_publish_typed_roots() {
        let baseline = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("owned-Get domain baseline");

        let mut empty = -1;
        assert_eq!(
            unsafe { super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new_result(&mut empty) },
            super::ORDINARY_MAP_OWNED_GET_NEW_RESULT_OK
        );
        assert_ne!(empty, 0, "New success must publish a live root");
        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(empty),
            0
        );

        let mut with_i32 = -1;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32_result(
                    empty,
                    b"answer".as_ptr(),
                    6,
                    42,
                    &mut with_i32,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_SET_RESULT_OK
        );
        assert_ne!(with_i32, 0, "Set(i32) success must publish a live root");
        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(empty),
            0
        );
        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(with_i32),
            1
        );
        let mut answer = -1;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_i32(
                    with_i32,
                    b"answer".as_ptr(),
                    6,
                    &mut answer,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_OK
        );
        assert_eq!(answer, 42);

        let key = "name\0🌿";
        let value = "Mimi\0字符串";
        let mut with_string = -1;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string_result(
                    with_i32,
                    key.as_ptr(),
                    key.len() as i64,
                    value.as_ptr(),
                    value.len() as i64,
                    &mut with_string,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_SET_RESULT_OK
        );
        assert_ne!(
            with_string, 0,
            "Set(String) success must publish a live root"
        );
        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(with_i32),
            1
        );
        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(with_string),
            2
        );
        let mut text = std::ptr::null_mut();
        let mut length = -1;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                    with_string,
                    key.as_ptr(),
                    key.len() as i64,
                    &mut text,
                    &mut length,
                )
            },
            super::ORDINARY_MAP_OWNED_GET_OK
        );
        assert_eq!(length as usize, value.len());
        assert_eq!(
            unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length as usize) },
            value.as_bytes()
        );
        unsafe { crate::runtime::mimi_string_free(text) };

        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(empty);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(with_i32);
        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(with_string);
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("owned-Get domain after successful root cleanup"),
            baseline,
            "successful branches must be exactly retired by Drop"
        );
    }

    #[test]
    fn owned_get_result_v1_capacity_err_codes_clear_output_and_preserve_owners() {
        // Existing source Result contracts are New Err(3) and Set Err(2).
        // The V1 helper codes deliberately keep those exact source values.
        assert_eq!(super::ORDINARY_MAP_OWNED_GET_NEW_RESULT_CAPACITY, 3);
        assert_eq!(super::ORDINARY_MAP_OWNED_GET_SET_RESULT_CAPACITY, 2);

        let baseline = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("owned-Get domain baseline before New Err");
        let mut no_root = -9;
        let (new_status, new_failure_triggered, new_publication_attempts) =
            super::with_ordinary_map_owned_get_result_root_publish_failure_for_test(0, || {
                let status = unsafe {
                    super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new_result(&mut no_root)
                };
                (
                    status,
                    super::ordinary_map_owned_get_result_root_publish_failure_triggered_for_test(),
                    super::ordinary_map_owned_get_result_root_publish_attempts_for_test(),
                )
            });
        assert_eq!(new_status, 3, "New capacity must remain source Err(3)");
        assert_eq!(no_root, 0, "New Err must not publish a partial root");
        assert!(new_failure_triggered);
        assert_eq!(new_publication_attempts, 1);
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("owned-Get domain after New Err"),
            baseline,
            "failed New must leave no root or owner behind"
        );

        let source = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_new();
        assert_ne!(source, 0);
        let before_set = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("owned-Get domain baseline before Set Err");

        let mut no_i32_root = -9;
        let (i32_status, i32_failure_triggered, i32_publication_attempts) =
            super::with_ordinary_map_owned_get_result_root_publish_failure_for_test(0, || unsafe {
                let status = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32_result(
                    source,
                    b"integer".as_ptr(),
                    7,
                    8,
                    &mut no_i32_root,
                );
                (
                    status,
                    super::ordinary_map_owned_get_result_root_publish_failure_triggered_for_test(),
                    super::ordinary_map_owned_get_result_root_publish_attempts_for_test(),
                )
            });
        assert_eq!(i32_status, 2, "Set(i32) capacity must remain source Err(2)");
        assert_eq!(no_i32_root, 0, "Set Err must not publish a new root");
        assert!(i32_failure_triggered);
        assert_eq!(i32_publication_attempts, 1);
        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(source),
            0
        );
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("owned-Get domain after staged i32 Set Err"),
            before_set,
            "failed Set(i32) must retain only the original source root and owners"
        );

        let mut no_string_root = -9;
        let (string_status, string_failure_triggered, string_publication_attempts) =
            super::with_ordinary_map_owned_get_result_root_publish_failure_for_test(0, || unsafe {
                let status = super::mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string_result(
                    source,
                    b"text".as_ptr(),
                    4,
                    b"staged value".as_ptr(),
                    12,
                    &mut no_string_root,
                );
                (
                    status,
                    super::ordinary_map_owned_get_result_root_publish_failure_triggered_for_test(),
                    super::ordinary_map_owned_get_result_root_publish_attempts_for_test(),
                )
            });
        assert_eq!(
            string_status, 2,
            "Set(String) capacity must remain source Err(2)"
        );
        assert_eq!(no_string_root, 0, "Set Err must not publish a new root");
        assert!(string_failure_triggered);
        assert_eq!(string_publication_attempts, 1);
        assert_eq!(
            super::mimi_mir_ordinary_map_owned_get_i32_string_v1_size(source),
            0
        );
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("owned-Get domain after staged String Set Err"),
            before_set,
            "failed Set(String) must drop its staged owner and keep source live"
        );

        super::mimi_mir_ordinary_map_owned_get_i32_string_v1_drop(source);
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("owned-Get domain after Set Err cleanup"),
            baseline
        );
    }

    #[test]
    fn owned_get_result_v1_only_capacity_is_recoverable() {
        // Invalid owner/input and internal errors remain fatal; they are not
        // translated into either source Map::new Err(3) or Map::set Err(2).
        for (error, operation) in [
            (
                super::OrdinaryMapSnapshotRuntimeError::InvalidInput,
                super::ORDINARY_MAP_OWNED_GET_NEW_RESULT_CAPACITY,
            ),
            (
                super::OrdinaryMapSnapshotRuntimeError::SizeOverflow,
                super::ORDINARY_MAP_OWNED_GET_SET_RESULT_CAPACITY,
            ),
            (
                super::OrdinaryMapSnapshotRuntimeError::Owner(
                    any_owner::AnyOwnerError::InvalidToken,
                ),
                super::ORDINARY_MAP_OWNED_GET_SET_RESULT_CAPACITY,
            ),
            (
                super::OrdinaryMapSnapshotRuntimeError::Handle(
                    super::super::handle::HandleError::Invalid,
                ),
                super::ORDINARY_MAP_OWNED_GET_SET_RESULT_CAPACITY,
            ),
        ] {
            assert_eq!(
                super::owned_get_result_capacity_status(error, operation),
                None,
                "{error:?} must remain a nonrecoverable runtime error"
            );
        }
        assert_eq!(
            super::owned_get_result_capacity_status(
                super::OrdinaryMapSnapshotRuntimeError::Capacity,
                super::ORDINARY_MAP_OWNED_GET_NEW_RESULT_CAPACITY,
            ),
            Some(3)
        );
        assert_eq!(
            super::owned_get_result_capacity_status(
                super::OrdinaryMapSnapshotRuntimeError::Capacity,
                super::ORDINARY_MAP_OWNED_GET_SET_RESULT_CAPACITY,
            ),
            Some(2)
        );
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

    #[test]
    fn v2_values_foreign_retirement_completes_before_creator_root_sweep() {
        let (list_tx, list_rx) = std::sync::mpsc::channel();
        let (start_drain_tx, start_drain_rx) = std::sync::mpsc::channel();
        let (drain_done_tx, drain_done_rx) = std::sync::mpsc::channel();
        let (wait_entered_tx, wait_entered_rx) = std::sync::mpsc::channel();
        let (resume_wait_tx, resume_wait_rx) = std::sync::mpsc::channel();

        let creator = std::thread::spawn(move || {
            let root = new_v2_map();
            let (empty_selection, selection_hash) = selection_records(&[]);
            let (status, list) = values_list_v2(root, &empty_selection, &selection_hash);
            assert_eq!(status, 0);
            assert!(!list.is_null());
            assert_eq!(
                unsafe { super::super::mimi_mir_list_len_managed_any_v2(list) },
                0
            );
            list_tx.send(list as usize).unwrap();

            super::with_managed_any_v2_list_drain_wait_hook_for_test(
                wait_entered_tx,
                resume_wait_rx,
                || {
                    start_drain_rx.recv().unwrap();
                    super::super::ordinary_map_domain::drain_current_thread_roots();
                    assert_eq!(super::current_thread_managed_any_v2_list_count(), Some(0));
                    drain_done_tx.send(()).unwrap();
                },
            );
        });

        let list = list_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("creator publishes its empty Values list before waiting")
            as *mut super::super::MimiList;
        let receipt_id = super::begin_managed_any_v2_list_retirement(
            list,
            super::ManagedAnyV2ListDomain::CanonicalOwnerListV2,
        )
        .expect("foreign thread can claim the active Values-list receipt")
        .expect("the empty Values list has a creator receipt");
        start_drain_tx.send(()).unwrap();

        // The creator pauses inside the registry wait while it still holds the
        // registry lock. This makes the observation deterministic: the foreign
        // destructor cannot finish until the creator atomically enters Condvar
        // wait and releases that lock.
        let wait_observed = wait_entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok();
        let drain_returned_before_resume = drain_done_rx.try_recv().is_ok();
        // The unbounded resume channel lets every failure path release a
        // creator that is paused in the hook, even if it reached the hook just
        // after the observation timeout or already returned.
        let resume_enqueued = resume_wait_tx.send(()).is_ok();

        // Empty Values avoids masking this ordering assertion with a duplicate
        // token release if the creator returns early. We still complete the
        // foreign claim on every path so a failed regression cannot leak a
        // blocked creator thread.
        unsafe { super::super::mimi_mir_list_drop_managed_any_v2_claimed(list, receipt_id) };
        let drain_completed = drain_done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok();
        let finish_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !creator.is_finished() && std::time::Instant::now() < finish_deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let creator_finished = creator.is_finished();
        let creator_join_succeeded = if creator_finished {
            creator.join().is_ok()
        } else {
            // A failed wake must not strand the test harness in JoinHandle::join.
            drop(creator);
            false
        };

        assert!(
            wait_observed,
            "creator drain must wait for a foreign Retiring receipt"
        );
        assert!(
            !drain_returned_before_resume,
            "creator drain returned before the foreign destructor finished"
        );
        assert!(
            !wait_observed || resume_enqueued,
            "observed creator wait must accept its buffered resume signal"
        );
        assert!(
            drain_completed,
            "creator drain completes after receipt finish"
        );
        assert!(creator_finished, "creator thread finishes after cleanup");
        assert!(creator_join_succeeded, "creator thread must not panic");
    }

    #[test]
    fn v2_values_drain_detects_its_claim_among_foreign_retirements() {
        let current = super::super::handle::current_lease_thread_id();
        let first = super::super::mimi_mir_list_new_managed_any_v2();
        let second = super::super::mimi_mir_list_new_managed_any_v2();
        let third = super::super::mimi_mir_list_new_managed_any_v2();
        assert!(!first.is_null());
        assert!(!second.is_null());
        assert!(!third.is_null());
        let first_address = first as usize;

        let (foreign_claimed_tx, foreign_claimed_rx) = std::sync::mpsc::channel();
        let (finish_foreign_tx, finish_foreign_rx) = std::sync::mpsc::channel();
        let foreign = std::thread::spawn(move || {
            let first = first_address as *mut super::super::MimiList;
            let receipt_id = match super::begin_managed_any_v2_list_retirement(
                first,
                super::ManagedAnyV2ListDomain::CanonicalOwnerListV2,
            ) {
                Ok(Some(receipt_id)) => Some(receipt_id),
                Ok(None) | Err(()) => None,
            };
            let _ = foreign_claimed_tx.send(receipt_id);
            let _ = finish_foreign_rx.recv_timeout(std::time::Duration::from_secs(5));
            if let Some(receipt_id) = receipt_id {
                unsafe {
                    super::super::mimi_mir_list_drop_managed_any_v2_claimed(first, receipt_id)
                };
            }
        });
        let foreign_receipt = foreign_claimed_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .flatten();

        let second_receipt = super::begin_managed_any_v2_list_retirement(
            second,
            super::ManagedAnyV2ListDomain::CanonicalOwnerListV2,
        )
        .ok()
        .flatten();
        let decision = if second_receipt.is_some() {
            super::claim_or_wait_current_thread_managed_any_v2_list_for_drain(current, current)
        } else {
            super::ManagedAnyV2ListDrainClaim::Complete
        };

        // Complete all receipts before assertions so a failed decision cannot
        // leave registry entries that poison later lifecycle tests.
        if let super::ManagedAnyV2ListDrainClaim::Claimed(list, receipt_id) = &decision {
            unsafe { super::super::mimi_mir_list_drop_managed_any_v2_claimed(*list, *receipt_id) };
        }
        if let Some(second_receipt) = second_receipt {
            unsafe {
                super::super::mimi_mir_list_drop_managed_any_v2_claimed(second, second_receipt)
            };
        }
        if let Some(third_receipt) = super::begin_managed_any_v2_list_retirement(
            third,
            super::ManagedAnyV2ListDomain::CanonicalOwnerListV2,
        )
        .ok()
        .flatten()
        {
            unsafe {
                super::super::mimi_mir_list_drop_managed_any_v2_claimed(third, third_receipt)
            };
        }
        let _ = finish_foreign_tx.send(());
        let foreign_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !foreign.is_finished() && std::time::Instant::now() < foreign_deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let foreign_finished = foreign.is_finished();
        let foreign_join_succeeded = if foreign_finished {
            foreign.join().is_ok()
        } else {
            drop(foreign);
            false
        };
        let foreign_receipt =
            foreign_receipt.or_else(|| foreign_claimed_rx.try_recv().ok().flatten());
        if foreign_finished && foreign_receipt.is_none() {
            unsafe { super::super::mimi_mir_list_drop_managed_any_v2(first) };
        }
        if second_receipt.is_none() {
            unsafe { super::super::mimi_mir_list_drop_managed_any_v2(second) };
        }

        assert!(
            foreign_receipt.is_some(),
            "foreign thread must claim the first Retiring receipt"
        );
        assert!(
            matches!(decision, super::ManagedAnyV2ListDrainClaim::Reentrant),
            "same-thread Retiring must win over foreign Retiring and Active receipts"
        );
        assert!(foreign_finished, "foreign cleanup thread finishes");
        assert!(
            foreign_join_succeeded,
            "foreign cleanup thread must not panic"
        );
    }

    #[test]
    fn v2_keys_snapshot_sorts_full_utf8_bytes_and_index_zero_copy_outlives_both_owners() {
        let counts_before = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains before Keys snapshot");
        let mut root = new_v2_map();
        let entries: &[(&[u8], i32)] = &[
            ("零🌿".as_bytes(), 0),
            (b"z", 1),
            (b"a\0", 2),
            ("☃".as_bytes(), 3),
            ("ä".as_bytes(), 4),
            (b"a", 5),
        ];
        for (key, value) in entries {
            let owner = new_v2_i32_owner(*value);
            let next = set_v2_owner(root, key, owner, 1, V2_I32_TYPE_ID, &V2_I32_HASH);
            drop_v2_map(root);
            assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
            root = next;
        }

        let expected: &[&[u8]] = &[
            b"a",
            b"a\0",
            b"z",
            "ä".as_bytes(),
            "☃".as_bytes(),
            "零🌿".as_bytes(),
        ];
        assert_eq!(size_v2(root), (0, expected.len() as i32));
        for (key, _) in entries {
            assert_eq!(contains_v2(root, key), (0, 1), "source Map retains {key:?}");
        }

        let (status, list) = keys_list_v2(root);
        assert_eq!(status, 0);
        assert!(!list.is_null());
        assert_eq!(unsafe { (*list).len }, expected.len() as i64);
        for (index, expected_key) in expected.iter().enumerate() {
            assert_eq!(unsafe { keys_list_bytes(list, index) }, *expected_key);
        }
        assert_eq!(size_v2(root), (0, expected.len() as i32));

        let mut copied = std::ptr::null_mut();
        let mut copied_length = -1;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_any_v2_keys_unpack_copy_string(
                    list,
                    &mut copied,
                    &mut copied_length,
                )
            },
            0
        );
        assert!(!copied.is_null());
        assert_eq!(copied_length, 1);
        assert_eq!(unsafe { *copied.cast::<u8>() }, b'a');
        let (first_payload, _) = unsafe { super::super::list_string::read_mimi_str(*(*list).data) }
            .expect("first list key is an owned ABI-v3 String");
        assert_ne!(
            copied, first_payload,
            "index zero must be a separate allocation"
        );

        drop_v2_map(root);
        assert_eq!(unsafe { keys_list_bytes(list, 0) }, b"a");
        unsafe { super::super::mimi_mir_list_drop_string(list) };
        assert_eq!(
            unsafe { std::slice::from_raw_parts(copied.cast::<u8>(), 1) },
            b"a"
        );
        unsafe { super::super::mimi_string_free(copied) };
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after Keys snapshot and copied key"),
            counts_before
        );
    }

    #[test]
    fn v2_keys_snapshot_allocation_failures_rollback_prefix_and_preserve_source() {
        let domain_baseline = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership-domain baseline before Keys allocation failures");
        let mut root = new_v2_map();
        for (index, key) in [b"delta".as_slice(), b"alpha", "雪".as_bytes(), b"omega"]
            .into_iter()
            .enumerate()
        {
            let owner = new_v2_i32_owner(index as i32);
            let next = set_v2_owner(root, key, owner, 1, V2_I32_TYPE_ID, &V2_I32_HASH);
            drop_v2_map(root);
            assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
            root = next;
        }
        let counts_with_root = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains before Keys allocation failures");
        let rollback_before = super::ordinary_map_owned_any_v2_keys_list_rollbacks_for_test();

        // Four keys exercise sorting storage, each key clone, List allocation,
        // each appended ABI-v3 box, and final output publication. Every
        // checkpoint is failed once; failures after List creation must drop
        // the empty or partially populated List and all of its boxes.
        for fail_after in 0..12 {
            super::begin_failure_injection(fail_after);
            let (status, list) = keys_list_v2(root);
            let staged_tokens = super::end_failure_injection();
            assert_eq!(
                status,
                ManagedAnyStatus::Capacity as i32,
                "stage {fail_after}"
            );
            assert!(list.is_null(), "stage {fail_after} must not publish a List");
            assert!(staged_tokens.is_empty(), "Keys must not create Any owners");
            assert_eq!(size_v2(root), (0, 4), "stage {fail_after}");
            for key in [b"delta".as_slice(), b"alpha", "雪".as_bytes(), b"omega"] {
                assert_eq!(contains_v2(root, key), (0, 1), "stage {fail_after}");
            }
            assert_eq!(
                crate::runtime::ordinary_map_domain_live_counts_for_test()
                    .expect("ownership domains after failed Keys snapshot"),
                counts_with_root,
                "stage {fail_after} leaked a Map/String owner"
            );
        }
        assert_eq!(
            super::ordinary_map_owned_any_v2_keys_list_rollbacks_for_test() - rollback_before,
            5,
            "the five failures after List allocation must run List rollback"
        );

        super::begin_failure_injection(12);
        let (status, list) = keys_list_v2(root);
        let staged_tokens = super::end_failure_injection();
        assert_eq!(status, 0);
        assert!(!list.is_null());
        assert!(staged_tokens.is_empty());
        unsafe { super::super::mimi_mir_list_drop_string(list) };
        assert_eq!(size_v2(root), (0, 4));
        drop_v2_map(root);
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after all Keys allocation-failure checks"),
            domain_baseline
        );
    }

    #[test]
    fn v2_keys_unpack_copy_allocation_and_publication_failures_leave_list_owned() {
        let domain_baseline = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership-domain baseline before Keys index-zero copy");
        let mut root = new_v2_map();
        let owner = new_v2_i32_owner(7);
        let next = set_v2_owner(
            root,
            "alpha".as_bytes(),
            owner,
            1,
            V2_I32_TYPE_ID,
            &V2_I32_HASH,
        );
        drop_v2_map(root);
        assert_eq!(super::mimi_any_payload_v2_drop(owner), 0);
        root = next;
        let (status, list) = keys_list_v2(root);
        assert_eq!(status, 0);
        assert!(!list.is_null());
        let counts_with_list = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains before Keys index-zero copy");

        for stage in [
            super::ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_CLONE_STAGE,
            super::ORDINARY_MAP_OWNED_ANY_V2_STRING_COPY_PUBLISH_STAGE,
        ] {
            let (status, pointer, length) =
                super::with_ordinary_map_owned_any_v2_string_copy_failure_for_test(stage, || {
                    let mut pointer = std::ptr::null_mut();
                    let mut length = -1;
                    let status = unsafe {
                        super::mimi_mir_ordinary_map_owned_any_v2_keys_unpack_copy_string(
                            list,
                            &mut pointer,
                            &mut length,
                        )
                    };
                    (status, pointer, length)
                });
            assert_eq!(status, ManagedAnyStatus::Capacity as i32);
            assert!(pointer.is_null());
            assert_eq!(length, 0);
            assert_eq!(unsafe { keys_list_bytes(list, 0) }, b"alpha");
            assert_eq!(
                crate::runtime::ordinary_map_domain_live_counts_for_test()
                    .expect("ownership domains after failed Keys String copy"),
                counts_with_list,
                "failed copy stage {stage} leaked an owned String"
            );
        }

        let mut copied = std::ptr::null_mut();
        let mut copied_length = -1;
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_any_v2_keys_unpack_copy_string(
                    list,
                    &mut copied,
                    &mut copied_length,
                )
            },
            0
        );
        assert_eq!(copied_length, 5);
        drop_v2_map(root);
        unsafe { super::super::mimi_mir_list_drop_string(list) };
        assert_eq!(
            unsafe { std::slice::from_raw_parts(copied.cast::<u8>(), 5) },
            b"alpha"
        );
        unsafe { super::super::mimi_string_free(copied) };
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after independent Keys String copy"),
            domain_baseline
        );
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
    use super::tests::{
        clone_v2_test_string_failure, drop_v2_map, drop_v2_test_string_failure, get_test_string,
        new_v2_map, set_test_string, size_v2, V2_TEST_STRING_CLONE_FAILURE_HASH,
        V2_TEST_STRING_CLONE_FAILURE_TYPE_ID,
    };
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
    fn ordinary_map_new_result_runtime_reserve_capacity_returns_status_without_output() {
        let counts_before = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains before V2 root reserve failure");
        let mut root = i64::MAX;
        super::super::ordinary_map_domain::with_root_reserve_overflow_for_test(|| {
            let status = unsafe { mimi_ordinary_map_owned_any_v2_new(&mut root) };
            assert_eq!(status, ManagedAnyStatus::Capacity as i32);
            assert_eq!(root, 0, "failed publication must not expose a handle");
            assert_eq!(
                super::super::ordinary_map_domain::root_reserve_overflow_observation_for_test(),
                (1, true),
                "Capacity must come from the real ROOTS Vec::try_reserve CapacityOverflow path"
            );
        });
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after V2 root reserve failure"),
            counts_before,
            "the unpublished handle and root leave no residual runtime owner"
        );
    }

    #[test]
    fn v2_unpack_string_bridge_is_fallible_for_empty_and_accepts_large_bytes() {
        let counts_before = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains before V2 String bridge");

        // The shared allocation failpoint reaches the final canonical String
        // allocation. Empty input still has a real allocation and status, so
        // this failure must not be mistaken for a valid empty String.
        let (allocation_status, allocation_attempts) =
            super::with_ordinary_map_owned_get_allocation_failure_for_test(0, || {
                let mut sentinel = 0_u8;
                let mut pointer = (&mut sentinel as *mut u8).cast::<std::ffi::c_char>();
                let mut length = -1;
                let status = unsafe {
                    super::mimi_mir_ordinary_map_owned_any_v2_values_unpack_copy_string(
                        std::ptr::null(),
                        0,
                        &mut pointer,
                        &mut length,
                    )
                };
                assert!(pointer.is_null());
                assert_eq!(length, 0);
                (
                    status,
                    super::ordinary_map_owned_get_allocation_attempts_for_test(),
                )
            });
        assert_eq!(allocation_status, ManagedAnyStatus::Capacity as i32);
        assert_eq!(allocation_attempts, 1);

        // A failed publication frees the already allocated empty String and
        // leaves both output slots in their canonical failure state.
        let mut sentinel = 0_u8;
        let mut pointer = (&mut sentinel as *mut u8).cast::<std::ffi::c_char>();
        let mut length = -1;
        let publication_status =
            super::super::ordinary_map_domain::with_registration_failure_for_test(|| unsafe {
                super::mimi_mir_ordinary_map_owned_any_v2_unpack_copy_string(
                    std::ptr::null(),
                    0,
                    &mut pointer,
                    &mut length,
                )
            });
        assert_eq!(publication_status, ManagedAnyStatus::Capacity as i32);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after failed publication"),
            counts_before,
            "failed empty publication retires its fresh allocation"
        );

        // Empty success owns a non-null allocation, preserving the same
        // status distinction that the callback emitter relies on.
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
        assert!(!pointer.is_null());
        assert_eq!(length, 0);
        unsafe { super::super::mimi_string_free(pointer) };

        let bytes = "before\0snow 雪".as_bytes();
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
        assert_eq!(length, bytes.len() as i64);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) },
            bytes
        );
        assert_eq!(unsafe { *pointer.cast::<u8>().add(length as usize) }, 0);
        unsafe { super::super::mimi_string_free(pointer) };

        // `mimi_str_clone` rejects this size. The V2 status bridge copies the
        // exact payload bytes without that unrelated 64 MiB API limit.
        let large = vec![0xa5_u8; 64 * 1024 * 1024 + 1];
        assert_eq!(
            unsafe {
                super::mimi_mir_ordinary_map_owned_any_v2_unpack_copy_string(
                    large.as_ptr().cast(),
                    large.len() as i64,
                    &mut pointer,
                    &mut length,
                )
            },
            ManagedAnyStatus::Ok as i32
        );
        assert_eq!(length, large.len() as i64);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) },
            large.as_slice()
        );
        unsafe { super::super::mimi_string_free(pointer) };
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after successful bridge copies"),
            counts_before
        );
    }

    #[test]
    fn v2_set_get_string_roundtrip_is_byte_exact_and_detached_from_every_root() {
        let values: [(&[u8], &[u8]); 3] = [
            (b"empty", b""),
            (b"utf8", "雪 🌿 東京".as_bytes()),
            (b"nul", b"before\0after"),
        ];
        let mut roots = vec![new_v2_map()];
        let mut current = roots[0];
        for (key, value) in values {
            let (status, next) = set_test_string(current, key, value);
            assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_OK);
            assert_ne!(next, 0);
            assert_eq!(
                size_v2(current),
                (0, (roots.len() - 1) as i32),
                "Set retains its source root"
            );
            roots.push(next);
            current = next;
        }

        let mut detached = Vec::new();
        for (key, expected) in values {
            let (status, pointer, length) = get_test_string(current, key);
            assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_OK);
            assert!(!pointer.is_null(), "even an empty String owns storage");
            assert_eq!(length, expected.len() as i64);
            assert_eq!(
                unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) },
                expected
            );
            assert_eq!(unsafe { *pointer.cast::<u8>().add(length as usize) }, 0);
            detached.push((pointer, expected));
        }

        // The returned strings belong to the canonical Mimi String domain;
        // release every persistent root before proving those copies readable.
        for root in roots {
            drop_v2_map(root);
        }
        for (pointer, expected) in detached {
            assert_eq!(
                unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), expected.len()) },
                expected
            );
            unsafe { super::super::mimi_string_free(pointer) };
        }
    }

    #[test]
    fn v2_get_string_distinguishes_missing_and_wrong_type_with_empty_outputs() {
        let empty = new_v2_map();
        let (missing, pointer, length) = get_test_string(empty, b"absent");
        assert_eq!(missing, super::ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_MISSING);
        assert!(pointer.is_null());
        assert_eq!(length, 0);

        let mut integer_root = 0;
        assert_eq!(
            unsafe {
                super::mimi_ordinary_map_owned_any_v2_set_i32_result(
                    empty,
                    b"integer".as_ptr(),
                    7,
                    42,
                    &mut integer_root,
                )
            },
            super::ORDINARY_MAP_OWNED_ANY_V2_OK,
            "i32 Set succeeds on the separate narrow ABI"
        );
        let (mismatch, pointer, length) = get_test_string(integer_root, b"integer");
        assert_eq!(
            mismatch,
            super::ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_TYPE_MISMATCH
        );
        assert!(pointer.is_null());
        assert_eq!(length, 0);

        drop_v2_map(integer_root);
        drop_v2_map(empty);
    }

    #[test]
    fn v2_set_get_string_failures_clear_outputs_and_retire_staged_owners() {
        let root = new_v2_map();
        let before_set = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains before failed Set");
        let failed_publish =
            super::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
                set_test_string(root, b"text", b"staged")
            });
        assert_eq!(
            failed_publish,
            (super::ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_CAPACITY, 0)
        );
        assert_eq!(size_v2(root), (0, 0));
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after failed Set publication"),
            before_set
        );

        // A compiler callback's arbitrary nonzero status is not capacity and
        // therefore cannot be translated to the recoverable Set error.
        let mut callback_failed_root = -1_i64;
        let callback_failure = unsafe {
            super::mimi_ordinary_map_owned_any_v2_set_string_result(
                root,
                b"text".as_ptr(),
                4,
                b"staged".as_ptr().cast(),
                6,
                V2_TEST_STRING_CLONE_FAILURE_TYPE_ID.as_ptr(),
                V2_TEST_STRING_CLONE_FAILURE_TYPE_ID.len() as i64,
                V2_TEST_STRING_CLONE_FAILURE_HASH.as_ptr(),
                Some(clone_v2_test_string_failure),
                Some(drop_v2_test_string_failure),
                &mut callback_failed_root,
            )
        };
        assert_eq!(callback_failure, ManagedAnyStatus::CloneFailed as i32);
        assert_eq!(callback_failed_root, 0);
        assert_eq!(size_v2(root), (0, 0));
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after failed compiler callback"),
            before_set
        );

        let (status, populated) = set_test_string(root, b"text", b"still readable");
        assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_OK);
        let before_get = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains before failed Get copies");
        let before_temporary_drops =
            super::ordinary_map_owned_any_v2_get_string_temp_drops_for_test();
        let allocation_failure =
            super::with_ordinary_map_owned_get_allocation_failure_for_test(0, || {
                get_test_string(populated, b"text")
            });
        assert_eq!(
            allocation_failure,
            (
                super::ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_CAPACITY,
                std::ptr::null_mut(),
                0
            )
        );
        assert_eq!(
            super::ordinary_map_owned_any_v2_get_string_temp_drops_for_test(),
            before_temporary_drops + 1,
            "the unpacked temporary is retired after final allocation failure"
        );
        let publication_failure =
            super::super::ordinary_map_domain::with_registration_failure_for_test(|| {
                get_test_string(populated, b"text")
            });
        assert_eq!(
            publication_failure,
            (
                super::ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_CAPACITY,
                std::ptr::null_mut(),
                0
            )
        );
        assert_eq!(
            super::ordinary_map_owned_any_v2_get_string_temp_drops_for_test(),
            before_temporary_drops + 2,
            "the unpacked temporary is retired after owned String registration failure"
        );
        assert_eq!(
            crate::runtime::ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after failed Get copies"),
            before_get
        );

        let (status, pointer, length) = get_test_string(populated, b"text");
        assert_eq!(status, super::ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_OK);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) },
            b"still readable"
        );
        unsafe { super::super::mimi_string_free(pointer) };
        drop_v2_map(populated);
        drop_v2_map(root);
    }
}
