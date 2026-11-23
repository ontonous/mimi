//! Execution-root ownership for the two closed ordinary Map profiles.
//!
//! A root is registered before its opaque, generational handle is published.
//! Registration follows the creating native thread across scalar call frames;
//! normal dedicated Drop can unregister it from any thread. An orderly trap
//! extracts only the trapping thread's roots, one at a time, and retires each
//! handle without holding the execution-domain registry lock.
//! Fresh canonical String owners produced by a receipted ordinary-Map native
//! program use a separate typed registry, including retained getter results
//! and dynamic keys that remain live across a trapping scalar call.

use std::ffi::c_char;
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, Once};

#[cfg(standalone)]
use super::libc as system_libc;
#[cfg(not(standalone))]
use libc as system_libc;

use super::any_owner::{self, AnyOwnerToken, DescriptorIdentity};
use super::handle::{self, LeaseThreadId};
use super::managed_any_payload_v2::ManagedAnyPayloadV2Identity;

const ORDINARY_MAP_OWNED_ANY_V2_GENERIC_SET_CAPACITY: i32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OrdinaryMapProfile {
    OwnerLifecycleI32StringV1,
    OwnedGetI32StringV1,
    AnyOwnerV2Record,
    OwnedAnyV2,
}

impl OrdinaryMapProfile {
    /// Cleanup deliberately uses the raw handle API, never the trapping ABI
    /// wrapper or normal Drop's execution-domain unregister path.
    fn retire_for_cleanup(self, root: i64) {
        let _ = match self {
            Self::OwnerLifecycleI32StringV1 => {
                handle::ordinary_map_owner_lifecycle_i32_string_v1_drop_for_trap(root)
            }
            Self::OwnedGetI32StringV1 => {
                handle::ordinary_map_owned_get_i32_string_v1_drop_for_trap(root)
            }
            Self::AnyOwnerV2Record => handle::ordinary_map_any_owner_v2_record_drop_for_trap(root),
            Self::OwnedAnyV2 => handle::ordinary_map_owned_any_v2_drop_for_trap(root),
        };
        // Stale, already-dropped, or busy roots must not raise a second trap
        // and replace the diagnostic that initiated orderly shutdown.
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OrdinaryMapDomainError {
    Capacity,
}

#[derive(Clone, Copy)]
struct RegisteredRoot {
    creator: LeaseThreadId,
    profile: OrdinaryMapProfile,
    handle: i64,
}

// This registry owns only identities, never Map objects or user Drop glue.
// Vec::new and Mutex::new are const and do not allocate on first cleanup.
static ROOTS: Mutex<Vec<RegisteredRoot>> = Mutex::new(Vec::new());
static PROCESS_SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static PROCESS_SHUTDOWN_HOOK: Once = Once::new();
#[cfg(feature = "ordinary-map-set-result-l3")]
static ORDINARY_MAP_SET_RESULT_L3_SHUTDOWN_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Unique runtime-owned handle returned by the production V2 New ABI.
///
/// This is deliberately not `Clone`: the wrapper represents one linear owner
/// of one registered root. Its drop path retires only that handle and never
/// drains other roots owned by the current thread.
pub(crate) struct OwnedAnyV2Root {
    handle: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnedAnyV2RootError {
    /// The production New ABI returned exactly Capacity with a zero output.
    Capacity,
    /// The New ABI returned a noncanonical status/output combination. If it
    /// published a nonzero handle, `try_new` retires that exact handle before
    /// returning this error.
    UnexpectedNewResult { status: i32, published_handle: bool },
    /// Size returned any non-OK status. This remains an invariant error and
    /// is never confused with New's recoverable Capacity result.
    SizeStatus(i32),
    /// The root handle could not be retired through its exact profile ABI.
    DropStatus(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnedAnyV2RootSetError {
    /// An exact typed or generic Set ABI capacity pair was returned with no
    /// published root. The borrowed source root remains live.
    Capacity,
    /// Set returned a noncanonical status/output pair. If the runtime
    /// published an output despite failure, the wrapper retires that exact
    /// handle before returning this error.
    UnexpectedSetResult { status: i32, published_handle: bool },
}

/// The exact physical Core String payload layout pinned by the V2 descriptor.
/// Bytes use C allocation so the paired clone/drop glue can cross the runtime
/// callback ABI without mixing Mimi's canonical String allocator domain.
#[repr(C)]
#[derive(Clone, Copy)]
struct OwnedAnyV2StringBox {
    data: *mut c_char,
    length: i64,
}

/// Clone one canonical `{ptr, i64}` String payload for an Any V2 owner.
///
/// Empty Strings use a null pointer and zero length. Non-empty payloads are
/// copied byte-for-byte into `malloc(length + 1)` storage, with a trailing NUL
/// for compatibility; the explicit length remains authoritative, so embedded
/// NUL bytes are preserved. The paired Drop glue calls C `free` exactly once
/// for each non-empty clone. Invalid length/pointer/UTF-8 or allocation
/// failure leaves `destination` wholly uninitialized and returns nonzero.
///
/// # Safety
/// The V2 glue registry calls this with aligned, writable destination storage
/// and a valid, aligned StringBox source matching the descriptor layout.
unsafe extern "C" fn clone_owned_any_v2_string_box(destination: *mut u8, source: *const u8) -> i32 {
    if destination.is_null() || source.is_null() {
        return 1;
    }
    // SAFETY: the registered descriptor fixes source to this exact C layout.
    let source = unsafe { source.cast::<OwnedAnyV2StringBox>().read() };
    let Ok(length) = usize::try_from(source.length) else {
        return 1;
    };
    if length > isize::MAX as usize || (length > 0 && source.data.is_null()) {
        return 1;
    }
    if length == 0 {
        // SAFETY: the callback contract supplies aligned, writable storage.
        unsafe {
            destination
                .cast::<OwnedAnyV2StringBox>()
                .write(OwnedAnyV2StringBox {
                    data: std::ptr::null_mut(),
                    length: 0,
                })
        };
        return 0;
    }

    // SAFETY: non-empty source storage and its explicit length were checked;
    // the descriptor's String invariant requires UTF-8 bytes.
    let bytes = unsafe { std::slice::from_raw_parts(source.data.cast::<u8>(), length) };
    if std::str::from_utf8(bytes).is_err() {
        return 1;
    }
    let Some(allocation_size) = length.checked_add(1) else {
        return 1;
    };
    // SAFETY: allocation_size is nonzero and checked above; its allocation is
    // paired with the C `free` call in `drop_owned_any_v2_string_box`.
    let data = unsafe { system_libc::malloc(allocation_size) }.cast::<c_char>();
    if data.is_null() {
        return 1;
    }
    // SAFETY: allocation_size is length + 1, source has length readable bytes,
    // and the new allocation is disjoint from its source.
    unsafe {
        std::ptr::copy_nonoverlapping(source.data.cast::<u8>(), data.cast::<u8>(), length);
        data.add(length).write(0);
        destination
            .cast::<OwnedAnyV2StringBox>()
            .write(OwnedAnyV2StringBox {
                data,
                length: source.length,
            });
    }
    0
}

/// Drop one initialized Any V2 String payload allocated by the clone glue.
///
/// Empty Strings own no allocation. Every non-empty clone is paired with one
/// `free`; the source String borrowed by Set is never freed here.
///
/// # Safety
/// The V2 glue registry calls this once for an initialized StringBox created
/// by `clone_owned_any_v2_string_box`.
unsafe extern "C" fn drop_owned_any_v2_string_box(payload: *mut u8) {
    if payload.is_null() {
        return;
    }
    // SAFETY: registry invokes Drop only for a fully initialized payload.
    let value = unsafe { payload.cast::<OwnedAnyV2StringBox>().read() };
    if !value.data.is_null() {
        // SAFETY: the clone glue allocated this exact buffer with C malloc.
        unsafe { system_libc::free(value.data.cast()) };
    }
}

impl OwnedAnyV2Root {
    const OK: i32 = 0;
    const CAPACITY: i32 = 3;

    /// Create one unique owner through the production New ABI. Only the exact
    /// `(status=3, out=0)` pair is recoverable Capacity; all other malformed
    /// pairs fail closed after retiring any nonzero published handle.
    pub(crate) fn try_new() -> Result<Self, OwnedAnyV2RootError> {
        let mut handle = 0_i64;
        // SAFETY: `handle` is a writable output slot for the production New ABI.
        let status =
            unsafe { super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_new(&mut handle) };
        match (status, handle) {
            (Self::OK, handle) if handle != 0 => Ok(Self {
                handle: Some(handle),
            }),
            (Self::CAPACITY, 0) => Err(OwnedAnyV2RootError::Capacity),
            (status, handle) => {
                let published_handle = handle != 0;
                if published_handle {
                    Self::retire_unpublished_handle(handle);
                }
                Err(OwnedAnyV2RootError::UnexpectedNewResult {
                    status,
                    published_handle,
                })
            }
        }
    }

    /// Borrow the opaque handle for one exact-profile ABI operation.
    pub(crate) fn handle(&self) -> i64 {
        self.handle
            .expect("OwnedAnyV2Root handle exists until its unique drop")
    }

    /// Read the root size through its exact production ABI. No non-OK status,
    /// including integer 3, is translated to New's Capacity error.
    pub(crate) fn size(&self) -> Result<i32, OwnedAnyV2RootError> {
        let mut size = -1_i32;
        // SAFETY: the wrapper owns this live handle and `size` is writable.
        let status = unsafe {
            super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_size(self.handle(), &mut size)
        };
        if status == Self::OK && size >= 0 {
            Ok(size)
        } else {
            Err(OwnedAnyV2RootError::SizeStatus(status))
        }
    }

    /// Check key presence through the production Contains ABI without altering root ownership.
    pub(crate) fn contains_key(&self, key: &str) -> Result<bool, String> {
        let key_len = i64::try_from(key.len())
            .map_err(|_| "ordinary Map Contains key length exceeds the runtime ABI".to_string())?;
        let mut contains = -1_i32;
        // SAFETY: `self` owns this live handle, `key` is valid UTF-8 for this call,
        // and `contains` is writable.
        let status = unsafe {
            super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_contains(
                self.handle(),
                key.as_ptr(),
                key_len,
                &mut contains,
            )
        };
        if status == super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_OK
            && matches!(contains, 0 | 1)
        {
            Ok(contains == 1)
        } else {
            Err(format!(
                "ordinary Map Contains failed (status={status}, contains={contains})"
            ))
        }
    }

    /// Remove `key` through the production Remove ABI, returning a fresh unique root.
    pub(crate) fn remove(&self, key: &str) -> Result<Self, String> {
        let key_len = i64::try_from(key.len())
            .map_err(|_| "ordinary Map Remove key length exceeds the runtime ABI".to_string())?;
        let mut new_handle = 0_i64;
        // SAFETY: `self` owns this live handle, `key` is valid UTF-8 for this call,
        // and `new_handle` is a writable output slot.
        let status = unsafe {
            super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_remove(
                self.handle(),
                key.as_ptr(),
                key_len,
                &mut new_handle,
            )
        };
        if status == super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_OK && new_handle != 0 {
            Ok(Self {
                handle: Some(new_handle),
            })
        } else {
            if new_handle != 0 {
                Self::retire_unpublished_handle(new_handle);
            }
            Err(format!(
                "ordinary Map Remove returned a noncanonical status/output pair (status={status}, handle_published={})",
                new_handle != 0
            ))
        }
    }

    /// Derive a new root with one canonical Core `i32` value. This borrows the
    /// source root and does not consume or mutate it; only exact Set capacity
    /// `(status=2, out=0)` is recoverable.
    pub(crate) fn try_set_i32(
        &self,
        key: &str,
        value: i32,
    ) -> Result<Self, OwnedAnyV2RootSetError> {
        let key_len = match i64::try_from(key.len()) {
            Ok(length) => length,
            Err(_) => {
                return Err(OwnedAnyV2RootSetError::UnexpectedSetResult {
                    status: super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_SET_INVALID_INPUT,
                    published_handle: false,
                });
            }
        };
        let mut handle = 0_i64;
        // SAFETY: `self` owns a live source handle; the key slice remains
        // readable for this call and `handle` is a writable output slot.
        let status = unsafe {
            super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_set_i32_result(
                self.handle(),
                key.as_ptr(),
                key_len,
                value,
                &mut handle,
            )
        };
        match (status, handle) {
            (super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_OK, handle) if handle != 0 => {
                Ok(Self {
                    handle: Some(handle),
                })
            }
            (super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_SET_CAPACITY, 0) => {
                Err(OwnedAnyV2RootSetError::Capacity)
            }
            (status, handle) => {
                let published_handle = handle != 0;
                if published_handle {
                    Self::retire_unpublished_handle(handle);
                }
                Err(OwnedAnyV2RootSetError::UnexpectedSetResult {
                    status,
                    published_handle,
                })
            }
        }
    }

    /// Execute the production GetAny ABI for the exact canonical i32 payload
    /// admitted by the Result-aware Set/GetAny route. The caller owns the
    /// returned Any token and must keep it alive until its Checker-receipted
    /// pair Drop.
    pub(crate) fn get_any_i32_token(&self, key: &str) -> Result<(bool, i64), String> {
        let key_len = i64::try_from(key.len())
            .map_err(|_| "ordinary Map GetAny key length exceeds the runtime ABI".to_string())?;
        let glue = super::managed_any_payload_v2::canonical_i32_glue();
        let mut found = 0_i32;
        let mut token = 0_i64;
        // SAFETY: `self` owns this live root; the key is valid UTF-8 for the
        // duration of the call; output slots do not overlap; and the exact
        // canonical i32 identity/layout selects runtime-owned memcpy/no-op
        // glue regardless of the non-null callback pair below.
        let status = unsafe {
            super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_get(
                self.handle(),
                key.as_ptr(),
                key_len,
                &mut found,
                &mut token,
                1,
                super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.as_ptr(),
                super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.len() as i64,
                super::managed_any_payload_v2::CANONICAL_I32_TYPE_DESC_HASH.as_ptr(),
                std::mem::size_of::<i32>() as i64,
                std::mem::align_of::<i32>() as i64,
                Some(glue.clone),
                Some(glue.drop),
            )
        };
        if status == super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_OK
            && matches!(found, 0 | 1)
            && token != 0
        {
            return Ok((found == 1, token));
        }
        if token != 0 {
            let drop_status = super::map_any_snapshot::mimi_any_payload_v2_drop(token);
            if drop_status != 0 {
                std::process::abort();
            }
        }
        Err(format!(
            "ordinary Map GetAny returned a noncanonical status/output pair (status={status}, found={found}, token_published={})",
            token != 0
        ))
    }

    /// Execute the production GetStringResult ABI for one String entry in an
    /// active V2 runtime root. Always releases the runtime-allocated buffer
    /// before returning the owned Rust `String`.
    pub(crate) fn get_string_result_value(
        &self,
        key: &str,
        type_id: &[u8],
        descriptor_hash: &[u8; 32],
    ) -> Result<String, String> {
        let key_len = i64::try_from(key.len()).map_err(|_| {
            "ordinary Map GetStringResult key length exceeds runtime ABI".to_string()
        })?;
        let type_id_len = i64::try_from(type_id.len()).map_err(|_| {
            "ordinary Map GetStringResult TypeDesc length exceeds runtime ABI".to_string()
        })?;
        let mut out_data = std::ptr::null_mut();
        let mut out_len = 0_i64;
        // SAFETY: `self` owns this live root; `key`, `type_id`, and `descriptor_hash`
        // remain valid for the duration of the call; `out_data`/`out_len` are writable.
        let status = unsafe {
            super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_get_string_result(
                self.handle(),
                key.as_ptr(),
                key_len,
                if type_id.is_empty() {
                    std::ptr::null()
                } else {
                    type_id.as_ptr()
                },
                type_id_len,
                descriptor_hash.as_ptr(),
                &mut out_data,
                &mut out_len,
            )
        };
        if status != super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_OK {
            if !out_data.is_null() {
                super::mimi_free(out_data.cast());
            }
            return Err(format!(
                "ordinary Map GetStringResult failed with status {status}"
            ));
        }
        if out_data.is_null() || out_len < 0 {
            if !out_data.is_null() {
                super::mimi_free(out_data.cast());
            }
            return Err("ordinary Map GetStringResult returned invalid pointer/length".into());
        }
        let Ok(length) = usize::try_from(out_len) else {
            super::mimi_free(out_data.cast());
            return Err("ordinary Map GetStringResult output length exceeds address space".into());
        };
        if length > isize::MAX as usize {
            super::mimi_free(out_data.cast());
            return Err("ordinary Map GetStringResult output length exceeds address space".into());
        }
        // SAFETY: OK status returns a registered owned allocation of `length` readable bytes.
        let bytes = unsafe { std::slice::from_raw_parts(out_data.cast::<u8>(), length) };
        let result = std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| "ordinary Map GetStringResult returned non-UTF-8 bytes".to_string());
        super::mimi_free(out_data.cast());
        result
    }

    /// Derive a new root with one canonical Mimi String value. The source root,
    /// key, and value stay borrowed for this call; the V2 clone glue makes a
    /// private C-allocated StringBox for the new Any owner. Only exact
    /// `(status=2, out=0)` is recoverable Capacity. Empty Strings use
    /// `{null, 0}`; non-empty Strings are copied by explicit UTF-8 byte length,
    /// so embedded NUL bytes are retained without relying on C-string scans.
    /// The cloned payload is released with its paired C `free` glue when its
    /// Map owners retire. A String returned separately by GetStringResult is
    /// registered in Mimi's String domain and must be released with
    /// `mimi_free`, never with the payload glue.
    pub(crate) fn try_set_string(
        &self,
        key: &str,
        value: &str,
        type_id: &[u8],
        descriptor_hash: &[u8; 32],
    ) -> Result<Self, OwnedAnyV2RootSetError> {
        let invalid_input = || OwnedAnyV2RootSetError::UnexpectedSetResult {
            status: super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_INVALID_INPUT,
            published_handle: false,
        };
        let Ok(key_len) = i64::try_from(key.len()) else {
            return Err(invalid_input());
        };
        let Ok(value_len) = i64::try_from(value.len()) else {
            return Err(invalid_input());
        };
        let Ok(type_id_len) = i64::try_from(type_id.len()) else {
            return Err(invalid_input());
        };
        if value.len() > isize::MAX as usize {
            return Err(invalid_input());
        }

        let value_box = OwnedAnyV2StringBox {
            data: if value.is_empty() {
                std::ptr::null_mut()
            } else {
                value.as_ptr().cast_mut().cast::<c_char>()
            },
            length: value_len,
        };
        let mut new_handle = 0_i64;
        // SAFETY: self owns a live source root; all input slices remain valid
        // throughout the call; value_box has the exact descriptor layout; the
        // registered clone/drop pair below owns all private payload storage;
        // and new_handle is writable output storage.
        let status = unsafe {
            super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_set_string_result(
                self.handle(),
                key.as_ptr(),
                key_len,
                value_box.data,
                value_len,
                if type_id.is_empty() {
                    std::ptr::null()
                } else {
                    type_id.as_ptr()
                },
                type_id_len,
                descriptor_hash.as_ptr(),
                Some(clone_owned_any_v2_string_box),
                Some(drop_owned_any_v2_string_box),
                &mut new_handle,
            )
        };
        Self::from_set_result(status, new_handle)
    }

    /// Derive a new root by cloning a published, descriptor-pinned V2 Any
    /// token through the generic Set ABI. This borrows the source root and
    /// token; the caller remains responsible for retiring its token exactly
    /// once. Only the generic ManagedAny `Capacity` status 3 with a zero
    /// output is recoverable. Every successful published handle is wrapped
    /// here immediately so VM code never transports an unowned raw root.
    pub(crate) fn try_set_any(
        &self,
        key: &str,
        payload_owner: i64,
        selection_bytes: &[u8],
        selection_hash: &[u8; 32],
    ) -> Result<Self, OwnedAnyV2RootSetError> {
        let invalid_input = || OwnedAnyV2RootSetError::UnexpectedSetResult {
            status: super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_SET_INVALID_INPUT,
            published_handle: false,
        };
        let Ok(key_len) = i64::try_from(key.len()) else {
            return Err(invalid_input());
        };
        let Ok(selection_len) = i64::try_from(selection_bytes.len()) else {
            return Err(invalid_input());
        };
        let mut new_handle = 0_i64;
        // SAFETY: self owns a live source handle; key, selection bytes and
        // selection hash remain readable for this call; payload_owner is an
        // opaque token whose lifetime is managed by the caller; and the
        // output slot is writable and disjoint from every input.
        let status = unsafe {
            super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_set(
                self.handle(),
                key.as_ptr(),
                key_len,
                payload_owner,
                if selection_bytes.is_empty() {
                    std::ptr::null()
                } else {
                    selection_bytes.as_ptr()
                },
                selection_len,
                selection_hash.as_ptr(),
                &mut new_handle,
            )
        };
        match (status, new_handle) {
            (super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_OK, handle) if handle != 0 => {
                Ok(Self {
                    handle: Some(handle),
                })
            }
            // The generic ManagedAny Set ABI uses the shared V2 status table,
            // where Capacity is 3 (the typed SetResult ABI uses 2).
            (ORDINARY_MAP_OWNED_ANY_V2_GENERIC_SET_CAPACITY, 0) => {
                Err(OwnedAnyV2RootSetError::Capacity)
            }
            (status, handle) => {
                let published_handle = handle != 0;
                if published_handle {
                    Self::retire_unpublished_handle(handle);
                }
                Err(OwnedAnyV2RootSetError::UnexpectedSetResult {
                    status,
                    published_handle,
                })
            }
        }
    }

    /// Convert one helper status/output pair into a unique root owner,
    /// retiring any unexpectedly published handle before exposing failure.
    fn from_set_result(status: i32, handle: i64) -> Result<Self, OwnedAnyV2RootSetError> {
        match (status, handle) {
            (status, handle)
                if status == super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_OK
                    && handle != 0 =>
            {
                Ok(Self {
                    handle: Some(handle),
                })
            }
            (super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_CAPACITY, 0) => {
                Err(OwnedAnyV2RootSetError::Capacity)
            }
            (status, handle) => {
                let published_handle = handle != 0;
                if published_handle {
                    Self::retire_unpublished_handle(handle);
                }
                Err(OwnedAnyV2RootSetError::UnexpectedSetResult {
                    status,
                    published_handle,
                })
            }
        }
    }

    /// Explicitly retire this owner. On failure the returned value retains
    /// ownership, allowing a caller to report the invariant error without
    /// losing the exact handle; dropping that returned owner is the unwind
    /// fallback and aborts if exact retirement still fails.
    pub(crate) fn try_drop(mut self) -> Result<(), (Self, OwnedAnyV2RootError)> {
        let handle = self.handle();
        // This wrapper is the unique owner for the exact V2 profile.
        let status = super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_drop(handle);
        if status == Self::OK {
            self.handle = None;
            Ok(())
        } else {
            Err((self, OwnedAnyV2RootError::DropStatus(status)))
        }
    }

    fn retire_unpublished_handle(handle: i64) {
        // A nonzero handle returned by New is an unpublished owner
        // belonging to this exact runtime profile.
        let status = super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_drop(handle);
        if status != Self::OK {
            // Do not return a malformed New result while leaking an owner.
            // This avoids the broader trap cleanup path, which owns more than
            // this one handle.
            std::process::abort();
        }
    }
}

impl Drop for OwnedAnyV2Root {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // This is the wrapper's sole remaining handle, and the exact V2
            // Drop ABI unregisters only this root.
            let status = super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_drop(handle);
            if status != Self::OK {
                // Dropping during unwind must not leave an untracked root or
                // drain unrelated roots from the current execution thread.
                std::process::abort();
            }
        }
    }
}

/// Register process teardown before Mimi main starts executing. Later hooks,
/// such as the structured worker-pool join, run first under POSIX atexit's
/// reverse registration order. This hook only releases empty runtime indexes;
/// live roots or Any owners remain intact and visible to Memcheck.
#[no_mangle]
pub extern "C" fn mimi_mir_ordinary_map_domain_register_shutdown() {
    PROCESS_SHUTDOWN_HOOK.call_once(|| {
        // SAFETY: the callback is a process-lifetime C ABI function. Native
        // canonical code calls this once from main's entry block.
        #[cfg(standalone)]
        let result = unsafe { super::libc::atexit(shutdown_ordinary_map_domain_at_exit) };
        #[cfg(not(standalone))]
        let result = unsafe { libc::atexit(shutdown_ordinary_map_domain_at_exit) };
        if result != 0 {
            // A program must not report a clean native exit if its runtime
            // cannot register the cleanup contract required by this route.
            std::process::abort();
        }
    });
    #[cfg(feature = "ordinary-map-set-result-l3")]
    ORDINARY_MAP_SET_RESULT_L3_SHUTDOWN_REGISTERED.store(true, Ordering::Release);
}

/// Read whether the generated SetResult executable registered the normal
/// ordinary-Map process shutdown hook.
#[cfg(feature = "ordinary-map-set-result-l3")]
pub fn mimi_ordinary_map_set_result_l3_shutdown_registered() -> i32 {
    ORDINARY_MAP_SET_RESULT_L3_SHUTDOWN_REGISTERED.load(Ordering::Acquire) as i32
}

extern "C" fn shutdown_ordinary_map_domain_at_exit() {
    PROCESS_SHUTTING_DOWN.store(true, Ordering::Release);

    // The generated main-entry hook is registered before user instructions;
    // any future/actor join hooks registered while main runs have already
    // completed by the time this callback is reached.
    let _ = any_owner::shutdown_empty_registry_at_exit();
    let _ = clear_empty_registry(&ROOTS);
    let _ = clear_empty_registry(&OWNED_ANY);
    let _ = clear_empty_registry(&OWNED_STRINGS);
}

fn clear_empty_registry<T>(registry: &Mutex<Vec<T>>) -> bool {
    let allocation = {
        let Ok(mut entries) = registry.lock() else {
            return false;
        };
        if !entries.is_empty() {
            return false;
        }
        mem::take(&mut *entries)
    };
    drop(allocation);
    true
}

/// The receipt names the canonical direct String allocation descriptor,
/// never a tagged Any token, borrowed byte slice, or guessed heap address.
const CANONICAL_STRING_DESCRIPTOR_VERSION: u32 = 1;

#[derive(Clone, Copy)]
struct RegisteredString {
    creator: LeaseThreadId,
    allocation_base: usize,
    payload_len: usize,
    descriptor_version: u32,
}

static OWNED_STRINGS: Mutex<Vec<RegisteredString>> = Mutex::new(Vec::new());

fn lock_owned_strings() -> MutexGuard<'static, Vec<RegisteredString>> {
    OWNED_STRINGS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) enum StringRegistrationError {
    Capacity,
    ConflictingReceipt,
}

fn register_owned_string(
    creator: LeaseThreadId,
    allocation_base: usize,
    payload_len: usize,
    fresh_managed_copy: bool,
) -> Result<(), StringRegistrationError> {
    #[cfg(not(any(test, feature = "ordinary-map-set-result-l3")))]
    let _ = fresh_managed_copy;
    if PROCESS_SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(StringRegistrationError::Capacity);
    }
    let mut strings = lock_owned_strings();
    if PROCESS_SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(StringRegistrationError::Capacity);
    }
    if let Some(existing) = strings
        .iter()
        .find(|existing| existing.allocation_base == allocation_base)
    {
        // A move or a Result wrapper can re-receipt the exact same allocation
        // without creating a second owner. A foreign creator or different
        // String layout never authorizes releasing the existing allocation.
        return if existing.creator == creator
            && existing.payload_len == payload_len
            && existing.descriptor_version == CANONICAL_STRING_DESCRIPTOR_VERSION
        {
            Ok(())
        } else {
            Err(StringRegistrationError::ConflictingReceipt)
        };
    }
    #[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
    let force_fresh_capacity =
        fresh_managed_copy && FRESH_STRING_REGISTRATION_FAIL_NEXT.with(|fail| fail.replace(false));
    #[cfg(not(any(test, feature = "ordinary-map-set-result-l3")))]
    let force_fresh_capacity = false;
    #[cfg(test)]
    if REGISTRATION_FAIL.with(std::cell::Cell::get) {
        return Err(StringRegistrationError::Capacity);
    }
    #[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
    if fresh_managed_copy {
        FRESH_STRING_REGISTRATION_ATTEMPTS
            .with(|attempts| attempts.set(attempts.get().saturating_add(1)));
    }
    let additional = if force_fresh_capacity { usize::MAX } else { 1 };
    let reserve = strings.try_reserve(additional);
    #[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
    if force_fresh_capacity && reserve.is_err() {
        FRESH_STRING_REGISTRATION_CAPACITY_FAILURES
            .with(|failures| failures.set(failures.get().saturating_add(1)));
    }
    reserve.map_err(|_| StringRegistrationError::Capacity)?;
    strings.push(RegisteredString {
        creator,
        allocation_base,
        payload_len,
        descriptor_version: CANONICAL_STRING_DESCRIPTOR_VERSION,
    });
    Ok(())
}

