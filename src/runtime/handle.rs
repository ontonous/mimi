//! Map/Set handle generation + per-op lease.
//!
//! 0.38.36 (B-HANDLE-001): handles are no longer raw Box addresses with a
//! "caller must not concurrent-destroy" contract. Each handle carries a
//! [`HandleGeneration`] (this name is **not** Flow `TransitionEpoch`).
//! Every internal op acquires one exclusive lease. Other threads serialize on
//! that handle; same-thread reentry fails closed rather than deadlocking or
//! creating aliased references. C `mimi_*_lease_*` calls are lifetime pins,
//! kept separate from operation access. Destroy stops new leases and pins,
//! then frees immediately or after the last outstanding reference drops.
//! Use of a destroyed / stale handle is a typed [`HandleError`], not UAF.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

/// Per-handle generation. Distinct from Flow `TransitionEpoch`.
pub type HandleGeneration = u32;

/// Largest Map/Set slot index. The low-word high bit is reserved for
/// canonical Any owner tokens and must not be used as a Map/Set table index.
const MAX_MAP_SET_SLOT_INDEX: u32 = 0x7fff_ffff;

/// Typed handle-op result codes (C ABI `i32`).
pub const HANDLE_OK: i32 = 0;
pub const HANDLE_ERR_INVALID: i32 = 1;
pub const HANDLE_ERR_STALE: i32 = 2;
pub const HANDLE_ERR_DESTROYED: i32 = 3;
pub const HANDLE_ERR_REENTRANT: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    Invalid,
    StaleGeneration,
    Destroyed,
    Reentrant,
}

impl HandleError {
    pub fn code(self) -> i32 {
        match self {
            HandleError::Invalid => HANDLE_ERR_INVALID,
            HandleError::StaleGeneration => HANDLE_ERR_STALE,
            HandleError::Destroyed => HANDLE_ERR_DESTROYED,
            HandleError::Reentrant => HANDLE_ERR_REENTRANT,
        }
    }
}

thread_local! {
    static LAST_HANDLE_ERROR: std::cell::Cell<i32> = const { std::cell::Cell::new(HANDLE_OK) };
    static LEASE_THREAD_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Lease ownership needs a stable, never-reused thread identity, but calling
/// std::thread::current() allocates a Rust Thread on a native C entry thread.
/// That allocation has no Rust main-runtime shutdown there. A const TLS cell
/// and monotonic identity provide the same reentry check without allocation
/// or a destructor, including for foreign threads calling the runtime ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LeaseThreadId(u64);

static NEXT_LEASE_THREAD_ID: AtomicU64 = AtomicU64::new(1);

fn allocate_lease_thread_id(counter: &AtomicU64) -> Option<LeaseThreadId> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .ok()
        .map(LeaseThreadId)
}

pub(super) fn current_lease_thread_id() -> LeaseThreadId {
    LEASE_THREAD_ID.with(|cached| {
        let value = cached.get();
        if value != 0 {
            return LeaseThreadId(value);
        }
        // Identity exhaustion must not wrap and turn a different thread into
        // a reentrant owner. No handle access occurs after exhaustion.
        let identity = allocate_lease_thread_id(&NEXT_LEASE_THREAD_ID)
            .unwrap_or_else(|| std::process::abort());
        cached.set(identity.0);
        identity
    })
}

/// Read the allocation-free thread identity without assigning a new one.
/// Orderly trap cleanup must not abort on identity exhaustion or initialize
/// thread state when this thread has never created an ordinary Map root.
pub(super) fn cached_lease_thread_id() -> Option<LeaseThreadId> {
    LEASE_THREAD_ID
        .try_with(|cached| {
            let value = cached.get();
            (value != 0).then_some(LeaseThreadId(value))
        })
        .ok()
        .flatten()
}

pub fn set_handle_error(err: HandleError) {
    LAST_HANDLE_ERROR.with(|c| c.set(err.code()));
}

pub fn clear_handle_error() {
    LAST_HANDLE_ERROR.with(|c| c.set(HANDLE_OK));
}

#[no_mangle]
pub extern "C" fn mimi_handle_last_error() -> i32 {
    LAST_HANDLE_ERROR.with(|c| c.get())
}

struct Slot<T> {
    generation: HandleGeneration,
    /// Internal operation lease count. It is either zero or one and is
    /// protected by the table mutex; C lifetime pins use `pins` separately.
    leases: AtomicI64,
    /// The thread holding the exclusive internal operation lease, if any.
    active_owner: Option<LeaseThreadId>,
    /// Explicit C ABI lifetime pins. Pins keep the allocation alive but do
    /// not grant access to the contained Map/Set.
    pins: AtomicI64,
    /// Destroy has started: no new leases.
    retired: AtomicBool,
    /// Destroy has been requested and frees after operations and pins drain.
    pending_free: AtomicBool,
    obj: Option<Box<T>>,
}

struct Table<T: Send> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    // Generations for compacted tables must never restart at one.
    generation_floor: HandleGeneration,
    // At least one slot reached MAX. Keep retired indices in place forever,
    // but continue allocating other free/fresh indices at the MAX generation.
    generation_exhausted: bool,
}

impl<T: Send> Table<T> {
    fn new() -> Self {
        // Index 0 is reserved so pack(0, _) is never a valid live handle
        // that collides with the C "null handle" value 0.
        Self {
            slots: vec![Slot {
                generation: 0,
                leases: AtomicI64::new(0),
                active_owner: None,
                pins: AtomicI64::new(0),
                retired: AtomicBool::new(true),
                pending_free: AtomicBool::new(false),
                obj: None,
            }],
            free: Vec::new(),
            generation_floor: 1,
            generation_exhausted: false,
        }
    }
}

fn pack(index: u32, gen: HandleGeneration) -> i64 {
    // low 32: index, high 32: generation. handle 0 remains invalid because
    // we never allocate index 0. The low-word high bit remains reserved for
    // canonical Any owner tokens.
    ((gen as i64) << 32) | (index as i64)
}

fn unpack(handle: i64) -> Result<(u32, HandleGeneration), HandleError> {
    if handle == 0 {
        return Err(HandleError::Invalid);
    }
    let index = handle as u32;
    let gen = (handle >> 32) as HandleGeneration;
    if index == 0 || index > MAX_MAP_SET_SLOT_INDEX {
        return Err(HandleError::Invalid);
    }
    Ok((index, gen))
}

/// A missing Map-table index can still identify an expired handle if the
/// table has been compacted since that handle was issued. Issued generations
/// below the retained floor predate every allocation after compaction;
/// generation zero and current/future generations at a never-allocated index
/// remain invalid.
fn missing_map_slot_error(
    generation: HandleGeneration,
    generation_floor: HandleGeneration,
) -> HandleError {
    if generation != 0 && generation < generation_floor {
        HandleError::StaleGeneration
    } else {
        HandleError::Invalid
    }
}

static MAP_TABLE: std::sync::OnceLock<Mutex<Table<super::MimiMap>>> = std::sync::OnceLock::new();
static SET_TABLE: std::sync::OnceLock<Mutex<Table<super::MimiSet>>> = std::sync::OnceLock::new();
static MAP_CONDVAR: std::sync::OnceLock<Condvar> = std::sync::OnceLock::new();
static SET_CONDVAR: std::sync::OnceLock<Condvar> = std::sync::OnceLock::new();

/// A thread-reentrant gate for recursive Map/Set serialization only. Ordinary
/// Map/Set operations use per-handle leases and are not globally serialized.
/// Serializer functions enter this scope before acquiring their first handle,
/// so two recursive serializer walks cannot hold opposite ends of a handle
/// graph while waiting on each other.
struct OperationGate {
    owner_depth: Mutex<Option<(LeaseThreadId, usize)>>,
    changed: Condvar,
}

static OPERATION_GATE: std::sync::OnceLock<OperationGate> = std::sync::OnceLock::new();

fn operation_gate() -> &'static OperationGate {
    OPERATION_GATE.get_or_init(|| OperationGate {
        owner_depth: Mutex::new(None),
        changed: Condvar::new(),
    })
}

