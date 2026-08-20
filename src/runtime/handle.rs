//! Map/Set handle generation + per-op lease.
//!
//! 0.38.36 (B-HANDLE-001): handles are no longer raw Box addresses with a
//! "caller must not concurrent-destroy" contract. Each handle carries a
//! [`HandleGeneration`] (this name is **not** Flow `TransitionEpoch`).
//! Every op acquires a lease; destroy stops new leases, waits until
//! in-flight leases are zero, then advances generation and frees the object.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    Invalid,
    StaleGeneration,
    Destroyed,
}

impl HandleError {
    pub fn code(self) -> i32 {
        match self {
            HandleError::Invalid => HANDLE_ERR_INVALID,
            HandleError::StaleGeneration => HANDLE_ERR_STALE,
            HandleError::Destroyed => HANDLE_ERR_DESTROYED,
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
    leases: AtomicI64,
    /// Destroy has started: no new leases.
    retired: AtomicBool,
    /// Destroy is waiting for leases to reach zero (finish not yet run).
    pending_free: AtomicBool,
    obj: Option<Box<T>>,
}

struct Table<T: Send> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    cond: Condvar,
}

impl<T: Send> Table<T> {
    fn new() -> Self {
        // Index 0 is reserved so pack(0, _) is never a valid live handle
        // that collides with the C "null handle" value 0.
        Self {
            slots: vec![Slot {
                generation: 0,
                leases: AtomicI64::new(0),
                retired: AtomicBool::new(true),
                pending_free: AtomicBool::new(false),
                obj: None,
            }],
            free: Vec::new(),
            cond: Condvar::new(),
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

fn maps() -> &'static Mutex<Table<super::MimiMap>> {
    MAP_TABLE.get_or_init(|| Mutex::new(Table::new()))
}
fn sets() -> &'static Mutex<Table<super::MimiSet>> {
    SET_TABLE.get_or_init(|| Mutex::new(Table::new()))
}

fn lock_maps() -> std::sync::MutexGuard<'static, Table<super::MimiMap>> {
    maps().lock().unwrap_or_else(|e| e.into_inner())
}
fn lock_sets() -> std::sync::MutexGuard<'static, Table<super::MimiSet>> {
    sets().lock().unwrap_or_else(|e| e.into_inner())
}

fn alloc_slot<T: Send>(table: &mut Table<T>, obj: T) -> i64 {
    let gen: HandleGeneration = 1;
    let index = if let Some(idx) = table.free.pop() {
        let slot = &mut table.slots[idx as usize];
        // generation was bumped on destroy; use the current value
        let g = slot.generation;
        slot.leases.store(0, Ordering::SeqCst);
        slot.retired.store(false, Ordering::SeqCst);
        slot.pending_free.store(false, Ordering::SeqCst);
        slot.obj = Some(Box::new(obj));
        pack(idx, g)
    } else {
        let idx = table.slots.len() as u32;
        table.slots.push(Slot {
            generation: gen,
            leases: AtomicI64::new(0),
            retired: AtomicBool::new(false),
            pending_free: AtomicBool::new(false),
            obj: Some(Box::new(obj)),
        });
        pack(idx, gen)
    };
    index
}

pub(super) struct MapLease {
    handle: i64,
    ptr: *mut super::MimiMap,
}