/// Register a newly allocated canonical String without consuming it on error.
/// The caller retains its sole allocation owner until this succeeds and frees
/// it after an error, outside OWNED_STRINGS. Used by ManagedAny copy publication.
pub(super) fn register_fresh_owned_string(
    pointer: *mut c_char,
    payload_len: usize,
) -> Result<(), StringRegistrationError> {
    register_owned_string(
        handle::current_lease_thread_id(),
        pointer as usize,
        payload_len,
        true,
    )
}

/// Separate from Map entry owners: only snapshots accepted by a ManagedAny
/// ABI appear here. Records carry the exact v1 or v2 descriptor receipt and
/// never expose/reinterpret the token as a ValueHandle.
#[derive(Clone, Copy)]
enum RegisteredAnyDescriptor {
    V1(DescriptorIdentity),
    V2(ManagedAnyPayloadV2Identity),
}

#[derive(Clone, Copy)]
struct RegisteredAny {
    creator: LeaseThreadId,
    token: AnyOwnerToken,
    descriptor: RegisteredAnyDescriptor,
}

static OWNED_ANY: Mutex<Vec<RegisteredAny>> = Mutex::new(Vec::new());

/// Snapshot of the ownership records used by ordinary-Map native execution.
/// This is test-only and deliberately exposes counts, never handles or owner
/// tokens. This runtime has no separate `VALUES_SHELLS` counter; the
/// underlying AnyOwner token count is reported independently.
#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrdinaryMapDomainLiveCounts {
    /// Active execution-domain root records, one per published profile handle.
    pub roots: usize,
    /// The exact number of dedicated ordinary-Map handles represented by the
    /// root registry (including both supported ordinary-Map profiles).
    pub ordinary_map_handles: usize,
    /// All live Map-table slots, across profiles. This is the exact runtime
    /// handle count, though the current test hook does not partition by profile.
    pub runtime_map_handles: usize,
    /// Roots belonging specifically to the owned-Get profile under test.
    pub owned_get_handles: usize,
    /// ManagedAny receipt records in the execution-domain registry.
    pub managed_any: usize,
    /// Canonical owned String receipt records in the execution-domain registry.
    pub owned_strings: usize,
    /// Live generation-bearing tokens in the underlying AnyOwner table.
    pub any_owner_tokens: usize,
}