struct SerializerGraphLease {
    owner: LeaseThreadId,
    _not_send_or_sync: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl SerializerGraphLease {
    fn acquire() -> Self {
        let owner = current_lease_thread_id();
        let gate = operation_gate();
        let mut state = gate
            .owner_depth
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            match *state {
                Some((active, _)) if active != owner => {
                    state = gate
                        .changed
                        .wait(state)
                        .unwrap_or_else(|error| error.into_inner());
                }
                Some((_, depth)) => {
                    *state = Some((
                        owner,
                        depth.checked_add(1).expect("operation depth overflow"),
                    ));
                    break;
                }
                None => {
                    *state = Some((owner, 1));
                    break;
                }
            }
        }
        Self {
            owner,
            _not_send_or_sync: std::marker::PhantomData,
        }
    }
}

impl Drop for SerializerGraphLease {
    fn drop(&mut self) {
        let gate = operation_gate();
        let mut state = gate
            .owner_depth
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some((owner, depth)) = *state else {
            return;
        };
        debug_assert_eq!(owner, self.owner, "operation gate dropped by non-owner");
        if owner != self.owner || depth == 0 {
            return;
        }
        if depth == 1 {
            *state = None;
            gate.changed.notify_all();
        } else {
            *state = Some((owner, depth - 1));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum JsonContainerKind {
    Map,
    Set,
}

thread_local! {
    static JSON_CONTAINER_PATH: std::cell::RefCell<Vec<(JsonContainerKind, i64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Scope for one node in a recursive JSON Map/Set walk. Cyclic edges are
/// reported as an explicit JSON error object instead of reentering a live
/// mutable handle and aborting the process.
pub(super) struct JsonContainerScope {
    kind: JsonContainerKind,
    handle: i64,
    _graph: SerializerGraphLease,
}

impl Drop for JsonContainerScope {
    fn drop(&mut self) {
        JSON_CONTAINER_PATH.with(|path| {
            let popped = path.borrow_mut().pop();
            debug_assert_eq!(popped, Some((self.kind, self.handle)));
        });
    }
}

pub(super) fn json_container_scope(
    kind: JsonContainerKind,
    handle: i64,
) -> Option<JsonContainerScope> {
    let graph = SerializerGraphLease::acquire();
    let entered = JSON_CONTAINER_PATH.with(|path| {
        let mut path = path.borrow_mut();
        if path.contains(&(kind, handle)) {
            false
        } else {
            path.push((kind, handle));
            true
        }
    });
    if !entered {
        drop(graph);
        return None;
    }
    Some(JsonContainerScope {
        kind,
        handle,
        _graph: graph,
    })
}

fn maps() -> &'static Mutex<Table<super::MimiMap>> {
    MAP_TABLE.get_or_init(|| Mutex::new(Table::new()))
}
fn sets() -> &'static Mutex<Table<super::MimiSet>> {
    SET_TABLE.get_or_init(|| Mutex::new(Table::new()))
}
fn map_condvar() -> &'static Condvar {
    MAP_CONDVAR.get_or_init(Condvar::new)
}
fn set_condvar() -> &'static Condvar {
    SET_CONDVAR.get_or_init(Condvar::new)
}

fn lock_maps() -> std::sync::MutexGuard<'static, Table<super::MimiMap>> {
    maps().lock().unwrap_or_else(|e| e.into_inner())
}
fn lock_sets() -> std::sync::MutexGuard<'static, Table<super::MimiSet>> {
    sets().lock().unwrap_or_else(|e| e.into_inner())
}

fn alloc_slot<T: Send>(table: &mut Table<T>, obj: T) -> i64 {
    let gen = table.generation_floor;
    if table.slots.is_empty() {
        table.slots.push(Slot {
            generation: 0,
            leases: AtomicI64::new(0),
            active_owner: None,
            pins: AtomicI64::new(0),
            retired: AtomicBool::new(true),
            pending_free: AtomicBool::new(false),
            obj: None,
        });
    }
    if let Some(idx) = table.free.pop() {
        let slot = &mut table.slots[idx as usize];
        // generation was bumped on destroy; use the current value
        let g = slot.generation.max(gen);
        slot.generation = g;
        slot.leases.store(0, Ordering::SeqCst);
        slot.active_owner = None;
        slot.pins.store(0, Ordering::SeqCst);
        slot.retired.store(false, Ordering::SeqCst);
        slot.pending_free.store(false, Ordering::SeqCst);
        slot.obj = Some(Box::new(obj));
        pack(idx, g)
    } else {
        let Some(idx) = checked_slot_index(table.slots.len()) else {
            return 0;
        };
        table.slots.push(Slot {
            generation: gen,
            leases: AtomicI64::new(0),
            active_owner: None,
            pins: AtomicI64::new(0),
            retired: AtomicBool::new(false),
            pending_free: AtomicBool::new(false),
            obj: Some(Box::new(obj)),
        });
        pack(idx, gen)
    }
}

fn checked_slot_index(slot_count: usize) -> Option<u32> {
    u32::try_from(slot_count)
        .ok()
        .filter(|index| *index != 0 && *index <= MAX_MAP_SET_SLOT_INDEX)
}

fn record_alloc_result(handle: i64) -> i64 {
    if handle == 0 {
        set_handle_error(HandleError::Invalid);
    }
    handle
}

pub(super) struct MapLease {
    handle: i64,
    ptr: *mut super::MimiMap,
    _not_send_or_sync: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl MapLease {
    pub fn get(&self) -> &super::MimiMap {
        unsafe { &*self.ptr }
    }
    pub fn get_mut(&mut self) -> &mut super::MimiMap {
        unsafe { &mut *self.ptr }
    }
    pub fn as_ptr(&self) -> *mut super::MimiMap {
        self.ptr
    }
}

impl std::ops::Deref for MapLease {
    type Target = super::MimiMap;
    fn deref(&self) -> &super::MimiMap {
        self.get()
    }
}
impl std::ops::DerefMut for MapLease {
    fn deref_mut(&mut self) -> &mut super::MimiMap {
        self.get_mut()
    }
}

impl Drop for MapLease {
    fn drop(&mut self) {
        let _ = map_release(self.handle);
    }
}

pub(super) struct SetLease {
    handle: i64,
    ptr: *mut super::MimiSet,
    _not_send_or_sync: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl SetLease {
    pub fn get(&self) -> &super::MimiSet {
        unsafe { &*self.ptr }
    }
    pub fn get_mut(&mut self) -> &mut super::MimiSet {
        unsafe { &mut *self.ptr }
    }
}

impl std::ops::Deref for SetLease {
    type Target = super::MimiSet;
    fn deref(&self) -> &super::MimiSet {
        self.get()
    }
}
impl std::ops::DerefMut for SetLease {
    fn deref_mut(&mut self) -> &mut super::MimiSet {
        self.get_mut()
    }
}

impl Drop for SetLease {
    fn drop(&mut self) {
        let _ = set_release(self.handle);
    }
}

pub fn map_new_handle(obj: super::MimiMap) -> i64 {
    clear_handle_error();
    let mut t = lock_maps();
    record_alloc_result(alloc_slot(&mut t, obj))
}

/// Fallible root allocation used by ordinary snapshot maps. Unlike the
/// legacy constructor, capacity failure returns the uninstalled Map so its
/// typed owners can be dropped after `MAP_TABLE` is unlocked.
pub(super) fn map_new_handle_fallible(obj: super::MimiMap) -> Result<i64, Box<super::MimiMap>> {
    clear_handle_error();
    let mut table = lock_maps();
    // Trap retirement and registration rollback cannot allocate a free-list
    // entry. Reuse their empty metadata slots in this fallible profile path;
    // legacy constructors and normal Drop queues keep their existing route.
    if table.free.is_empty() {
        if let Some(index) = unqueued_reusable_map_slot(&table) {
            let generation_floor = table.generation_floor;
            let slot = &mut table.slots[index as usize];
            let generation = slot.generation.max(generation_floor);
            slot.generation = generation;
            slot.leases.store(0, Ordering::SeqCst);
            slot.active_owner = None;
            slot.pins.store(0, Ordering::SeqCst);
            slot.retired.store(false, Ordering::SeqCst);
            slot.pending_free.store(false, Ordering::SeqCst);
            slot.obj = Some(Box::new(obj));
            return Ok(pack(index, generation));
        }
    }
    let slot_count = table.slots.len().max(1);
    if table.free.is_empty() && checked_slot_index(slot_count).is_none() {
        set_handle_error(HandleError::Invalid);
        drop(table);
        return Err(Box::new(obj));
    }
    let handle = alloc_slot(&mut table, obj);
    debug_assert_ne!(handle, 0, "preflighted Map slot allocation must succeed");
    Ok(handle)
}

/// Find a reusable slot omitted from the free queue by no-allocation
/// retirement. The reserved null slot and exhausted generations never qualify.
fn unqueued_reusable_map_slot(table: &Table<super::MimiMap>) -> Option<u32> {
    table
        .slots
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, slot)| {
            if slot.obj.is_none() && !slot.retired.load(Ordering::SeqCst) {
                u32::try_from(index).ok()
            } else {
                None
            }
        })
}