impl MapLease {
    pub fn get(&self) -> &super::MimiMap {
        unsafe { &*self.ptr }
    }
    pub fn get_mut(&self) -> &mut super::MimiMap {
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
}

impl SetLease {
    pub fn get(&self) -> &super::MimiSet {
        unsafe { &*self.ptr }
    }
    pub fn get_mut(&self) -> &mut super::MimiSet {
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
    slot.leases.fetch_add(1, Ordering::SeqCst);
    let ptr = slot
        .obj
        .as_mut()
        .map(|b| &mut **b as *mut super::MimiMap)
        .ok_or(HandleError::Destroyed)?;
    clear_handle_error();
    Ok(MapLease { handle, ptr })
}

pub fn set_acquire(handle: i64) -> Result<SetLease, HandleError> {
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
    slot.leases.fetch_add(1, Ordering::SeqCst);
    let ptr = slot
        .obj
        .as_mut()
        .map(|b| &mut **b as *mut super::MimiSet)
        .ok_or(HandleError::Destroyed)?;
    clear_handle_error();
    Ok(SetLease { handle, ptr })
}

fn map_release(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    let (remaining, should_free, notify) = {
        let slot = t
            .slots
            .get_mut(index as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation != gen {
            return Err(HandleError::StaleGeneration);
        }
        let prev = slot.leases.fetch_sub(1, Ordering::SeqCst);
        let remaining = prev - 1;
        let should_free = remaining == 0 && slot.pending_free.load(Ordering::SeqCst);
        (remaining, should_free, remaining == 0)
    };
    if notify {
        t.cond.notify_all();
    }
    if should_free {
        finish_map_free(&mut t, index);
    }
    Ok(remaining)
}

fn set_release(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_sets();
    let (remaining, should_free, notify) = {
        let slot = t
            .slots
            .get_mut(index as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation != gen {
            return Err(HandleError::StaleGeneration);
        }
        let prev = slot.leases.fetch_sub(1, Ordering::SeqCst);
        let remaining = prev - 1;
        let should_free = remaining == 0 && slot.pending_free.load(Ordering::SeqCst);
        (remaining, should_free, remaining == 0)
    };
    if notify {
        t.cond.notify_all();
    }
    if should_free {
        finish_set_free(&mut t, index);
    }
    Ok(remaining)
}

fn finish_map_free(t: &mut Table<super::MimiMap>, index: u32) {
    let slot = &mut t.slots[index as usize];
    if let Some(map) = slot.obj.take() {
        for (vh, kind) in map.owned.iter() {
            super::free_map_owned_value(*vh, *kind);
        }
        drop(map);
    }
    slot.generation = slot.generation.wrapping_add(1);
    if slot.generation == 0 {
        slot.generation = 1;
    }
    slot.retired.store(false, Ordering::SeqCst);
    slot.pending_free.store(false, Ordering::SeqCst);
    slot.leases.store(0, Ordering::SeqCst);
    t.free.push(index);
}

fn finish_set_free(t: &mut Table<super::MimiSet>, index: u32) {
    let slot = &mut t.slots[index as usize];
    slot.obj.take();
    slot.generation = slot.generation.wrapping_add(1);
    if slot.generation == 0 {
        slot.generation = 1;
    }
    slot.retired.store(false, Ordering::SeqCst);
    slot.pending_free.store(false, Ordering::SeqCst);
    slot.leases.store(0, Ordering::SeqCst);
    t.free.push(index);
}

/// Stop new leases. Does not wait or free.
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
    Ok(())
}

/// Wait until leases are zero, bump generation, free the object.
pub fn map_finish_destroy(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_maps();
    loop {
        let (stale, gone, idle) = {
            let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
            (
                slot.generation != gen,
                slot.obj.is_none(),
                slot.leases.load(Ordering::SeqCst) == 0,
            )
        };
        if stale {
            return Err(HandleError::StaleGeneration);
        }
        if gone {
            return Ok(());
        }
        if idle {
            finish_map_free(&mut t, index);
            clear_handle_error();
            return Ok(());
        }
        // Same-thread simulate: last release finishes the free via pending_free.
        t.slots[index as usize]
            .pending_free
            .store(true, Ordering::SeqCst);
        t.cond.notify_all();
        clear_handle_error();
        return Ok(());
    }
}

pub fn set_finish_destroy(handle: i64) -> Result<(), HandleError> {
    let (index, gen) = unpack(handle)?;
    let mut t = lock_sets();
    let (stale, gone, idle) = {
        let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
        (
            slot.generation != gen,
            slot.obj.is_none(),
            slot.leases.load(Ordering::SeqCst) == 0,
        )
    };
    if stale {
        return Err(HandleError::StaleGeneration);
    }
    if gone {
        return Ok(());
    }
    if idle {
        finish_set_free(&mut t, index);
        clear_handle_error();
        return Ok(());
    }
    t.slots[index as usize]
        .pending_free
        .store(true, Ordering::SeqCst);
    t.cond.notify_all();
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
    Ok(slot.leases.load(Ordering::SeqCst))
}

pub fn set_lease_count(handle: i64) -> Result<i64, HandleError> {
    let (index, gen) = unpack(handle)?;
    let t = lock_sets();
    let slot = t.slots.get(index as usize).ok_or(HandleError::Invalid)?;
    if slot.generation != gen {
        return Err(HandleError::StaleGeneration);
    }
    Ok(slot.leases.load(Ordering::SeqCst))
}

pub fn with_map<R>(handle: i64, default: R, f: impl FnOnce(&mut super::MimiMap) -> R) -> R {
    match map_acquire(handle) {
        Ok(lease) => f(lease.get_mut()),
        Err(e) => {
            set_handle_error(e);
            default
        }
    }
}

pub fn with_set<R>(handle: i64, default: R, f: impl FnOnce(&mut super::MimiSet) -> R) -> R {
    match set_acquire(handle) {
        Ok(lease) => f(lease.get_mut()),
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
    match map_acquire(handle) {
        Ok(lease) => {
            // Leak the RAII guard: the matching release is explicit.
            std::mem::forget(lease);
            HANDLE_OK
        }
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_lease_release(handle: i64) -> i32 {
    match map_release(handle) {
        Ok(_) => HANDLE_OK,
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_lease_acquire(handle: i64) -> i32 {
    match set_acquire(handle) {
        Ok(lease) => {
            std::mem::forget(lease);
            HANDLE_OK
        }
        Err(e) => {
            set_handle_error(e);
            e.code()
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_lease_release(handle: i64) -> i32 {
    match set_release(handle) {
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