/// Read a stable test snapshot of ordinary-Map ownership domains. Callers
/// compare before/after values around serial native execution; unrelated
/// runtime registrations may already exist in the process.
#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
pub fn ordinary_map_domain_live_counts_for_test(
) -> Result<OrdinaryMapDomainLiveCounts, &'static str> {
    // Copy identities and release ROOTS before observing other registries.
    // The test call sites sample only between synchronous operations, so this
    // avoids lock-order coupling while retaining delta-based assertions.
    let roots = lock_roots().clone();
    let managed_any = OWNED_ANY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    let owned_strings = lock_owned_strings().len();
    let any_owner_tokens = any_owner::live_count().map_err(|_| "AnyOwner registry is unhealthy")?;
    let runtime_map_handles = usize::try_from(handle::mimi_test_map_live_count())
        .map_err(|_| "Map handle count is negative")?;
    let owned_get_handles = roots
        .iter()
        .filter(|root| root.profile == OrdinaryMapProfile::OwnedGetI32StringV1)
        .count();

    Ok(OrdinaryMapDomainLiveCounts {
        roots: roots.len(),
        ordinary_map_handles: roots.len(),
        runtime_map_handles,
        owned_get_handles,
        managed_any,
        owned_strings,
        any_owner_tokens,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManagedAnyDomainError {
    Capacity,
    ConflictingReceipt,
    Poisoned,
}

/// Accept an unpublished, independently owned token. On error ownership stays
/// with the staging RAII wrapper. No clone or destructor runs under this lock.
pub(super) fn register_fresh_managed_any(
    token: AnyOwnerToken,
    descriptor: DescriptorIdentity,
) -> Result<(), ManagedAnyDomainError> {
    if PROCESS_SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(ManagedAnyDomainError::Capacity);
    }
    let creator = handle::current_lease_thread_id();
    let mut owners = OWNED_ANY
        .lock()
        .map_err(|_| ManagedAnyDomainError::Poisoned)?;
    if PROCESS_SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(ManagedAnyDomainError::Capacity);
    }
    if descriptor.version != 1 || owners.iter().any(|owner| owner.token == token) {
        return Err(ManagedAnyDomainError::ConflictingReceipt);
    }
    #[cfg(test)]
    if REGISTRATION_FAIL.with(std::cell::Cell::get) {
        return Err(ManagedAnyDomainError::Capacity);
    }
    owners
        .try_reserve(1)
        .map_err(|_| ManagedAnyDomainError::Capacity)?;
    owners.push(RegisteredAny {
        creator,
        token,
        descriptor: RegisteredAnyDescriptor::V1(descriptor),
    });
    Ok(())
}