/// MapRoot's closed-profile operations run under the registry lock itself.
/// This avoids acquiring a thread-id lease while preserving the production
/// Map object and handle validation rules.
pub(super) fn map_root_set(handle: i64, key: String, value: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let slot = t
        .slots
        .get_mut(index as usize)
        .ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, gen)?;
    validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
    slot.obj
        .as_mut()
        .ok_or(HandleError::Destroyed)?
        .inner
        .insert(key, value);
    Ok(())
}

/// Store one newly cloned tagged Any String as an owned Map payload. The
/// Checker-receipted profile has no get/values/escape operations; persistent
/// Map clones still share the payload Arc so runtime API use cannot free the
/// backing while a sibling clone remains alive.
pub(super) fn map_root_set_owned_any_string(
    handle: i64,
    key: String,
    value: i64,
) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let previous_owner = {
        let mut t = lock_maps();
        let slot = t
            .slots
            .get_mut(index as usize)
            .ok_or(HandleError::Invalid)?;
        validate_map_root_slot(slot, gen)?;
        validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
        let map = slot.obj.as_mut().ok_or(HandleError::Destroyed)?;
        if map.owned.contains_key(&value) {
            return Err(HandleError::Invalid);
        }
        map.owned
            .insert(value, super::map_owned_any_string_payload(value));
        let old_value = map.inner.insert(key, value);
        old_value.and_then(|old_value| {
            (old_value != value)
                .then(|| map.owned.remove(&old_value))
                .flatten()
        })
    };
    // Releasing a replaced tagged string acquires the Any provenance lock.
    // Drop outside MAP_TABLE so future changes cannot introduce lock nesting
    // through a payload destructor.
    drop(previous_owner);
    Ok(())
}

/// Remove one MapRoot entry and release its String owner only after the
/// registry lock is released. A payload may still be named by another entry
/// in this exact table or by a shallow Map clone, so the local table is
/// checked before detaching its shared owner record.
pub(super) fn map_root_remove(handle: i64, key: &str) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let removed_owner = {
        let mut t = lock_maps();
        let slot = t
            .slots
            .get_mut(index as usize)
            .ok_or(HandleError::Invalid)?;
        validate_map_root_slot(slot, gen)?;
        validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
        let map = slot.obj.as_mut().ok_or(HandleError::Destroyed)?;
        let Some(removed_value) = map.inner.remove(key) else {
            return Ok(());
        };
        if map.inner.values().any(|value| *value == removed_value) {
            None
        } else {
            map.owned.remove(&removed_value)
        }
    };
    // MapOwnedPayload may unregister a tagged Any String. Keep that destructor
    // outside MAP_TABLE so Map and Any registry locks are never nested here.
    drop(removed_owner);
    Ok(())
}

/// Check whether a live MapRoot contains a key without observing its payload.
pub(super) fn map_root_contains(handle: i64, key: &str) -> Result<bool, HandleError> {
    let (index, gen) = unpack(handle)?;
    let t = lock_maps();
    let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, gen)?;
    validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
    let map = slot.obj.as_ref().ok_or(HandleError::Destroyed)?;
    Ok(map.inner.contains_key(key))
}

pub(super) fn map_root_size(handle: i64) -> Result<usize, HandleError> {
    let (index, gen) = unpack(handle)?;
    let t = lock_maps();
    let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, gen)?;
    validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
    Ok(slot.obj.as_ref().ok_or(HandleError::Destroyed)?.inner.len())
}

pub(super) fn map_root_drop(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, gen)?;
    validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
    let retired_map = finish_map_free(&mut t, index);
    drop(t);
    drop(retired_map);
    Ok(())
}

/// Retire one MapAnyOwner v1 root and return the removed Map object. The
/// caller releases its typed owner tokens only after this function has
/// invalidated the Map handle and released MAP_TABLE.
pub(super) fn map_any_owner_drop(handle: i64) -> Result<Box<super::MimiMap>, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, gen)?;
    validate_map_slot_mode(slot, super::MapStorageMode::CanonicalAnyOwnerV1)?;
    let retired_map = finish_map_free(&mut t, index).ok_or(HandleError::Destroyed)?;
    drop(t);
    Ok(retired_map)
}

/// Retire one ordinary Map lifecycle v1 root. Typed entry owners are released
/// only after the shared Map table has invalidated the handle and unlocked.
pub(super) fn ordinary_map_owner_lifecycle_i32_string_v1_drop(
    handle: i64,
) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut table = lock_maps();
    let slot = table
        .slots
        .get(index as usize)
        .ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, gen)?;
    validate_map_slot_mode(
        slot,
        super::MapStorageMode::OrdinaryMapOwnerLifecycleI32StringV1,
    )?;
    let retired_map = finish_map_free(&mut table, index);
    drop(table);
    drop(retired_map);
    Ok(())
}

/// Retire one ordinary Map owned-Get v1 root. This profile shares the typed
/// snapshot storage implementation with lifecycle v1, while retaining its
/// own Map storage mode so no other root API can accept its handle.
pub(super) fn ordinary_map_owned_get_i32_string_v1_drop(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut table = lock_maps();
    let slot = table
        .slots
        .get(index as usize)
        .ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, gen)?;
    validate_map_slot_mode(slot, super::MapStorageMode::OrdinaryMapOwnedGetI32StringV1)?;
    let retired_map = finish_map_free(&mut table, index);
    drop(table);
    drop(retired_map);
    Ok(())
}

/// Retire one ordinary Map/Any V2 concrete-record root.  The record owner
/// payload is dropped only after the generational Map slot has been
/// invalidated and the table lock has been released.
pub(super) fn ordinary_map_any_owner_v2_record_drop(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut table = lock_maps();
    let slot = table
        .slots
        .get(index as usize)
        .ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, gen)?;
    validate_map_slot_mode(slot, super::MapStorageMode::OrdinaryMapAnyOwnerV2Record)?;
    let retired_map = finish_map_free(&mut table, index);
    drop(table);
    drop(retired_map);
    Ok(())
}

/// Retire the distinct descriptor-selected ManagedAny V2 ordinary-map root.
/// Payload callbacks run only after MAP_TABLE has been released.
pub(super) fn ordinary_map_owned_any_v2_drop(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut table = lock_maps();
    let slot = table
        .slots
        .get(index as usize)
        .ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, gen)?;
    validate_map_slot_mode(slot, super::MapStorageMode::OrdinaryMapOwnedAnyV2)?;
    let retired_map = finish_map_free(&mut table, index);
    drop(table);
    drop(retired_map);
    Ok(())
}

/// Raw cleanup for an unpublished or trapping-thread lifecycle-profile
/// root. It never allocates a free-list entry or invokes the trapping ABI.
pub(super) fn ordinary_map_owner_lifecycle_i32_string_v1_drop_for_trap(
    handle: i64,
) -> Result<(), HandleError> {
    ordinary_map_profile_drop_for_trap(
        handle,
        super::MapStorageMode::OrdinaryMapOwnerLifecycleI32StringV1,
    )
}

