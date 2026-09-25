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

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Condvar, Mutex};

/// Per-handle generation. Distinct from Flow `TransitionEpoch`.
pub type HandleGeneration = u32;

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
    active_owner: Option<std::thread::ThreadId>,
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
        }
    }
}

fn pack(index: u32, gen: HandleGeneration) -> i64 {
    // low 32: index, high 32: generation. handle 0 remains invalid because
    // we never allocate index 0.
    ((gen as i64) << 32) | (index as i64)
}

fn unpack(handle: i64) -> Result<(u32, HandleGeneration), HandleError> {
    if handle == 0 {
        return Err(HandleError::Invalid);
    }
    let index = handle as u32;
    let gen = (handle >> 32) as HandleGeneration;
    if index == 0 {
        return Err(HandleError::Invalid);
    }
    Ok((index, gen))
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
    owner_depth: Mutex<Option<(std::thread::ThreadId, usize)>>,
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
    owner: std::thread::ThreadId,
    _not_send_or_sync: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl SerializerGraphLease {
    fn acquire() -> Self {
        let owner = std::thread::current().id();
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
    let gen: HandleGeneration = 1;
    if let Some(idx) = table.free.pop() {
        let slot = &mut table.slots[idx as usize];
        // generation was bumped on destroy; use the current value
        let g = slot.generation;
        slot.leases.store(0, Ordering::SeqCst);
        slot.active_owner = None;
        slot.pins.store(0, Ordering::SeqCst);
        slot.retired.store(false, Ordering::SeqCst);
        slot.pending_free.store(false, Ordering::SeqCst);
        slot.obj = Some(Box::new(obj));
        pack(idx, g)
    } else {
        let idx = table.slots.len() as u32;
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
    alloc_slot(&mut t, obj)
}

pub fn set_new_handle(obj: super::MimiSet) -> i64 {
    clear_handle_error();
    let mut t = lock_sets();
    alloc_slot(&mut t, obj)
}

pub fn map_acquire(handle: i64) -> Result<MapLease, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let current = std::thread::current().id();
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
    let current = std::thread::current().id();
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
        if slot.active_owner != Some(std::thread::current().id())
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
        finish_map_free(&mut t, index);
    }
    map_condvar().notify_all();
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
        if slot.active_owner != Some(std::thread::current().id())
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
        finish_map_free(&mut t, index);
    }
    map_condvar().notify_all();
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

fn finish_map_free(t: &mut Table<super::MimiMap>, index: u32) {
    let slot = &mut t.slots[index as usize];
    if let Some(map) = slot.obj.take() {
        // Map-owned payloads are Arc-backed; dropping this Map releases its
        // references and the payload Drop frees each allocation after the
        // last sibling clone is gone.
        drop(map);
    }
    let can_reuse = if let Some(next) = slot.generation.checked_add(1) {
        slot.generation = next;
        slot.retired.store(false, Ordering::SeqCst);
        true
    } else {
        // Never let a 32-bit generation wrap and make a very old handle live
        // again. This slot is permanently retired at exhaustion.
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
        finish_map_free(&mut t, index);
    } else {
        t.slots[index as usize]
            .pending_free
            .store(true, Ordering::SeqCst);
    }
    map_condvar().notify_all();
    clear_handle_error();
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
        map_new_handle(super::super::MimiMap {
            inner: Default::default(),
            owned: Default::default(),
        })
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