/// Register a fresh V2 token for creator-thread trap cleanup. The identity is
/// copied from the owner before this call and no destructor runs under lock.
pub(super) fn register_fresh_managed_any_v2(
    token: AnyOwnerToken,
    descriptor: ManagedAnyPayloadV2Identity,
) -> Result<(), ManagedAnyDomainError> {
    if PROCESS_SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(ManagedAnyDomainError::Capacity);
    }
    let creator = handle::current_lease_thread_id();
    let mut owners = OWNED_ANY
        .lock()
        .map_err(|_| ManagedAnyDomainError::Poisoned)?;
    if PROCESS_SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(ManagedAnyDomainError::Capacity);
    }
    if owners.iter().any(|owner| owner.token == token) {
        return Err(ManagedAnyDomainError::ConflictingReceipt);
    }
    #[cfg(test)]
    if REGISTRATION_FAIL.with(std::cell::Cell::get) {
        return Err(ManagedAnyDomainError::Capacity);
    }
    owners
        .try_reserve(1)
        .map_err(|_| ManagedAnyDomainError::Capacity)?;
    owners.push(RegisteredAny {
        creator,
        token,
        descriptor: RegisteredAnyDescriptor::V2(descriptor),
    });
    Ok(())
}

/// A domain record authorizes this token, but never pins its payload. The owner
/// table must acquire and validate it once together with the expected receipt.
pub(super) fn managed_any_descriptor(
    token: AnyOwnerToken,
) -> Result<Option<DescriptorIdentity>, ManagedAnyDomainError> {
    let owners = OWNED_ANY
        .lock()
        .map_err(|_| ManagedAnyDomainError::Poisoned)?;
    Ok(owners.iter().find_map(|owner| {
        if owner.token != token {
            return None;
        }
        match owner.descriptor {
            RegisteredAnyDescriptor::V1(descriptor) => Some(descriptor),
            RegisteredAnyDescriptor::V2(_) => None,
        }
    }))
}

pub(super) fn managed_any_v2_identity(
    token: AnyOwnerToken,
) -> Result<Option<ManagedAnyPayloadV2Identity>, ManagedAnyDomainError> {
    let owners = OWNED_ANY
        .lock()
        .map_err(|_| ManagedAnyDomainError::Poisoned)?;
    Ok(owners.iter().find_map(|owner| {
        if owner.token != token {
            return None;
        }
        match owner.descriptor {
            RegisteredAnyDescriptor::V1(_) => None,
            RegisteredAnyDescriptor::V2(descriptor) => Some(descriptor),
        }
    }))
}

/// After successful owner-table release, remove the exact token's original
/// creator association, including on a foreign freeing thread. Recovering a
/// poisoned identity-only domain here cannot turn a completed release into a
/// leak or replace a shutdown diagnostic.
pub(super) fn forget_managed_any(token: AnyOwnerToken) {
    let mut owners = OWNED_ANY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(index) = owners.iter().position(|owner| owner.token == token) {
        owners.remove(index);
    }
}

fn extract_managed_any_for(creator: LeaseThreadId) -> Option<RegisteredAny> {
    let mut owners = OWNED_ANY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let index = owners.iter().rposition(|owner| owner.creator == creator)?;
    Some(owners.remove(index))
}

/// Accept a fresh canonical String owner for this creating thread's ordinary
/// Map execution domain. Return 1 on success, including an allocation-free
/// `(NULL, 0)` receipt and an exact repeated receipt on the same thread.
/// Return 0 on failure. Capacity failure consumes and frees the fresh String
/// outside the registry lock; invalid lengths and conflicting receipts do
/// not accept ownership or free the allocation.
///
/// Only a native consumer's exact canonical String TypeDesc receipt in a
/// program containing an ordinary Map profile may call this entry point.
/// An arbitrary pointer's shape is not proof of allocation provenance.
///
/// # Safety
/// A non-null `pointer` must be the live base returned by `mimi_alloc` for a
/// canonical owned String, never an interior pointer or borrowed/literal
/// bytes. For a new valid receipt the caller transfers that sole allocation
/// owner; `length` is the exact readable payload length. An already registered
/// live allocation may be re-receipted, with success restricted to its creator
/// and exact length. Invalid length receipts retain the caller's ownership.
/// NULL is an empty unallocated String only when `length == 0`.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_ordinary_map_domain_own_string(
    pointer: *mut c_char,
    length: i64,
) -> i32 {
    if pointer.is_null() {
        return i32::from(length == 0);
    }
    let Ok(payload_len) = usize::try_from(length) else {
        return 0;
    };
    let allocation_base = pointer as usize;
    if payload_len > isize::MAX as usize || allocation_base.checked_add(payload_len).is_none() {
        return 0;
    }
    let creator = handle::current_lease_thread_id();
    match register_owned_string(creator, allocation_base, payload_len, false) {
        Ok(()) => 1,
        Err(StringRegistrationError::Capacity) => {
            // The fresh allocation's typed receipt is proven by the caller.
            // Registration has returned and released OWNED_STRINGS; the
            // normal free hook can therefore safely reenter forget below.
            super::mimi_free(pointer.cast());
            0
        }
        Err(StringRegistrationError::ConflictingReceipt) => 0,
    }
}

/// Called immediately before normal `mimi_free` releases an allocation.
/// Pointer association locates its original creator even on a different
/// freeing thread. This only forgets an accepted String receipt; ownership
/// release remains the caller's responsibility and never occurs under lock.
pub(super) fn forget_owned_string(pointer: *mut c_char) {
    if pointer.is_null() {
        return;
    }
    let allocation_base = pointer as usize;
    let mut strings = lock_owned_strings();
    if let Some(index) = strings
        .iter()
        .position(|string| string.allocation_base == allocation_base)
    {
        strings.remove(index);
    }
}

fn extract_owned_string_for(creator: LeaseThreadId) -> Option<RegisteredString> {
    let mut strings = lock_owned_strings();
    let index = strings
        .iter()
        .rposition(|string| string.creator == creator)?;
    Some(strings.remove(index))
}

fn lock_roots() -> MutexGuard<'static, Vec<RegisteredRoot>> {
    ROOTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn register_root(
    creator: LeaseThreadId,
    profile: OrdinaryMapProfile,
    handle: i64,
) -> Result<(), OrdinaryMapDomainError> {
    if PROCESS_SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(OrdinaryMapDomainError::Capacity);
    }
    let mut roots = lock_roots();
    if PROCESS_SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(OrdinaryMapDomainError::Capacity);
    }
    // In tests, registration failure is injected by requesting an impossible
    // capacity from the same fallible Vec primitive used in production. This
    // exercises the real ReserveError path after the handle has been
    // installed, so publish_new_root must retire the unpublished handle.
    #[cfg(test)]
    let additional = if REGISTRATION_FAIL.with(std::cell::Cell::get)
        || ROOT_RESERVE_OVERFLOW_FAIL.with(std::cell::Cell::get)
    {
        usize::MAX
    } else {
        1
    };
    #[cfg(not(test))]
    let additional = 1;

    #[cfg(test)]
    ROOT_RESERVE_ATTEMPTS.with(|attempts| attempts.set(attempts.get().saturating_add(1)));
    let reserve = roots.try_reserve(additional);
    #[cfg(test)]
    if additional == usize::MAX && reserve.is_err() {
        ROOT_RESERVE_OVERFLOW_OBSERVED.with(|observed| observed.set(true));
    }
    reserve.map_err(|_| OrdinaryMapDomainError::Capacity)?;
    roots.push(RegisteredRoot {
        creator,
        profile,
        handle,
    });
    Ok(())
}

/// Take ownership of a freshly installed, unpublished profile root. A
/// registration-capacity failure retires that root before returning Capacity;
/// the caller must neither publish nor drop a handle returned through Err.
/// The creating thread identity uses the same allocation-free const TLS as
/// operation leases, including when entered from a foreign native thread.
pub(super) fn publish_new_root(
    profile: OrdinaryMapProfile,
    handle: i64,
) -> Result<i64, OrdinaryMapDomainError> {
    let creator = handle::current_lease_thread_id();
    match register_root(creator, profile, handle) {
        Ok(()) => Ok(handle),
        Err(error) => {
            // register_root has returned and released ROOTS before any Map
            // or typed Any owner destructor can execute.
            profile.retire_for_cleanup(handle);
            Err(error)
        }
    }
}

/// Forget a root only after its dedicated raw Drop has succeeded. Look up
/// the creator through the handle association instead of the calling thread,
/// so cross-thread normal Drop cannot leave a stale creator registration.
/// A missing record is a no-op, including after a prior trap drain.
pub(super) fn forget_root(profile: OrdinaryMapProfile, handle: i64) {
    let mut roots = lock_roots();
    if let Some(index) = roots
        .iter()
        .position(|root| root.profile == profile && root.handle == handle)
    {
        roots.remove(index);
    }
}

fn extract_root_for(creator: LeaseThreadId) -> Option<RegisteredRoot> {
    let mut roots = lock_roots();
    let index = roots.iter().rposition(|root| root.creator == creator)?;
    // remove preserves publication order for the other threads; shifting
    // these Copy-only identity records cannot allocate or run user Drop.
    Some(roots.remove(index))
}

thread_local! {
    // Allocation-free scope state: root RAII destructors may see stale or
    // poisoned secondary errors while the original trap already owns control.
    static TRAP_CLEANUP_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) fn in_trap_cleanup() -> bool {
    TRAP_CLEANUP_ACTIVE.with(std::cell::Cell::get)
}