/// Raw cleanup for the owned-Get profile, with the same no-allocation and
/// exact-profile validation contract as lifecycle cleanup.
pub(super) fn ordinary_map_owned_get_i32_string_v1_drop_for_trap(
    handle: i64,
) -> Result<(), HandleError> {
    ordinary_map_profile_drop_for_trap(
        handle,
        super::MapStorageMode::OrdinaryMapOwnedGetI32StringV1,
    )
}

/// Allocation-free trap cleanup for a V2 concrete-record root.
pub(super) fn ordinary_map_any_owner_v2_record_drop_for_trap(
    handle: i64,
) -> Result<(), HandleError> {
    ordinary_map_profile_drop_for_trap(handle, super::MapStorageMode::OrdinaryMapAnyOwnerV2Record)
}

/// Allocation-free trap cleanup for a descriptor-selected ManagedAny V2 root.
pub(super) fn ordinary_map_owned_any_v2_drop_for_trap(handle: i64) -> Result<(), HandleError> {
    ordinary_map_profile_drop_for_trap(handle, super::MapStorageMode::OrdinaryMapOwnedAnyV2)
}

fn ordinary_map_profile_drop_for_trap(
    handle: i64,
    expected: super::MapStorageMode,
) -> Result<(), HandleError> {
    let (index, generation) = unpack(handle)?;
    let mut table = lock_maps();
    let slot = table
        .slots
        .get(index as usize)
        .ok_or(HandleError::Invalid)?;
    validate_map_root_slot(slot, generation)?;
    validate_map_slot_mode(slot, expected)?;
    let retired_map = finish_map_free_for_trap(&mut table, index);
    drop(table);
    // Any descriptor Drop glue must remain outside both MAP_TABLE and the
    // execution-domain registry. The caller has detached its domain record.
    drop(retired_map);
    Ok(())
}

fn validate_map_root_slot<T>(
    slot: &Slot<T>,
    generation: HandleGeneration,
) -> Result<(), HandleError> {
    if slot.generation != generation {
        return Err(HandleError::StaleGeneration);
    }
    if slot.retired.load(Ordering::SeqCst) || slot.obj.is_none() {
        return Err(HandleError::Destroyed);
    }
    if slot.leases.load(Ordering::SeqCst) != 0
        || slot.pins.load(Ordering::SeqCst) != 0
        || slot.active_owner.is_some()
    {
        return Err(HandleError::Reentrant);
    }
    Ok(())
}

fn validate_map_slot_mode(
    slot: &Slot<super::MimiMap>,
    expected: super::MapStorageMode,
) -> Result<(), HandleError> {
    let map = slot.obj.as_deref().ok_or(HandleError::Destroyed)?;
    validate_map_storage_mode(map, expected)
}

fn validate_map_storage_mode(
    map: &super::MimiMap,
    expected: super::MapStorageMode,
) -> Result<(), HandleError> {
    if map.mode == expected {
        Ok(())
    } else {
        Err(HandleError::Invalid)
    }
}

fn compact_empty_map_table(t: &mut Table<super::MimiMap>) {
    if t.generation_exhausted
        || t.slots
            .iter()
            .skip(1)
            .any(|s| s.obj.is_some() || s.retired.load(Ordering::SeqCst))
    {
        return;
    }
    // Retain the monotone generation floor while releasing slot/free capacity.
    t.slots.clear();
    t.slots.shrink_to_fit();
    t.free.clear();
    t.free.shrink_to_fit();
}

/// Count currently allocated production Map slots for the test-only
/// MapRoot lifecycle probe.
#[cfg(any(test, mimi_test_ub_symbols))]
pub fn mimi_test_map_live_count() -> i64 {
    let t = lock_maps();
    i64::try_from(
        t.slots
            .iter()
            .filter(|slot| slot.obj.is_some() && !slot.retired.load(Ordering::SeqCst))
            .count(),
    )
    .unwrap_or(i64::MAX)
}

pub fn set_new_handle(obj: super::MimiSet) -> i64 {
    clear_handle_error();
    let mut t = lock_sets();
    record_alloc_result(alloc_slot(&mut t, obj))
}

pub fn map_acquire(handle: i64) -> Result<MapLease, HandleError> {
    map_acquire_mode(handle, super::MapStorageMode::LegacyRaw)
}

/// Lease a MapAnyOwner v1 object without making it eligible for ordinary
/// ValueHandle-based Map helpers.
pub(super) fn map_any_owner_acquire(handle: i64) -> Result<MapLease, HandleError> {
    map_acquire_mode(handle, super::MapStorageMode::CanonicalAnyOwnerV1)
}

/// Lease a persistent ordinary-Map lifecycle v1 root. Its dedicated runtime
/// operations cannot be reached through legacy `ValueHandle` helpers.
pub(super) fn ordinary_map_owner_lifecycle_i32_string_v1_acquire(
    handle: i64,
) -> Result<MapLease, HandleError> {
    map_acquire_mode(
        handle,
        super::MapStorageMode::OrdinaryMapOwnerLifecycleI32StringV1,
    )
}

/// Lease a persistent ordinary-Map owned-Get v1 root. The distinct storage
/// mode prevents lifecycle-v1, MapAnyOwner, MapRoot, and legacy helpers from
/// accepting this handle.
pub(super) fn ordinary_map_owned_get_i32_string_v1_acquire(
    handle: i64,
) -> Result<MapLease, HandleError> {
    map_acquire_mode(
        handle,
        super::MapStorageMode::OrdinaryMapOwnedGetI32StringV1,
    )
}

/// Lease a persistent ordinary Map/Any V2 record root.  Its dedicated mode
/// keeps the concrete Label owner table out of all legacy ValueHandle and v1
/// AnyOwner entry points.
pub(super) fn ordinary_map_any_owner_v2_record_acquire(
    handle: i64,
) -> Result<MapLease, HandleError> {
    map_acquire_mode(handle, super::MapStorageMode::OrdinaryMapAnyOwnerV2Record)
}

/// Lease a persistent descriptor-selected ManagedAny V2 ordinary Map root.
pub(super) fn ordinary_map_owned_any_v2_acquire(handle: i64) -> Result<MapLease, HandleError> {
    map_acquire_mode(handle, super::MapStorageMode::OrdinaryMapOwnedAnyV2)
}

fn map_acquire_mode(handle: i64, expected: super::MapStorageMode) -> Result<MapLease, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let current = current_lease_thread_id();
    loop {
        let slot = match t.slots.get(index as usize) {
            Some(slot) => slot,
            None => return Err(missing_map_slot_error(gen, t.generation_floor)),
        };
        if slot.generation != gen {
            return Err(HandleError::StaleGeneration);
        }
        if slot.retired.load(Ordering::SeqCst) || slot.obj.is_none() {
            return Err(HandleError::Destroyed);
        }
        validate_map_slot_mode(slot, expected)?;
        match slot.active_owner {
            Some(owner) if owner == current => return Err(HandleError::Reentrant),
            Some(_) => {
                t = map_condvar().wait(t).unwrap_or_else(|e| e.into_inner());
            }
            None => {
                let slot = &mut t.slots[index as usize];
                slot.active_owner = Some(current);
                slot.leases.store(1, Ordering::SeqCst);
                let ptr = slot
                    .obj
                    .as_mut()
                    .map(|b| &mut **b as *mut super::MimiMap)
                    .ok_or(HandleError::Destroyed)?;
                clear_handle_error();
                return Ok(MapLease {
                    handle,
                    ptr,
                    _not_send_or_sync: std::marker::PhantomData,
                });
            }
        }
    }
}

pub fn set_acquire(handle: i64) -> Result<SetLease, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_sets();
    let current = current_lease_thread_id();
    loop {
        let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
        if slot.generation != gen {
            return Err(HandleError::StaleGeneration);
        }
        if slot.retired.load(Ordering::SeqCst) || slot.obj.is_none() {
            return Err(HandleError::Destroyed);
        }
        match slot.active_owner {
            Some(owner) if owner == current => return Err(HandleError::Reentrant),
            Some(_) => {
                t = set_condvar().wait(t).unwrap_or_else(|e| e.into_inner());
            }
            None => {
                let slot = &mut t.slots[index as usize];
                slot.active_owner = Some(current);
                slot.leases.store(1, Ordering::SeqCst);
                let ptr = slot
                    .obj
                    .as_mut()
                    .map(|b| &mut **b as *mut super::MimiSet)
                    .ok_or(HandleError::Destroyed)?;
                clear_handle_error();
                return Ok(SetLease {
                    handle,
                    ptr,
                    _not_send_or_sync: std::marker::PhantomData,
                });
            }
        }
    }
}

