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
use std::sync::{Mutex, MutexGuard};

use super::any_owner::{self, AnyOwnerToken, DescriptorIdentity};
use super::handle::{self, LeaseThreadId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OrdinaryMapProfile {
    OwnerLifecycleI32StringV1,
    OwnedGetI32StringV1,
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
) -> Result<(), StringRegistrationError> {
    let mut strings = lock_owned_strings();
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
    #[cfg(test)]
    if REGISTRATION_FAIL.with(std::cell::Cell::get) {
        return Err(StringRegistrationError::Capacity);
    }
    strings
        .try_reserve(1)
        .map_err(|_| StringRegistrationError::Capacity)?;
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
    )
}

/// Separate from Map entry owners: only snapshots accepted by the ManagedAny
/// ABI appear here. Registration does not expose or reinterpret ValueHandle.
#[derive(Clone, Copy)]
struct RegisteredAny {
    creator: LeaseThreadId,
    token: AnyOwnerToken,
    descriptor: DescriptorIdentity,
}

static OWNED_ANY: Mutex<Vec<RegisteredAny>> = Mutex::new(Vec::new());

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
    let creator = handle::current_lease_thread_id();
    let mut owners = OWNED_ANY
        .lock()
        .map_err(|_| ManagedAnyDomainError::Poisoned)?;
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
        descriptor,
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
    Ok(owners
        .iter()
        .find(|owner| owner.token == token)
        .map(|owner| owner.descriptor))
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
    match register_owned_string(creator, allocation_base, payload_len) {
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
    #[cfg(test)]
    if REGISTRATION_FAIL.with(std::cell::Cell::get) {
        return Err(OrdinaryMapDomainError::Capacity);
    }

    let mut roots = lock_roots();
    roots
        .try_reserve(1)
        .map_err(|_| OrdinaryMapDomainError::Capacity)?;
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
    while let Some(owner) = extract_managed_any_for(creator) {
        // Detach first; stale/poisoned/descriptor errors are secondary cleanup
        // failures and must not recurse into the original trap path.
        let _ = any_owner::release_owner_v1(owner.token, owner.descriptor, 1);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::map_any_snapshot::OrdinaryMapSnapshot;

    const PROFILES: [OrdinaryMapProfile; 2] = [
        OrdinaryMapProfile::OwnerLifecycleI32StringV1,
        OrdinaryMapProfile::OwnedGetI32StringV1,
    ];

    fn install(profile: OrdinaryMapProfile, snapshot: OrdinaryMapSnapshot) -> i64 {
        let map = match profile {
            OrdinaryMapProfile::OwnerLifecycleI32StringV1 => {
                super::super::MimiMap::ordinary_map_owner_lifecycle_i32_string_v1(snapshot)
            }
            OrdinaryMapProfile::OwnedGetI32StringV1 => {
                super::super::MimiMap::ordinary_map_owned_get_i32_string_v1(snapshot)
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
                _ => b"owned",
            };
            let original = !message.is_null()
                && unsafe { std::ffi::CStr::from_ptr(message) }.to_bytes() == expected;
            let settled = acquire(OrdinaryMapProfile::OwnedGetI32StringV1, root).is_err()
                && !registered(OrdinaryMapProfile::OwnedGetI32StringV1, root)
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
                    || (registered_string(diagnostic).is_none()
                        && !string_allocation_live(diagnostic)));
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
        let dynamic = fresh_string(b"dynamic-key");
        assert_eq!(
            unsafe { mimi_mir_ordinary_map_domain_own_string(dynamic, 11) },
            1
        );
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
            _ => panic!("unknown callback child mode"),
        };
        let diagnostic = if mode == 0 || mode == 3 {
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
            _ => any_owner::string_payload_pointer_for_test(managed).unwrap(),
        };
        unsafe { super::super::mimi_runtime_abort(message) };
    }
}