/// Copy an exact canonical i32 from a live ManagedAny-v2 token without
/// consuming that token. The VM keeps the token as the owner for its
/// Checker-receipted GetAny pair until Drop.
pub(crate) fn unpack_managed_any_v2_i32(token: i64) -> Result<i32, i32> {
    let mut output = std::mem::MaybeUninit::<i32>::uninit();
    // SAFETY: the token is retained by its VM-side owner backing, and the
    // output slot has the exact canonical i32 size/alignment.
    let status = unsafe {
        super::map_any_snapshot::mimi_any_payload_v2_unpack(
            token,
            1,
            super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.as_ptr(),
            super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.len() as i64,
            super::managed_any_payload_v2::CANONICAL_I32_TYPE_DESC_HASH.as_ptr(),
            output.as_mut_ptr().cast::<u8>(),
            std::mem::size_of::<i32>() as i64,
            std::mem::align_of::<i32>() as i64,
        )
    };
    if status == 0 {
        // SAFETY: the exact descriptor was accepted and status zero means the
        // runtime initialized the full i32 output slot.
        Ok(unsafe { output.assume_init() })
    } else {
        Err(status)
    }
}

/// Retire a VM-owned ManagedAny-v2 token through the same runtime domain used
/// by native GetAny. A failed release is an ownership invariant violation.
pub(crate) fn drop_managed_any_v2_token(token: i64) -> i32 {
    super::map_any_snapshot::mimi_any_payload_v2_drop(token)
}

struct TrapCleanupScope(bool);

impl TrapCleanupScope {
    fn enter() -> Self {
        Self(TRAP_CLEANUP_ACTIVE.with(|active| active.replace(true)))
    }
}

impl Drop for TrapCleanupScope {
    fn drop(&mut self) {
        TRAP_CLEANUP_ACTIVE.with(|active| active.set(self.0));
    }
}

/// Drain the trapping native thread's execution roots without allocation.
/// No new identity is allocated for a thread that has never owned a lease or
/// root. Each record is detached before raw retirement, so cleanup runs with
/// no domain lock held and repeated cleanup is harmless. Other creators'
/// roots are retained even if their handles were read by this thread. Maps
/// retire first, then any surviving canonical String results and dynamic
/// keys; independent Any snapshots retire between roots and Strings. Normal
/// free callbacks can unregister strings during Map retirement.
pub(super) fn drain_current_thread_roots() {
    let Some(creator) = handle::cached_lease_thread_id() else {
        return;
    };
    let _cleanup_scope = TrapCleanupScope::enter();
    while let Some(root) = extract_root_for(creator) {
        root.profile.retire_for_cleanup(root.handle);
    }
    // A Values result is published as an ordinary MimiList whose slots own
    // independent V2 tokens. Drop these Lists first so their normal list glue
    // releases element owners and their Boxes/backing buffers before the
    // registry sweeps any unrelated standalone V2 results below.
    super::map_any_snapshot::drain_current_thread_managed_any_v2_lists();
    // V2 `values` results are independent owners, not Map roots or V1
    // ManagedAny Lists. Retire their creator-owned snapshot graphs outside the
    // list registry lock before draining separately published Any snapshots.
    super::map_any_snapshot::drain_current_thread_v2_values_snapshots();
    while let Some(owner) = extract_managed_any_for(creator) {
        // Detach first; stale/poisoned/descriptor errors are secondary cleanup
        // failures and must not recurse into the original trap path.
        let _ = match owner.descriptor {
            RegisteredAnyDescriptor::V1(descriptor) => {
                any_owner::release_owner_v1(owner.token, descriptor, 1).map_err(|_| ())
            }
            RegisteredAnyDescriptor::V2(descriptor) => {
                any_owner::release_managed_v2_owner(owner.token, descriptor).map_err(|_| ())
            }
        };
    }
    while let Some(string) = extract_owned_string_for(creator) {
        if string.descriptor_version == CANONICAL_STRING_DESCRIPTOR_VERSION {
            // The typed receipt supplies the original mimi_alloc base. The
            // record has been detached; normal free's forget hook reenters
            // without a live guard or another owner for this allocation.
            super::mimi_free(string.allocation_base as *mut std::ffi::c_void);
        }
    }
}

#[cfg(test)]
thread_local! {
    static REGISTRATION_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ROOT_RESERVE_OVERFLOW_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ROOT_RESERVE_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ROOT_RESERVE_OVERFLOW_OBSERVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "ordinary-map-set-result-l3"))]
thread_local! {
    // The String L3 child can fail the actual fresh-owned-String registry
    // `Vec::try_reserve` call after the final payload copy has been allocated.
    // It is deliberately separate from generic root/Any registration
    // failures and automatically consumes one installed failure.
    static FRESH_STRING_REGISTRATION_FAIL_NEXT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FRESH_STRING_REGISTRATION_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FRESH_STRING_REGISTRATION_CAPACITY_FAILURES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "ordinary-map-set-result-l3")]
pub(super) fn install_fresh_string_registration_capacity_failure_l3() {
    FRESH_STRING_REGISTRATION_FAIL_NEXT.with(|fail| fail.set(true));
}

#[cfg(feature = "ordinary-map-set-result-l3")]
pub(super) fn reset_fresh_string_registration_observation_l3() {
    FRESH_STRING_REGISTRATION_FAIL_NEXT.with(|fail| fail.set(false));
    FRESH_STRING_REGISTRATION_ATTEMPTS.with(|attempts| attempts.set(0));
    FRESH_STRING_REGISTRATION_CAPACITY_FAILURES.with(|failures| failures.set(0));
}

#[cfg(feature = "ordinary-map-set-result-l3")]
pub(super) fn fresh_string_registration_observation_l3() -> (usize, usize) {
    (
        FRESH_STRING_REGISTRATION_ATTEMPTS.with(std::cell::Cell::get),
        FRESH_STRING_REGISTRATION_CAPACITY_FAILURES.with(std::cell::Cell::get),
    )
}

/// Fail fresh root and String registrations in this calling-thread scope, restoring the
/// previous failpoint even after unwinding. Construction and owner staging
/// still complete, exposing the installed-but-unpublished rollback boundary.
#[cfg(test)]
pub(crate) fn with_registration_failure_for_test<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            REGISTRATION_FAIL.with(|fail| fail.set(self.0));
        }
    }
    let _restore = Restore(REGISTRATION_FAIL.with(|fail| fail.replace(true)));
    operation()
}

/// Inject a failure specifically through the root registry's real
/// `Vec::try_reserve` operation. The impossible capacity is test-only; callers
/// still traverse the production New ABI, handle installation, publication,
/// ReserveError conversion, and unpublished-root retirement.
#[cfg(test)]
pub(crate) fn with_root_reserve_overflow_for_test<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore {
        fail: bool,
        attempts: usize,
        observed: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            ROOT_RESERVE_OVERFLOW_FAIL.with(|value| value.set(self.fail));
            ROOT_RESERVE_ATTEMPTS.with(|value| value.set(self.attempts));
            ROOT_RESERVE_OVERFLOW_OBSERVED.with(|value| value.set(self.observed));
        }
    }
    let _restore = Restore {
        fail: ROOT_RESERVE_OVERFLOW_FAIL.with(|value| value.replace(true)),
        attempts: ROOT_RESERVE_ATTEMPTS.with(|value| value.replace(0)),
        observed: ROOT_RESERVE_OVERFLOW_OBSERVED.with(|value| value.replace(false)),
    };
    operation()
}