fn map_release(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let (remaining, should_free) = {
        let slot = t
            .slots
            .get_mut(index as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation != gen {
            return Err(HandleError::StaleGeneration);
        }
        if slot.active_owner != Some(current_lease_thread_id())
            || slot.leases.load(Ordering::SeqCst) != 1
        {
            return Err(HandleError::Invalid);
        }
        slot.active_owner = None;
        slot.leases.store(0, Ordering::SeqCst);
        let remaining = slot.pins.load(Ordering::SeqCst);
        let should_free = remaining == 0 && slot.pending_free.load(Ordering::SeqCst);
        (remaining, should_free)
    };
    let retired_map = if should_free {
        finish_map_free(&mut t, index)
    } else {
        None
    };
    map_condvar().notify_all();
    drop(t);
    // Payload destructors can acquire other runtime registries. Never run
    // them while holding MAP_TABLE.
    drop(retired_map);
    Ok(remaining)
}

fn set_release(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_sets();
    let (remaining, should_free) = {
        let slot = t
            .slots
            .get_mut(index as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation != gen {
            return Err(HandleError::StaleGeneration);
        }
        if slot.active_owner != Some(current_lease_thread_id())
            || slot.leases.load(Ordering::SeqCst) != 1
        {
            return Err(HandleError::Invalid);
        }
        slot.active_owner = None;
        slot.leases.store(0, Ordering::SeqCst);
        let remaining = slot.pins.load(Ordering::SeqCst);
        let should_free = remaining == 0 && slot.pending_free.load(Ordering::SeqCst);
        (remaining, should_free)
    };
    if should_free {
        finish_set_free(&mut t, index);
    }
    set_condvar().notify_all();
    Ok(remaining)
}

fn map_pin(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let slot = t
        .slots
        .get_mut(index as usize)
        .ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    if slot.retired.load(Ordering::SeqCst) || slot.obj.is_none() {
        return Err(HandleError::Destroyed);
    }
    validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
    let pins = slot.pins.load(Ordering::SeqCst);
    let next = pins.checked_add(1).ok_or(HandleError::Invalid)?;
    slot.pins.store(next, Ordering::SeqCst);
    clear_handle_error();
    Ok(slot.leases.load(Ordering::SeqCst).saturating_add(next))
}

fn set_pin(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_sets();
    let slot = t
        .slots
        .get_mut(index as usize)
        .ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    if slot.retired.load(Ordering::SeqCst) || slot.obj.is_none() {
        return Err(HandleError::Destroyed);
    }
    let pins = slot.pins.load(Ordering::SeqCst);
    let next = pins.checked_add(1).ok_or(HandleError::Invalid)?;
    slot.pins.store(next, Ordering::SeqCst);
    clear_handle_error();
    Ok(slot.leases.load(Ordering::SeqCst).saturating_add(next))
}

fn map_unpin(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let (remaining, should_free) = {
        let slot = t
            .slots
            .get_mut(index as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation != gen {
            return Err(HandleError::StaleGeneration);
        }
        validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
        let pins = slot.pins.load(Ordering::SeqCst);
        if pins == 0 {
            return Err(HandleError::Invalid);
        }
        let next = pins - 1;
        slot.pins.store(next, Ordering::SeqCst);
        let remaining = next.saturating_add(slot.leases.load(Ordering::SeqCst));
        let should_free = remaining == 0 && slot.pending_free.load(Ordering::SeqCst);
        (remaining, should_free)
    };
    let retired_map = if should_free {
        finish_map_free(&mut t, index)
    } else {
        None
    };
    map_condvar().notify_all();
    drop(t);
    // A pending destroy is completed by the final unpin. Defer owner drops
    // until the registry lock is released, just like immediate destruction.
    drop(retired_map);
    Ok(remaining)
}

fn set_unpin(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_sets();
    let (remaining, should_free) = {
        let slot = t
            .slots
            .get_mut(index as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation != gen {
            return Err(HandleError::StaleGeneration);
        }
        let pins = slot.pins.load(Ordering::SeqCst);
        if pins == 0 {
            return Err(HandleError::Invalid);
        }
        let next = pins - 1;
        slot.pins.store(next, Ordering::SeqCst);
        let remaining = next.saturating_add(slot.leases.load(Ordering::SeqCst));
        let should_free = remaining == 0 && slot.pending_free.load(Ordering::SeqCst);
        (remaining, should_free)
    };
    if should_free {
        finish_set_free(&mut t, index);
    }
    set_condvar().notify_all();
    Ok(remaining)
}

fn finish_map_free(t: &mut Table<super::MimiMap>, index: u32) -> Option<Box<super::MimiMap>> {
    let slot = &mut t.slots[index as usize];
    let retired_map = slot.obj.take();
    let can_reuse = if let Some(next) = slot.generation.checked_add(1) {
        slot.generation = next;
        t.generation_floor = t.generation_floor.max(next);
        slot.retired.store(false, Ordering::SeqCst);
        true
    } else {
        // Never let a 32-bit generation wrap and make a very old handle live
        // again. This slot is permanently retired at exhaustion.
        slot.retired.store(true, Ordering::SeqCst);
        t.generation_exhausted = true;
        false
    };
    slot.pending_free.store(false, Ordering::SeqCst);
    slot.leases.store(0, Ordering::SeqCst);
    slot.active_owner = None;
    slot.pins.store(0, Ordering::SeqCst);
    if can_reuse {
        t.free.push(index);
    }
    compact_empty_map_table(t);
    // Slot invalidation and generation advancement are complete before this
    // value leaves the table. The caller must release its table guard before
    // dropping the returned Map, because its payload owners may lock other
    // runtime registries or run descriptor-specific glue.
    retired_map
}

/// Retire without growing the free queue or compacting its metadata.
/// The fallible ordinary-profile constructor finds these empty slots directly,
/// so repeated registration failures reuse capacity rather than growing slots.
fn finish_map_free_for_trap(
    table: &mut Table<super::MimiMap>,
    index: u32,
) -> Option<Box<super::MimiMap>> {
    let slot = &mut table.slots[index as usize];
    let retired_map = slot.obj.take();
    if let Some(next) = slot.generation.checked_add(1) {
        slot.generation = next;
        table.generation_floor = table.generation_floor.max(next);
        slot.retired.store(false, Ordering::SeqCst);
    } else {
        slot.retired.store(true, Ordering::SeqCst);
        table.generation_exhausted = true;
    }
    slot.pending_free.store(false, Ordering::SeqCst);
    slot.leases.store(0, Ordering::SeqCst);
    slot.active_owner = None;
    slot.pins.store(0, Ordering::SeqCst);
    retired_map
}

fn finish_set_free(t: &mut Table<super::MimiSet>, index: u32) {
    let slot = &mut t.slots[index as usize];
    if let Some(set) = slot.obj.take() {
        for value in &set.string_values {
            super::mimi_free(*value as *mut std::ffi::c_void);
        }
        drop(set);
    }
    let can_reuse = if let Some(next) = slot.generation.checked_add(1) {
        slot.generation = next;
        slot.retired.store(false, Ordering::SeqCst);
        true
    } else {
        // See finish_map_free: avoid generation wrap resurrecting stale Set handles.
        slot.retired.store(true, Ordering::SeqCst);
        false
    };
    slot.pending_free.store(false, Ordering::SeqCst);
    slot.leases.store(0, Ordering::SeqCst);
    slot.active_owner = None;
    slot.pins.store(0, Ordering::SeqCst);
    if can_reuse {
        t.free.push(index);
    }
}

/// Stop new operation leases and C lifetime pins. Does not wait or free.
pub fn map_begin_destroy(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let slot = t
        .slots
        .get_mut(index as usize)
        .ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    if slot.obj.is_none() {
        return Err(HandleError::Destroyed);
    }
    validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
    slot.retired.store(true, Ordering::SeqCst);
    clear_handle_error();
    map_condvar().notify_all();
    Ok(())
}

pub fn set_begin_destroy(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_sets();
    let slot = t
        .slots
        .get_mut(index as usize)
        .ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    if slot.obj.is_none() {
        return Err(HandleError::Destroyed);
    }
    slot.retired.store(true, Ordering::SeqCst);
    clear_handle_error();
    set_condvar().notify_all();
    Ok(())
}

/// Finish destroy without blocking. The object is freed now if no internal
/// operation lease or C lifetime pin remains; otherwise the last release
/// completes reclamation.
pub fn map_finish_destroy(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let idle = {
        let slot = t
            .slots
            .get_mut(index as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation != gen {
            return Err(HandleError::StaleGeneration);
        }
        if slot.obj.is_none() {
            return Ok(());
        }
        validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
        slot.retired.store(true, Ordering::SeqCst);
        slot.leases.load(Ordering::SeqCst) == 0 && slot.pins.load(Ordering::SeqCst) == 0
    };
    let retired_map = if idle {
        finish_map_free(&mut t, index)
    } else {
        t.slots[index as usize]
            .pending_free
            .store(true, Ordering::SeqCst);
        None
    };
    map_condvar().notify_all();
    clear_handle_error();
    drop(t);
    drop(retired_map);
    Ok(())
}

pub fn set_finish_destroy(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_sets();
    let slot = t
        .slots
        .get_mut(index as usize)
        .ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    if slot.obj.is_none() {
        return Ok(());
    }
    slot.retired.store(true, Ordering::SeqCst);
    let idle = slot.leases.load(Ordering::SeqCst) == 0 && slot.pins.load(Ordering::SeqCst) == 0;
    if idle {
        finish_set_free(&mut t, index);
    } else {
        t.slots[index as usize]
            .pending_free
            .store(true, Ordering::SeqCst);
    }
    set_condvar().notify_all();
    clear_handle_error();
    Ok(())
}

pub fn map_destroy(handle: i64) -> Result<(), HandleError> {
    if handle == 0 {
        return Ok(());
    }
    match map_begin_destroy(handle) {
        Ok(()) => {}
        Err(HandleError::StaleGeneration) | Err(HandleError::Destroyed) => return Ok(()),
        Err(e) => return Err(e),
    }
    map_finish_destroy(handle).or(Ok(()))
}

pub fn set_destroy(handle: i64) -> Result<(), HandleError> {
    if handle == 0 {
        return Ok(());
    }
    match set_begin_destroy(handle) {
        Ok(()) => {}
        Err(HandleError::StaleGeneration) | Err(HandleError::Destroyed) => return Ok(()),
        Err(e) => return Err(e),
    }
    set_finish_destroy(handle).or(Ok(()))
}

pub fn map_generation(handle: i64) -> Result<HandleGeneration, HandleError> {
    let (index, gen) = unpack(handle)?;
    let t = lock_maps();
    let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    if slot.obj.is_none() {
        return Err(HandleError::Destroyed);
    }
    validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
    Ok(slot.generation)
}

pub fn set_generation(handle: i64) -> Result<HandleGeneration, HandleError> {
    let (index, gen) = unpack(handle)?;
    let t = lock_sets();
    let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    if slot.obj.is_none() {
        return Err(HandleError::Destroyed);
    }
    Ok(slot.generation)
}

pub fn map_lease_count(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let t = lock_maps();
    let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    validate_map_slot_mode(slot, super::MapStorageMode::LegacyRaw)?;
    Ok(slot
        .leases
        .load(Ordering::SeqCst)
        .saturating_add(slot.pins.load(Ordering::SeqCst)))
}

pub fn set_lease_count(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let t = lock_sets();
    let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    Ok(slot
        .leases
        .load(Ordering::SeqCst)
        .saturating_add(slot.pins.load(Ordering::SeqCst)))
}

pub fn with_map<R>(handle: i64, default: R, f: impl FnOnce(&mut super::MimiMap) -> R) -> R {
    match map_acquire(handle) {
        Ok(mut lease) => f(lease.get_mut()),
        Err(e) => {
            set_handle_error(e);
            default
        }
    }
}

pub fn with_set<R>(handle: i64, default: R, f: impl FnOnce(&mut super::MimiSet) -> R) -> R {
    match set_acquire(handle) {
        Ok(mut lease) => f(lease.get_mut()),
        Err(e) => {
            set_handle_error(e);
            default
        }
    }
}

// ---------------------------------------------------------------------------
// Shipped C API
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn mimi_map_lease_acquire(handle: i64) -> i32 {
    match map_pin(handle) {
        Ok(_) => HANDLE_OK,
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_lease_release(handle: i64) -> i32 {
    match map_unpin(handle) {
        Ok(_) => HANDLE_OK,
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_lease_acquire(handle: i64) -> i32 {
    match set_pin(handle) {
        Ok(_) => HANDLE_OK,
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_lease_release(handle: i64) -> i32 {
    match set_unpin(handle) {
        Ok(_) => HANDLE_OK,
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_begin_destroy(handle: i64) -> i32 {
    match map_begin_destroy(handle) {
        Ok(()) => HANDLE_OK,
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_finish_destroy(handle: i64) -> i32 {
    match map_finish_destroy(handle) {
        Ok(()) => HANDLE_OK,
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_begin_destroy(handle: i64) -> i32 {
    match set_begin_destroy(handle) {
        Ok(()) => HANDLE_OK,
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_finish_destroy(handle: i64) -> i32 {
    match set_finish_destroy(handle) {
        Ok(()) => HANDLE_OK,
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_lease_count(handle: i64) -> i64 {
    match map_lease_count(handle) {
        Ok(n) => n,
        Err(e) => {
            set_handle_error(e);
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_lease_count(handle: i64) -> i64 {
    match set_lease_count(handle) {
        Ok(n) => n,
        Err(e) => {
            set_handle_error(e);
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_generation(handle: i64) -> i64 {
    match map_generation(handle) {
        Ok(g) => g as i64,
        Err(e) => {
            set_handle_error(e);
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_generation(handle: i64) -> i64 {
    match set_generation(handle) {
        Ok(g) => g as i64,
        Err(e) => {
            set_handle_error(e);
            -1
        }
    }
}

#[no_mangle]
/// Try to read a Map size, storing zero on failure when `out` is non-null.
///
/// # Safety
/// `out` must be null or point to a properly aligned, writable `i64` that
/// remains valid for the duration of this call. The handle is validated by
/// the runtime and reported through the returned status code.
pub unsafe extern "C" fn mimi_map_try_size(handle: i64, out: *mut i64) -> i32 {
    match map_acquire(handle) {
        Ok(lease) => {
            if !out.is_null() {
                unsafe { *out = lease.get().inner.len() as i64 };
            }
            HANDLE_OK
        }
        Err(e) => {
            set_handle_error(e);
            if !out.is_null() {
                unsafe { *out = 0 };
            }
            e.code()
        }
    }
}

#[no_mangle]
/// Try to read a Set size, storing zero on failure when `out` is non-null.
///
/// # Safety
/// `out` must be null or point to a properly aligned, writable `i64` that
/// remains valid for the duration of this call. The handle is validated by
/// the runtime and reported through the returned status code.
pub unsafe extern "C" fn mimi_set_try_size(handle: i64, out: *mut i64) -> i32 {
    match set_acquire(handle) {
        Ok(lease) => {
            if !out.is_null() {
                unsafe { *out = lease.get().inner.len() as i64 };
            }
            HANDLE_OK
        }
        Err(e) => {
            set_handle_error(e);
            if !out.is_null() {
                unsafe { *out = 0 };
            }
            e.code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn new_map() -> i64 {
        map_new_handle(super::super::MimiMap::legacy_raw())
    }

    fn empty_map() -> super::super::MimiMap {
        super::super::MimiMap::legacy_raw()
    }

    #[test]
    fn map_table_compaction_preserves_generation_and_live_slots() {
        let mut table = Table::new();
        let first = alloc_slot(&mut table, empty_map());
        let (idx, gen1) = unpack(first).unwrap();
        drop(finish_map_free(&mut table, idx));
        compact_empty_map_table(&mut table);
        assert_eq!(table.slots.len(), 0);
        assert_eq!(table.slots.capacity(), 0);
        assert_eq!(table.free.capacity(), 0);
        let second = alloc_slot(&mut table, empty_map());
        let (idx2, gen2) = unpack(second).unwrap();
        assert_eq!(idx, idx2);
        assert!(gen2 > gen1);

        let other = alloc_slot(&mut table, empty_map());
        let (other_idx, _) = unpack(other).unwrap();
        drop(finish_map_free(&mut table, idx2));
        compact_empty_map_table(&mut table);
        assert!(table.slots[other_idx as usize].obj.is_some());
        assert!(table.slots.len() > 1);
    }

    #[test]
    fn compacted_map_index_distinguishes_stale_generation_from_invalid_future_handle() {
        let mut table = Table::new();
        let old = alloc_slot(&mut table, empty_map());
        let (index, old_generation) = unpack(old).unwrap();
        drop(finish_map_free(&mut table, index));
        compact_empty_map_table(&mut table);
        assert!(table.slots.is_empty());
        assert!(table.generation_floor > old_generation);

        assert_eq!(
            missing_map_slot_error(old_generation, table.generation_floor),
            HandleError::StaleGeneration
        );
        assert_eq!(
            missing_map_slot_error(0, table.generation_floor),
            HandleError::Invalid,
            "generation zero was never issued and remains invalid"
        );
        assert_eq!(
            missing_map_slot_error(table.generation_floor, table.generation_floor),
            HandleError::Invalid
        );
        if table.generation_floor < HandleGeneration::MAX {
            assert_eq!(
                missing_map_slot_error(table.generation_floor + 1, table.generation_floor),
                HandleError::Invalid
            );
        }
    }

    #[test]
    fn exhausted_map_generation_retires_only_its_slot_and_preserves_signed_handles() {
        let mut table = Table::new();
        table.generation_floor = HandleGeneration::MAX - 1;
        let first_old = alloc_slot(&mut table, empty_map());
        let second_old = alloc_slot(&mut table, empty_map());
        let guard = alloc_slot(&mut table, empty_map());
        let (first_idx, first_old_gen) = unpack(first_old).unwrap();
        let (second_idx, second_old_gen) = unpack(second_old).unwrap();
        assert_eq!(first_old_gen, HandleGeneration::MAX - 1);
        assert_eq!(second_old_gen, HandleGeneration::MAX - 1);
        drop(finish_map_free(&mut table, first_idx));
        drop(finish_map_free(&mut table, second_idx));

        // Both free indices can safely advance to MAX: no handle for either
        // index at MAX has been issued yet.
        let max_handle = alloc_slot(&mut table, empty_map());
        let (max_idx, max_gen) = unpack(max_handle).unwrap();
        assert_eq!(max_idx, second_idx);
        assert_eq!(max_gen, HandleGeneration::MAX);
        assert!(
            max_handle < 0,
            "the generation high bit makes this i64 signed"
        );
        assert_eq!(unpack(max_handle), Ok((max_idx, HandleGeneration::MAX)));

        // Once a MAX-generation handle is destroyed, its slot is burned. A
        // different free index and then a fresh index remain usable.
        drop(finish_map_free(&mut table, max_idx));
        compact_empty_map_table(&mut table);
        assert!(table.generation_exhausted);
        assert_eq!(table.slots.len(), 4);
        assert!(table.slots[max_idx as usize].retired.load(Ordering::SeqCst));
        assert!(table.free.contains(&first_idx));

        let other_free = alloc_slot(&mut table, empty_map());
        let (other_idx, other_gen) = unpack(other_free).unwrap();
        assert_eq!(other_idx, first_idx);
        assert_eq!(other_gen, HandleGeneration::MAX);
        assert_eq!(
            validate_map_root_slot(&table.slots[first_idx as usize], first_old_gen),
            Err(HandleError::StaleGeneration)
        );
        assert_eq!(
            validate_map_root_slot(&table.slots[max_idx as usize], HandleGeneration::MAX),
            Err(HandleError::Destroyed)
        );
        drop(finish_map_free(&mut table, other_idx));
        let fresh = alloc_slot(&mut table, empty_map());
        let (fresh_idx, fresh_gen) = unpack(fresh).unwrap();
        assert_eq!(fresh_idx, unpack(guard).unwrap().0 + 1);
        assert_eq!(fresh_gen, HandleGeneration::MAX);
        assert_ne!(fresh, max_handle);
        assert!(table.slots[max_idx as usize].retired.load(Ordering::SeqCst));
    }

    #[test]
    fn ordinary_map_trap_retirement_keeps_free_capacity_and_reusable_metadata() {
        let mut table = Table::new();
        let guard = alloc_slot(&mut table, empty_map());
        let root = alloc_slot(&mut table, empty_map());
        let (index, generation) = unpack(root).unwrap();
        let slot_count = table.slots.len();
        let free_capacity = table.free.capacity();
        drop(finish_map_free_for_trap(&mut table, index));
        assert_eq!(table.free.capacity(), free_capacity);
        assert!(table.free.is_empty());
        assert_eq!(table.slots.len(), slot_count);
        assert_eq!(unqueued_reusable_map_slot(&table), Some(index));
        assert_eq!(
            validate_map_root_slot(&table.slots[index as usize], generation),
            Err(HandleError::StaleGeneration)
        );
        drop(finish_map_free(&mut table, unpack(guard).unwrap().0));
    }

    #[test]
    fn slot_index_conversion_reserves_bit_31_for_any_owner_tokens() {
        assert_eq!(checked_slot_index(0), None);
        assert_eq!(checked_slot_index(1), Some(1));
        assert_eq!(
            checked_slot_index(MAX_MAP_SET_SLOT_INDEX as usize),
            Some(MAX_MAP_SET_SLOT_INDEX)
        );
        assert_eq!(
            checked_slot_index(MAX_MAP_SET_SLOT_INDEX as usize + 1),
            None,
            "the first index with the class bit set is forbidden"
        );
        assert_eq!(unpack(pack(0, 1)), Err(HandleError::Invalid));
        assert_eq!(
            unpack(pack(MAX_MAP_SET_SLOT_INDEX + 1, 1)),
            Err(HandleError::Invalid)
        );
        assert_eq!(
            unpack(pack(0x8000_0001, 1)),
            Err(HandleError::Invalid),
            "class-tagged low words must be rejected before table access"
        );
    }

    #[test]
    fn map_and_set_handles_keep_signed_full_width_generations() {
        let generation = 0x8000_0000;
        let mut map_table = Table::new();
        map_table.generation_floor = generation;
        let map_handle = alloc_slot(&mut map_table, empty_map());

        let mut set_table = Table::new();
        set_table.generation_floor = generation;
        let set_handle = alloc_slot(
            &mut set_table,
            super::super::MimiSet {
                inner: Default::default(),
                string_values: Default::default(),
            },
        );

        assert!(map_handle < 0);
        assert!(set_handle < 0);
        assert_eq!(unpack(map_handle), Ok((1, generation)));
        assert_eq!(unpack(set_handle), Ok((1, generation)));
    }

    #[test]
    fn lease_thread_identity_is_stable_and_survives_thread_exit_without_reuse() {
        let current = current_lease_thread_id();
        assert_eq!(current, current_lease_thread_id());
        let mut identities = std::collections::BTreeSet::from([current.0]);
        for _ in 0..32 {
            let identity = std::thread::spawn(|| {
                let first = current_lease_thread_id();
                assert_eq!(first, current_lease_thread_id());
                first
            })
            .join()
            .expect("lease identity worker");
            assert_ne!(identity.0, 0);
            assert!(identities.insert(identity.0), "thread identity was reused");
        }
    }

    #[test]
    fn lease_thread_identity_exhaustion_cannot_wrap() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(
            allocate_lease_thread_id(&counter),
            Some(LeaseThreadId(u64::MAX - 1))
        );
        assert_eq!(allocate_lease_thread_id(&counter), None);
        assert_eq!(allocate_lease_thread_id(&counter), None);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn map_root_operations_reject_live_lease_on_another_thread() {
        let handle = map_new_handle(empty_map());
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let worker = std::thread::spawn(move || {
            let lease = map_acquire(handle).unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(lease);
        });
        ready_rx.recv().unwrap();
        assert_eq!(
            map_root_set(handle, "busy".into(), 1),
            Err(HandleError::Reentrant)
        );
        assert_eq!(map_root_size(handle), Err(HandleError::Reentrant));
        assert_eq!(map_root_drop(handle), Err(HandleError::Reentrant));
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        map_root_drop(handle).unwrap();
    }

    #[test]
    fn zero_allocation_result_sets_observable_handle_error() {
        clear_handle_error();
        assert_eq!(record_alloc_result(0), 0);
        assert_eq!(mimi_handle_last_error(), HANDLE_ERR_INVALID);
    }

    #[test]
    fn map_root_slot_validation_rejects_stale_retired_and_busy() {
        let mut table = Table::new();
        let handle = alloc_slot(&mut table, empty_map());
        let (index, generation) = unpack(handle).unwrap();
        let slot = &table.slots[index as usize];
        assert_eq!(
            validate_map_root_slot(slot, generation + 1),
            Err(HandleError::StaleGeneration)
        );
        slot.pins.store(1, Ordering::SeqCst);
        assert_eq!(
            validate_map_root_slot(slot, generation),
            Err(HandleError::Reentrant)
        );
        slot.pins.store(0, Ordering::SeqCst);
        slot.retired.store(true, Ordering::SeqCst);
        assert_eq!(
            validate_map_root_slot(slot, generation),
            Err(HandleError::Destroyed)
        );
    }

    #[test]
    fn map_root_operations_reject_live_busy_and_old_generation() {
        let handle = map_new_handle(empty_map());
        let (index, generation) = unpack(handle).unwrap();
        {
            let mut table = lock_maps();
            let slot = &mut table.slots[index as usize];
            slot.active_owner = Some(current_lease_thread_id());
            slot.leases.store(1, Ordering::SeqCst);
        }
        assert_eq!(
            map_root_set(handle, "busy".into(), 1),
            Err(HandleError::Reentrant)
        );
        assert_eq!(map_root_size(handle), Err(HandleError::Reentrant));
        assert_eq!(map_root_drop(handle), Err(HandleError::Reentrant));
        {
            let mut table = lock_maps();
            let slot = &mut table.slots[index as usize];
            slot.active_owner = None;
            slot.leases.store(0, Ordering::SeqCst);
            slot.pins.store(1, Ordering::SeqCst);
        }
        assert_eq!(
            map_root_set(handle, "busy".into(), 1),
            Err(HandleError::Reentrant)
        );
        assert_eq!(map_root_size(handle), Err(HandleError::Reentrant));
        assert_eq!(map_root_drop(handle), Err(HandleError::Reentrant));
        lock_maps().slots[index as usize]
            .pins
            .store(0, Ordering::SeqCst);
        map_root_drop(handle).unwrap();
        let replacement = map_new_handle(empty_map());
        let (_, replacement_generation) = unpack(replacement).unwrap();
        assert!(replacement_generation > generation);
        assert_eq!(map_root_size(handle), Err(HandleError::StaleGeneration));
        map_root_drop(replacement).unwrap();
    }

    fn new_set() -> i64 {
        set_new_handle(super::super::MimiSet {
            inner: Default::default(),
            string_values: Default::default(),
        })
    }

    #[test]
    fn map_operation_lease_is_exclusive_and_reentry_fails_fast() {
        let handle = new_map();
        let lease = map_acquire(handle).unwrap();
        assert!(matches!(map_acquire(handle), Err(HandleError::Reentrant)));

        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let lease = map_acquire(handle).unwrap();
            acquired_tx.send(()).unwrap();
            drop(lease);
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(acquired_rx.recv_timeout(Duration::from_millis(30)).is_err());
        drop(lease);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
        map_destroy(handle).unwrap();
    }

    #[test]
    fn set_operation_lease_is_exclusive_and_reentry_fails_fast() {
        let handle = new_set();
        let lease = set_acquire(handle).unwrap();
        assert!(matches!(set_acquire(handle), Err(HandleError::Reentrant)));

        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let lease = set_acquire(handle).unwrap();
            acquired_tx.send(()).unwrap();
            drop(lease);
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(acquired_rx.recv_timeout(Duration::from_millis(30)).is_err());
        drop(lease);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
        set_destroy(handle).unwrap();
    }

    #[test]
    fn serializer_graph_scope_is_reentrant_and_does_not_block_other_handles() {
        let map = new_map();
        let set = new_set();
        let outer = json_container_scope(JsonContainerKind::Map, map).unwrap();
        let nested = json_container_scope(JsonContainerKind::Set, set).unwrap();
        assert!(json_container_scope(JsonContainerKind::Map, map).is_none());

        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _set = set_acquire(set).unwrap();
            acquired_tx.send(()).unwrap();
        });

        // Ordinary operations on unrelated handles do not inherit the
        // serializer-only graph gate.
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
        drop(nested);
        drop(outer);

        map_destroy(map).unwrap();
        set_destroy(set).unwrap();
    }

    #[test]
    fn unmatched_map_pin_release_cannot_steal_operation_lease() {
        let handle = new_map();
        assert_eq!(mimi_map_lease_release(handle), HANDLE_ERR_INVALID);
        let operation = map_acquire(handle).unwrap();
        assert_eq!(mimi_map_lease_release(handle), HANDLE_ERR_INVALID);
        assert_eq!(map_lease_count(handle), Ok(1));

        assert_eq!(mimi_map_lease_acquire(handle), HANDLE_OK);
        assert_eq!(map_lease_count(handle), Ok(2));
        map_begin_destroy(handle).unwrap();
        map_finish_destroy(handle).unwrap();
        drop(operation);
        assert_eq!(map_lease_count(handle), Ok(1));
        assert_eq!(mimi_map_lease_release(handle), HANDLE_OK);
        assert_eq!(map_generation(handle), Err(HandleError::StaleGeneration));
    }

    #[test]
    fn unmatched_set_pin_release_cannot_steal_operation_lease() {
        let handle = new_set();
        assert_eq!(mimi_set_lease_release(handle), HANDLE_ERR_INVALID);
        let operation = set_acquire(handle).unwrap();
        assert_eq!(mimi_set_lease_release(handle), HANDLE_ERR_INVALID);
        assert_eq!(set_lease_count(handle), Ok(1));

        assert_eq!(mimi_set_lease_acquire(handle), HANDLE_OK);
        assert_eq!(set_lease_count(handle), Ok(2));
        set_begin_destroy(handle).unwrap();
        set_finish_destroy(handle).unwrap();
        drop(operation);
        assert_eq!(set_lease_count(handle), Ok(1));
        assert_eq!(mimi_set_lease_release(handle), HANDLE_OK);
        assert_eq!(set_generation(handle), Err(HandleError::StaleGeneration));
    }

    #[test]
    fn c_lifetime_pin_can_be_released_by_another_thread() {
        let map = new_map();
        let set = new_set();
        assert_eq!(mimi_map_lease_acquire(map), HANDLE_OK);
        assert_eq!(mimi_set_lease_acquire(set), HANDLE_OK);

        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send((mimi_map_lease_release(map), mimi_set_lease_release(set)))
                .unwrap();
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            (HANDLE_OK, HANDLE_OK)
        );
        worker.join().unwrap();

        map_destroy(map).unwrap();
        set_destroy(set).unwrap();
    }

    #[test]
    fn exhausted_generation_retires_slot_instead_of_resurrecting_old_handles() {
        let first = new_map();
        let (index, _) = unpack(first).unwrap();
        {
            let mut table = lock_maps();
            table.slots[index as usize].generation = HandleGeneration::MAX;
        }
        let terminal = pack(index, HandleGeneration::MAX);
        map_destroy(terminal).unwrap();

        let next = new_map();
        assert_ne!(unpack(next).unwrap().0, index);
        assert!(matches!(map_acquire(terminal), Err(HandleError::Destroyed)));
        map_destroy(next).unwrap();
    }
}