#[cfg(test)]
pub(crate) fn root_reserve_overflow_observation_for_test() -> (usize, bool) {
    (
        ROOT_RESERVE_ATTEMPTS.with(std::cell::Cell::get),
        ROOT_RESERVE_OVERFLOW_OBSERVED.with(std::cell::Cell::get),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::map_any_snapshot::OrdinaryMapSnapshot;

    const PROFILES: [OrdinaryMapProfile; 2] = [
        OrdinaryMapProfile::OwnerLifecycleI32StringV1,
        OrdinaryMapProfile::OwnedGetI32StringV1,
    ];
    const WRAPPER_STRING_TYPE_ID: &[u8; 35] = b"rt:88888888888888888888888888888888";
    const WRAPPER_STRING_DESCRIPTOR_HASH: [u8; 32] = [227; 32];

    fn wrapper_get_string(root: i64, key: &str) -> (i32, *mut c_char, i64) {
        let mut data = std::ptr::null_mut();
        let mut length = -1_i64;
        let status = unsafe {
            crate::runtime::mimi_ordinary_map_owned_any_v2_get_string_result(
                root,
                key.as_ptr(),
                key.len() as i64,
                WRAPPER_STRING_TYPE_ID.as_ptr(),
                WRAPPER_STRING_TYPE_ID.len() as i64,
                WRAPPER_STRING_DESCRIPTOR_HASH.as_ptr(),
                &mut data,
                &mut length,
            )
        };
        (status, data, length)
    }

    fn install(profile: OrdinaryMapProfile, snapshot: OrdinaryMapSnapshot) -> i64 {
        let map = match profile {
            OrdinaryMapProfile::OwnerLifecycleI32StringV1 => {
                super::super::MimiMap::ordinary_map_owner_lifecycle_i32_string_v1(snapshot)
            }
            OrdinaryMapProfile::OwnedGetI32StringV1 => {
                super::super::MimiMap::ordinary_map_owned_get_i32_string_v1(snapshot)
            }
            OrdinaryMapProfile::AnyOwnerV2Record => {
                super::super::MimiMap::ordinary_map_any_owner_v2_record(
                    super::super::map_any_snapshot::OrdinaryMapAnyOwnerV2Snapshot::new(),
                )
            }
            OrdinaryMapProfile::OwnedAnyV2 => {
                unreachable!("V2 tests install their own descriptor snapshot")
            }
        };
        match handle::map_new_handle_fallible(map) {
            Ok(root) => root,
            Err(_) => panic!("test root installation must succeed"),
        }
    }

    fn new_root(profile: OrdinaryMapProfile) -> i64 {
        let snapshot = OrdinaryMapSnapshot::new()
            .set_string("text", "owned\0值")
            .unwrap()
            .set_i32("count", 7)
            .unwrap();
        publish_new_root(profile, install(profile, snapshot)).unwrap()
    }

    fn acquire(
        profile: OrdinaryMapProfile,
        root: i64,
    ) -> Result<handle::MapLease, handle::HandleError> {
        match profile {
            OrdinaryMapProfile::OwnerLifecycleI32StringV1 => {
                handle::ordinary_map_owner_lifecycle_i32_string_v1_acquire(root)
            }
            OrdinaryMapProfile::OwnedGetI32StringV1 => {
                handle::ordinary_map_owned_get_i32_string_v1_acquire(root)
            }
            OrdinaryMapProfile::AnyOwnerV2Record => {
                handle::ordinary_map_any_owner_v2_record_acquire(root)
            }
            OrdinaryMapProfile::OwnedAnyV2 => handle::ordinary_map_owned_any_v2_acquire(root),
        }
    }

    fn clone_root(profile: OrdinaryMapProfile, root: i64) -> i64 {
        let source = acquire(profile, root).unwrap();
        let snapshot = source
            .ordinary_map_snapshot
            .as_ref()
            .unwrap()
            .clone_root()
            .unwrap();
        drop(source);
        publish_new_root(profile, install(profile, snapshot)).unwrap()
    }

    fn normal_drop(profile: OrdinaryMapProfile, root: i64) {
        match profile {
            OrdinaryMapProfile::OwnerLifecycleI32StringV1 => {
                handle::ordinary_map_owner_lifecycle_i32_string_v1_drop(root)
            }
            OrdinaryMapProfile::OwnedGetI32StringV1 => {
                handle::ordinary_map_owned_get_i32_string_v1_drop(root)
            }
            OrdinaryMapProfile::AnyOwnerV2Record => {
                handle::ordinary_map_any_owner_v2_record_drop(root)
            }
            OrdinaryMapProfile::OwnedAnyV2 => handle::ordinary_map_owned_any_v2_drop(root),
        }
        .unwrap();
        forget_root(profile, root);
    }

    fn registered(profile: OrdinaryMapProfile, handle: i64) -> bool {
        lock_roots()
            .iter()
            .any(|root| root.profile == profile && root.handle == handle)
    }

    fn fresh_string(bytes: &[u8]) -> *mut c_char {
        let pointer = super::super::alloc_c_string_from_bytes(bytes);
        assert!(!pointer.is_null());
        // This existing provenance marker disappears only when mimi_free
        // actually executes, so post-drain assertions never dereference freed
        // memory or infer liveness from a malloc address's appearance.
        super::super::register_any_value_string(pointer, bytes.len());
        pointer
    }

    fn string_allocation_live(pointer: *mut c_char) -> bool {
        super::super::any_value_strings()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&(pointer as usize))
    }

    fn registered_string(pointer: *mut c_char) -> Option<RegisteredString> {
        lock_owned_strings()
            .iter()
            .find(|string| string.allocation_base == pointer as usize)
            .copied()
    }

    #[test]
    fn ordinary_map_new_result_root_owner_wrapper_uses_real_new_size_and_exact_drop_abis() {
        let baseline = ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains before wrapper success");
        let root = OwnedAnyV2Root::try_new().expect("production New ABI creates a root");
        assert_ne!(root.handle(), 0);
        assert_eq!(root.size(), Ok(0));
        let live = ordinary_map_domain_live_counts_for_test()
            .expect("ownership domains with wrapper root");
        assert_eq!(live.roots, baseline.roots + 1);
        assert_eq!(live.runtime_map_handles, baseline.runtime_map_handles + 1);
        assert!(
            root.try_drop().is_ok(),
            "explicit drop retires exactly once"
        );
        assert_eq!(
            ordinary_map_domain_live_counts_for_test().expect("ownership domains after drop"),
            baseline
        );

        let (capacity, reserve_observation) = with_root_reserve_overflow_for_test(|| {
            (
                OwnedAnyV2Root::try_new().err(),
                root_reserve_overflow_observation_for_test(),
            )
        });
        assert_eq!(capacity, Some(OwnedAnyV2RootError::Capacity));
        assert_eq!(reserve_observation, (1, true));
        assert_eq!(
            ordinary_map_domain_live_counts_for_test()
                .expect("ownership domains after real reserve failure"),
            baseline,
            "failed New exposes no root or runtime handle"
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_string_wrapper_copies_utf8_and_keeps_source_live() {
        let source = OwnedAnyV2Root::try_new().expect("production New creates a source root");
        let first = source
            .try_set_string(
                "payload",
                "utf8\0and 雪",
                WRAPPER_STRING_TYPE_ID,
                &WRAPPER_STRING_DESCRIPTOR_HASH,
            )
            .expect("String SetResult creates a new root");
        assert_eq!(source.size(), Ok(0), "Set borrows without consuming source");
        assert_eq!(first.size(), Ok(1));

        let second = first
            .try_set_string(
                "empty",
                "",
                WRAPPER_STRING_TYPE_ID,
                &WRAPPER_STRING_DESCRIPTOR_HASH,
            )
            .expect("empty String uses the same canonical descriptor");
        assert_eq!(first.size(), Ok(1), "second Set leaves its source live");
        assert_eq!(second.size(), Ok(2));

        let (status, payload, payload_len) = wrapper_get_string(second.handle(), "payload");
        assert_eq!(
            status,
            crate::runtime::ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_OK
        );
        assert!(!payload.is_null());
        assert_eq!(payload_len as usize, "utf8\0and 雪".len());
        let (empty_status, empty, empty_len) = wrapper_get_string(second.handle(), "empty");
        assert_eq!(
            empty_status,
            crate::runtime::ORDINARY_MAP_OWNED_ANY_V2_GET_STRING_OK
        );
        assert!(!empty.is_null());
        assert_eq!(empty_len, 0);

        assert!(source.try_drop().is_ok(), "source root retires");
        assert!(first.try_drop().is_ok(), "intermediate root retires");
        assert!(second.try_drop().is_ok(), "result root retires");

        // GetString returns a separately registered canonical String. It stays
        // readable after every Map root and Any payload has been retired.
        let payload_bytes =
            unsafe { std::slice::from_raw_parts(payload.cast::<u8>(), payload_len as usize) };
        assert_eq!(payload_bytes, "utf8\0and 雪".as_bytes());
        assert!(std::str::from_utf8(payload_bytes).is_ok());
        assert!(unsafe { std::slice::from_raw_parts(empty.cast::<u8>(), 0) }.is_empty());
        crate::runtime::mimi_free(payload.cast());
        crate::runtime::mimi_free(empty.cast());
    }

    #[test]
    fn ordinary_map_owned_any_v2_string_wrapper_maps_only_exact_capacity_and_cleans_unknown_output()
    {
        let source = OwnedAnyV2Root::try_new().expect("production New creates a source root");
        let capacity =
            crate::runtime::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
                source
                    .try_set_string(
                        "payload",
                        "value",
                        WRAPPER_STRING_TYPE_ID,
                        &WRAPPER_STRING_DESCRIPTOR_HASH,
                    )
                    .err()
            });
        assert_eq!(capacity, Some(OwnedAnyV2RootSetError::Capacity));
        assert_eq!(source.size(), Ok(0), "capacity leaves the source unchanged");
        assert!(source.try_drop().is_ok(), "source root retires");

        let orphan = OwnedAnyV2Root::try_new().expect("production New creates a test root");
        let orphan_handle = orphan.handle();
        std::mem::forget(orphan);
        assert_eq!(
            OwnedAnyV2Root::from_set_result(777, orphan_handle).err(),
            Some(OwnedAnyV2RootSetError::UnexpectedSetResult {
                status: 777,
                published_handle: true,
            }),
            "unknown status must fail closed and retire its unexpected output"
        );
        let mut size = -1_i32;
        let stale_status = unsafe {
            super::super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_size(
                orphan_handle,
                &mut size,
            )
        };
        assert_ne!(
            stale_status,
            super::super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_OK
        );

        let malformed_capacity =
            OwnedAnyV2Root::try_new().expect("production New creates another test root");
        let malformed_handle = malformed_capacity.handle();
        std::mem::forget(malformed_capacity);
        assert_eq!(
            OwnedAnyV2Root::from_set_result(
                crate::runtime::ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_CAPACITY,
                malformed_handle,
            )
            .err(),
            Some(OwnedAnyV2RootSetError::UnexpectedSetResult {
                status: crate::runtime::ORDINARY_MAP_OWNED_ANY_V2_SET_STRING_CAPACITY,
                published_handle: true,
            }),
            "capacity with a published handle is malformed, not recoverable"
        );
        let malformed_status = unsafe {
            super::super::map_any_snapshot::mimi_ordinary_map_owned_any_v2_size(
                malformed_handle,
                &mut size,
            )
        };
        assert_ne!(
            malformed_status,
            super::super::map_any_snapshot::ORDINARY_MAP_OWNED_ANY_V2_OK
        );
    }

    #[test]
    fn ordinary_map_domain_tracks_both_profiles_and_normal_drop_unregisters() {
        for profile in PROFILES {
            let root = new_root(profile);
            assert!(registered(profile, root));
            normal_drop(profile, root);
            assert!(!registered(profile, root));
            assert!(acquire(profile, root).is_err());
        }
    }

    #[test]
    fn ordinary_map_domain_clone_cleanup_survives_either_normal_drop_order() {
        for profile in PROFILES {
            for source_first in [false, true] {
                let source = new_root(profile);
                let cloned = clone_root(profile, source);
                let (first, remaining) = if source_first {
                    (source, cloned)
                } else {
                    (cloned, source)
                };
                normal_drop(profile, first);
                assert_eq!(
                    acquire(profile, remaining)
                        .unwrap()
                        .ordinary_map_snapshot
                        .as_ref()
                        .unwrap()
                        .size(),
                    2
                );
                drain_current_thread_roots();
                assert!(acquire(profile, remaining).is_err());
                assert!(!registered(profile, remaining));
            }
        }
    }

    #[test]
    fn ordinary_map_domain_cleanup_retires_all_roots_and_is_idempotent() {
        let first = new_root(PROFILES[0]);
        let cloned = clone_root(PROFILES[0], first);
        let other = new_root(PROFILES[1]);
        drain_current_thread_roots();
        drain_current_thread_roots();
        for (profile, root) in [
            (PROFILES[0], first),
            (PROFILES[0], cloned),
            (PROFILES[1], other),
        ] {
            assert!(!registered(profile, root));
            assert!(acquire(profile, root).is_err());
        }
    }

    #[test]
    fn ordinary_map_domain_current_thread_cleanup_preserves_other_creators() {
        let (published_tx, published_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let roots = [new_root(PROFILES[0]), new_root(PROFILES[1])];
            published_tx.send(roots).unwrap();
            resume_rx.recv().unwrap();
            for (profile, root) in PROFILES.into_iter().zip(roots) {
                assert!(acquire(profile, root).is_ok());
            }
            drain_current_thread_roots();
            roots
        });
        let own_roots = [new_root(PROFILES[0]), new_root(PROFILES[1])];
        let worker_roots = published_rx.recv().unwrap();
        drain_current_thread_roots();
        for (profile, root) in PROFILES.into_iter().zip(own_roots) {
            assert!(acquire(profile, root).is_err());
        }
        for (profile, root) in PROFILES.into_iter().zip(worker_roots) {
            assert!(acquire(profile, root).is_ok());
            assert!(registered(profile, root));
        }
        resume_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), worker_roots);
        for (profile, root) in PROFILES.into_iter().zip(worker_roots) {
            assert!(acquire(profile, root).is_err());
            assert!(!registered(profile, root));
        }
    }

    #[test]
    fn ordinary_map_domain_cross_thread_normal_drop_forgets_original_creator() {
        for profile in PROFILES {
            let root = new_root(profile);
            std::thread::spawn(move || normal_drop(profile, root))
                .join()
                .unwrap();
            assert!(!registered(profile, root));
            drain_current_thread_roots();
            assert!(acquire(profile, root).is_err());
        }
    }

    #[test]
    fn ordinary_map_domain_registration_failure_retires_unpublished_roots() {
        for profile in PROFILES {
            let existing = new_root(profile);
            let snapshot = OrdinaryMapSnapshot::new()
                .set_string("new", "rollback")
                .unwrap();
            let unpublished = install(profile, snapshot);
            assert_eq!(
                with_registration_failure_for_test(|| publish_new_root(profile, unpublished)),
                Err(OrdinaryMapDomainError::Capacity)
            );
            assert!(!registered(profile, unpublished));
            assert!(acquire(profile, unpublished).is_err());
            assert!(acquire(profile, existing).is_ok());
            // The scoped failure must not poison later publication.
            let later = new_root(profile);
            normal_drop(profile, existing);
            normal_drop(profile, later);
        }
    }

    #[test]
    fn ordinary_map_domain_string_null_empty_receipt_does_not_create_thread_identity() {
        std::thread::spawn(|| {
            assert!(handle::cached_lease_thread_id().is_none());
            assert_eq!(
                unsafe { mimi_mir_ordinary_map_domain_own_string(std::ptr::null_mut(), 0) },
                1
            );
            assert_eq!(
                unsafe { mimi_mir_ordinary_map_domain_own_string(std::ptr::null_mut(), 1) },
                0
            );
            assert_eq!(
                unsafe { mimi_mir_ordinary_map_domain_own_string(std::ptr::null_mut(), -1) },
                0
            );
            assert!(handle::cached_lease_thread_id().is_none());
            drain_current_thread_roots();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn ordinary_map_domain_string_receipt_is_idempotent_and_checks_exact_length() {
        let bytes = b"receipt\0bytes";
        let pointer = fresh_string(bytes);
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(pointer, bytes.len() as i64) },
            1
        );
        let descriptor = registered_string(pointer).unwrap();
        assert_eq!(descriptor.payload_len, bytes.len());
        assert_eq!(
            descriptor.descriptor_version,
            CANONICAL_STRING_DESCRIPTOR_VERSION
        );
        assert_eq!(
            with_registration_failure_for_test(|| unsafe {
                mimi_mir_ordinary_map_domain_own_string(pointer, bytes.len() as i64)
            }),
            1
        );
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(pointer, bytes.len() as i64 + 1) },
            0
        );
        assert_eq!(
            lock_owned_strings()
                .iter()
                .filter(|string| string.allocation_base == pointer as usize)
                .count(),
            1
        );
        assert!(string_allocation_live(pointer));
        super::super::mimi_free(pointer.cast());
        assert!(registered_string(pointer).is_none());
        assert!(!string_allocation_live(pointer));
        drain_current_thread_roots();
    }

    #[test]
    fn ordinary_map_domain_string_invalid_length_retains_unaccepted_fresh_owner() {
        let pointer = fresh_string(b"owned");
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(pointer, -1) },
            0
        );
        assert!(registered_string(pointer).is_none());
        assert!(string_allocation_live(pointer));
        // An invalid length receipt never proves ownership transfer; its
        // original caller still releases the real fresh allocation.
        super::super::mimi_free(pointer.cast());
        assert!(!string_allocation_live(pointer));
    }

    #[test]
    fn ordinary_map_domain_getter_string_and_clone_survive_map_destroy_and_normal_free() {
        let profile = OrdinaryMapProfile::OwnedGetI32StringV1;
        let root = new_root(profile);
        let mut pointer = std::ptr::null_mut();
        let mut length = 0;
        assert_eq!(
            unsafe {
                super::super::map_any_snapshot::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                root, b"text".as_ptr(), 4, &mut pointer, &mut length,
            )
            },
            super::super::map_any_snapshot::ORDINARY_MAP_OWNED_GET_OK
        );
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(pointer, length) },
            1
        );
        let expected = "owned\0值".as_bytes();
        super::super::register_any_value_string(pointer, expected.len());
        let cloned = fresh_string(expected);
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(cloned, expected.len() as i64) },
            1
        );
        normal_drop(profile, root);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) },
            expected
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(cloned.cast::<u8>(), expected.len()) },
            expected
        );
        super::super::mimi_free(pointer.cast());
        assert!(registered_string(pointer).is_none());
        assert!(!string_allocation_live(pointer));
        assert!(string_allocation_live(cloned));
        drain_current_thread_roots();
        assert!(registered_string(cloned).is_none());
        assert!(!string_allocation_live(cloned));
        drain_current_thread_roots();
    }

    #[test]
    fn ordinary_map_domain_string_registration_failure_frees_fresh_owner() {
        let existing = fresh_string(b"retained");
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(existing, 8) },
            1
        );
        let unpublished = fresh_string(b"rollback");
        assert_eq!(
            with_registration_failure_for_test(|| unsafe {
                mimi_mir_ordinary_map_domain_own_string(unpublished, 8)
            }),
            0
        );
        assert!(registered_string(unpublished).is_none());
        assert!(!string_allocation_live(unpublished));
        assert!(registered_string(existing).is_some());
        assert!(string_allocation_live(existing));
        super::super::mimi_free(existing.cast());
    }

    #[test]
    fn ordinary_map_domain_string_cleanup_preserves_other_creator_and_rejects_foreign_receipt() {
        let (published_tx, published_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let pointer = fresh_string(b"foreign");
            assert_eq!(
                unsafe { mimi_mir_ordinary_map_domain_own_string(pointer, 7) },
                1
            );
            published_tx.send(pointer as usize).unwrap();
            resume_rx.recv().unwrap();
            assert!(string_allocation_live(pointer));
            assert!(registered_string(pointer).is_some());
            drain_current_thread_roots();
            assert!(!string_allocation_live(pointer));
            assert!(registered_string(pointer).is_none());
        });
        let pointer = fresh_string(b"local");
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(pointer, 5) },
            1
        );
        let foreign = published_rx.recv().unwrap() as *mut c_char;
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(foreign, 7) },
            0
        );
        assert!(string_allocation_live(foreign));
        drain_current_thread_roots();
        assert!(!string_allocation_live(pointer));
        assert!(string_allocation_live(foreign));
        resume_tx.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn ordinary_map_domain_cross_thread_string_free_forgets_original_creator() {
        let pointer = fresh_string(b"cross-thread");
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(pointer, 12) },
            1
        );
        let allocation_base = pointer as usize;
        std::thread::spawn(move || {
            super::super::mimi_free(allocation_base as *mut std::ffi::c_void);
        })
        .join()
        .unwrap();
        assert!(registered_string(pointer).is_none());
        assert!(!string_allocation_live(pointer));
        drain_current_thread_roots();
    }

    #[test]
    fn ordinary_map_domain_scalar_callee_cleanup_keeps_original_diagnostic() {
        fn scalar_callee_failure(diagnostic: &'static str) -> &'static str {
            drain_current_thread_roots();
            diagnostic
        }
        let root = new_root(OrdinaryMapProfile::OwnerLifecycleI32StringV1);
        // A previously retired handle exercises ignored cleanup errors; its
        // stale record must not replace the scalar callee's original trap.
        handle::ordinary_map_owner_lifecycle_i32_string_v1_drop(root).unwrap();
        let retained = fresh_string(b"retained-result");
        let dynamic_key = fresh_string(b"dynamic-key");
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(retained, 15) },
            1
        );
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(dynamic_key, 11) },
            1
        );
        let diagnostic = "[E0802] original scalar callee overflow";
        assert_eq!(scalar_callee_failure(diagnostic), diagnostic);
        assert!(!registered(
            OrdinaryMapProfile::OwnerLifecycleI32StringV1,
            root
        ));
        for pointer in [retained, dynamic_key] {
            assert!(registered_string(pointer).is_none());
            assert!(!string_allocation_live(pointer));
        }
    }

    fn managed_snapshot(root: i64) -> AnyOwnerToken {
        let (mut found, mut owner) = (0, 0);
        assert_eq!(
            unsafe {
                super::super::map_any_snapshot::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any(
                root, b"text".as_ptr(), 4, &mut found, &mut owner,
            )
            },
            0
        );
        assert_eq!(found, 1);
        AnyOwnerToken::from_wire_bits(owner as u64).unwrap()
    }

    fn managed_copy(owner: AnyOwnerToken) -> *mut c_char {
        let (mut pointer, mut length) = (std::ptr::null_mut(), 0);
        assert_eq!(
            unsafe {
                super::super::map_any_snapshot::mimi_mir_managed_any_i32_string_v1_copy_string(
                    owner.wire_bits() as i64,
                    2,
                    1,
                    &mut pointer,
                    &mut length,
                )
            },
            0
        );
        super::super::register_any_value_string(pointer, length as usize);
        pointer
    }

    #[test]
    fn managed_any_normal_drop_from_foreign_thread_forgets_original_creator() {
        let root = new_root(OrdinaryMapProfile::OwnedGetI32StringV1);
        let owner = managed_snapshot(root);
        let descriptor = managed_any_descriptor(owner).unwrap().unwrap();
        let creator = OWNED_ANY
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.token == owner)
            .unwrap()
            .creator;
        assert_eq!(creator, handle::current_lease_thread_id());
        let worker = std::thread::spawn(move || {
            assert_ne!(creator, handle::current_lease_thread_id());
            assert_eq!(
                super::super::map_any_snapshot::mimi_mir_managed_any_i32_string_v1_drop(
                    owner.wire_bits() as i64,
                    1
                ),
                0
            );
        });
        worker.join().unwrap();
        assert_eq!(managed_any_descriptor(owner), Ok(None));
        assert_eq!(
            any_owner::descriptor_identity(owner),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        assert_eq!(descriptor.version, 1);
        drain_current_thread_roots();
        assert!(acquire(OrdinaryMapProfile::OwnedGetI32StringV1, root).is_err());
    }

    #[test]
    fn managed_any_domain_trap_drain_settles_any_roots_strings_and_keeps_foreign_creator() {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let root = new_root(OrdinaryMapProfile::OwnedGetI32StringV1);
            let owner = managed_snapshot(root);
            let copied = managed_copy(owner);
            ready_tx.send((root, owner, copied as usize)).unwrap();
            resume_rx.recv().unwrap();
            assert_eq!(any_owner::copy_string(owner), Ok("owned\0值".to_owned()));
            assert!(managed_any_descriptor(owner).unwrap().is_some());
            assert!(registered_string(copied).is_some());
            assert!(string_allocation_live(copied));
            drain_current_thread_roots();
            assert!(managed_any_descriptor(owner).unwrap().is_none());
            assert!(!string_allocation_live(copied));
        });
        let root = new_root(OrdinaryMapProfile::OwnedGetI32StringV1);
        let owner = managed_snapshot(root);
        let copied = managed_copy(owner);
        let (foreign_root, foreign_owner, foreign_copy) = ready_rx.recv().unwrap();
        drain_current_thread_roots();
        drain_current_thread_roots();
        assert!(acquire(OrdinaryMapProfile::OwnedGetI32StringV1, root).is_err());
        assert_eq!(
            any_owner::validate(owner),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        assert!(managed_any_descriptor(owner).unwrap().is_none());
        assert!(!string_allocation_live(copied));
        assert!(registered_string(copied).is_none());
        assert!(acquire(OrdinaryMapProfile::OwnedGetI32StringV1, foreign_root).is_ok());
        assert!(managed_any_descriptor(foreign_owner).unwrap().is_some());
        assert!(string_allocation_live(foreign_copy as *mut c_char));
        resume_tx.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn managed_any_domain_rejects_duplicate_registration_and_ignores_stale_cleanup_record() {
        let root = new_root(OrdinaryMapProfile::OwnedGetI32StringV1);
        let owner = managed_snapshot(root);
        let descriptor = managed_any_descriptor(owner).unwrap().unwrap();
        assert_eq!(
            register_fresh_managed_any(owner, descriptor),
            Err(ManagedAnyDomainError::ConflictingReceipt)
        );
        any_owner::release_owner_v1(owner, descriptor, 1).unwrap();
        // Simulate a stale cleanup identity: the original error still owns the
        // diagnostic and cleanup must not call trapping ABI or release twice.
        drain_current_thread_roots();
        assert_eq!(managed_any_descriptor(owner), Ok(None));
        assert_eq!(
            any_owner::validate(owner),
            Err(any_owner::AnyOwnerError::StaleToken)
        );
        drain_current_thread_roots();
    }

    #[test]
    fn ordinary_map_domain_abort_callback_observes_settled_roots_and_strings_in_child() {
        for (mode, diagnostic) in [
            ("static", "[E0802] original callback diagnostic"),
            ("owned", "owned diagnostic"),
            ("interior", "diagnostic"),
            ("managed", "owned"),
            (
                "get-any-failure",
                "[E0800] ordinary Map/Any owner-v2 GetAny failed",
            ),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::ordinary_map_domain::tests::ordinary_map_domain_abort_callback_subprocess_only",
                    "--ignored",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env("MIMI_ORDINARY_MAP_ABORT_CALLBACK_CHILD", mode)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode} child callback failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
            assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        }
    }

    #[test]
    #[ignore = "isolated callback subprocess helper"]
    fn ordinary_map_domain_abort_callback_subprocess_only() {
        let Ok(mode) = std::env::var("MIMI_ORDINARY_MAP_ABORT_CALLBACK_CHILD") else {
            return;
        };
        #[derive(Clone, Copy)]
        struct CallbackState {
            root: i64,
            v2_root: i64,
            retained: usize,
            dynamic: usize,
            diagnostic: usize,
            mode: u8,
            managed: AnyOwnerToken,
            stale_managed: AnyOwnerToken,
            managed_string: usize,
        }
        thread_local! {
            static CALLBACK_STATE: std::cell::Cell<Option<CallbackState>> = const { std::cell::Cell::new(None) };
        }
        unsafe extern "C" fn callback(message: *const c_char) {
            let Some(CallbackState {
                root,
                v2_root,
                retained,
                dynamic,
                diagnostic,
                mode,
                managed,
                stale_managed,
                managed_string,
            }) = CALLBACK_STATE.with(std::cell::Cell::get)
            else {
                unsafe { libc::_exit(98) };
            };
            let retained = retained as *mut c_char;
            let dynamic = dynamic as *mut c_char;
            let diagnostic = diagnostic as *mut c_char;
            let expected: &[u8] = match mode {
                0 => b"[E0802] original callback diagnostic",
                1 => b"owned diagnostic",
                2 => b"diagnostic",
                3 => b"owned",
                _ => b"[E0800] ordinary Map/Any owner-v2 GetAny failed",
            };
            let original = !message.is_null()
                && unsafe { std::ffi::CStr::from_ptr(message) }.to_bytes() == expected;
            let settled = acquire(OrdinaryMapProfile::OwnedGetI32StringV1, root).is_err()
                && !registered(OrdinaryMapProfile::OwnedGetI32StringV1, root)
                && (v2_root == 0
                    || (acquire(OrdinaryMapProfile::OwnedAnyV2, v2_root).is_err()
                        && !registered(OrdinaryMapProfile::OwnedAnyV2, v2_root)))
                && crate::runtime::any_owner::live_count() == Ok(0)
                && managed_any_descriptor(managed) == Ok(None)
                && managed_any_descriptor(stale_managed) == Ok(None)
                && any_owner::validate(managed) == Err(any_owner::AnyOwnerError::StaleToken)
                && any_owner::validate(stale_managed) == Err(any_owner::AnyOwnerError::StaleToken)
                && registered_string(managed_string as *mut c_char).is_none()
                && !string_allocation_live(managed_string as *mut c_char)
                && registered_string(retained).is_none()
                && !string_allocation_live(retained)
                && registered_string(dynamic).is_none()
                && !string_allocation_live(dynamic)
                && (mode == 0
                    || mode == 3
                    || mode == 4
                    || (registered_string(diagnostic).is_none()
                        && !string_allocation_live(diagnostic)))
                && ordinary_map_domain_live_counts_for_test().is_ok_and(|counts| {
                    // This callback runs in a purpose-built one-test child,
                    // so unlike the in-process JIT assertions its complete
                    // runtime ownership domain must be empty after trap drain.
                    counts == OrdinaryMapDomainLiveCounts::default()
                });
            // Exit from the real callback before trap_exit can run. Success
            // therefore proves that mimi_runtime_abort settled ownership
            // before invoking a callback that transfers control permanently.
            unsafe { libc::_exit(if original && settled { 0 } else { 97 }) };
        }
        let root = new_root(OrdinaryMapProfile::OwnedGetI32StringV1);
        let mut retained = std::ptr::null_mut();
        let mut length = 0;
        assert_eq!(
            unsafe {
                super::super::map_any_snapshot::mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string(
                root, b"text".as_ptr(), 4, &mut retained, &mut length,
            )
            },
            super::super::map_any_snapshot::ORDINARY_MAP_OWNED_GET_OK
        );
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(retained, length) },
            1
        );
        super::super::register_any_value_string(retained, length as usize);
        // Exercise the exact native argv ABI used by DynamicKeysGet. The
        // return from args_get is an independent allocation, so clearing the
        // process argument table must not invalidate its owner before the
        // ordinary-Map domain registers it.
        let executable = std::ffi::CString::new("mimi-test").unwrap();
        let argument = std::ffi::CString::new("dynamic-key").unwrap();
        let mut argv = [
            executable.as_ptr() as *mut c_char,
            argument.as_ptr() as *mut c_char,
        ];
        unsafe { super::super::env::mimi_args_init(argv.len() as i32, argv.as_mut_ptr()) };
        let dynamic = super::super::env::mimi_args_get(0);
        assert!(
            !dynamic.is_null(),
            "native args_get returns an owned argv[0]"
        );
        assert_eq!(
            unsafe { std::ffi::CStr::from_ptr(dynamic) }.to_bytes(),
            b"dynamic-key"
        );
        unsafe { super::super::env::mimi_args_init(0, std::ptr::null_mut()) };
        assert_eq!(
            unsafe { std::ffi::CStr::from_ptr(dynamic) }.to_bytes(),
            b"dynamic-key",
            "the owned argv copy outlives the argument table"
        );
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(dynamic, 11) },
            1
        );
        let mut v2_root = 0;
        if mode == "get-any-failure" {
            assert_eq!(
                unsafe { crate::runtime::mimi_ordinary_map_owned_any_v2_new(&mut v2_root) },
                crate::runtime::ORDINARY_MAP_OWNED_ANY_V2_OK,
                "the failure-injection case has a live, registered V2 Map root"
            );
            let mut found = 0;
            let mut token = 0;
            let status = super::super::map_any_snapshot::with_ordinary_map_owned_any_v2_get_any_publish_failure_for_test(
                0,
                || unsafe {
                    crate::runtime::mimi_ordinary_map_owned_any_v2_get(
                        v2_root,
                        dynamic.cast(),
                        11,
                        &mut found,
                        &mut token,
                        1,
                        super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.as_ptr(),
                        super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_ID.len() as i64,
                        super::super::managed_any_payload_v2::CANONICAL_I32_TYPE_DESC_HASH.as_ptr(),
                        std::mem::size_of::<i32>() as i64,
                        std::mem::align_of::<i32>() as i64,
                        Some(copy_i32_for_domain_abort_test),
                        Some(drop_i32_for_domain_abort_test),
                    )
                },
            );
            assert_ne!(
                status,
                crate::runtime::ORDINARY_MAP_OWNED_ANY_V2_OK,
                "GetAny publication must fail after the argv String has been registered"
            );
            assert_eq!((found, token), (0, 0));
            assert_eq!(
                super::super::map_any_snapshot::ordinary_map_owned_any_v2_get_any_publish_attempts_for_test(),
                1,
                "the later GetAny failure was actually injected"
            );
        }
        let managed = managed_snapshot(root);
        let managed_string = managed_copy(managed);
        let stale_managed = managed_snapshot(root);
        let stale_descriptor = managed_any_descriptor(stale_managed).unwrap().unwrap();
        any_owner::release_owner_v1(stale_managed, stale_descriptor, 1).unwrap();
        let mode = match mode.as_str() {
            "static" => 0,
            "owned" => 1,
            "interior" => 2,
            "managed" => 3,
            "get-any-failure" => 4,
            _ => panic!("unknown callback child mode"),
        };
        let diagnostic = if mode == 0 || mode == 3 || mode == 4 {
            std::ptr::null_mut()
        } else {
            let diagnostic = fresh_string(b"owned diagnostic");
            assert_eq!(
                unsafe { mimi_mir_ordinary_map_domain_own_string(diagnostic, 16) },
                1
            );
            diagnostic
        };
        CALLBACK_STATE.with(|state| {
            state.set(Some(CallbackState {
                root,
                v2_root,
                retained: retained as usize,
                dynamic: dynamic as usize,
                diagnostic: diagnostic as usize,
                mode,
                managed,
                stale_managed,
                managed_string: managed_string as usize,
            }))
        });
        super::super::mimi_runtime_set_error_handler(Some(callback));
        let message = match mode {
            0 => c"[E0802] original callback diagnostic".as_ptr(),
            1 => diagnostic.cast_const(),
            2 => unsafe { diagnostic.add(6) }.cast_const(),
            3 => any_owner::string_payload_pointer_for_test(managed).unwrap(),
            _ => c"[E0800] ordinary Map/Any owner-v2 GetAny failed".as_ptr(),
        };
        unsafe { super::super::mimi_runtime_abort(message) };
    }

    // SAFETY: the canonical Core i32 GetAny miss descriptor copies exactly
    // four bytes and owns no nested resources.
    unsafe extern "C" fn copy_i32_for_domain_abort_test(
        destination: *mut u8,
        source: *const u8,
    ) -> i32 {
        unsafe { std::ptr::copy_nonoverlapping(source, destination, 4) };
        0
    }

    // SAFETY: canonical Core i32 has no nested resources to release.
    unsafe extern "C" fn drop_i32_for_domain_abort_test(_payload: *mut u8) {}
}
