//! Flow `TransitionEpoch` — packed at escape, stripped locally.
//!
//! 0.38.46 (Phase C / Q1 S): every Flow value conceptually carries a
//! [`TransitionEpoch`]. This name is **not** Map/Set [`HandleGeneration`].
//!
//! Clause 5.1 silent stay: a local self-loop never leaves the turn/actor, so
//! lowering strips the epoch (plain record, no atomic tax). Crossing Channel,
//! FFI, or an actor mailbox must [`mimi_flow_pack`]. A peer that still holds
//! an older epoch gets a typed [`EpochError::Stale`], not a silent alias.
//!
//! `recover` of an escaped Flow is [`mimi_flow_bump_epoch`]: consume the
//! current packed generation and publish a new epoch. Reusing the payload
//! buffer is an optimisation; observers still see a new epoch.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Per-instance Flow generation. Distinct from Map/Set `HandleGeneration`.
pub type TransitionEpoch = u64;

/// Initial epoch assigned at first pack (escape). 0 is never a live epoch.
pub const EPOCH_INITIAL: TransitionEpoch = 1;

pub const EPOCH_OK: i32 = 0;
pub const EPOCH_ERR_INVALID: i32 = 1;
pub const EPOCH_ERR_STALE: i32 = 2;
pub const EPOCH_ERR_BARE_RECORD: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochError {
    Invalid,
    Stale,
    BareRecord,
}

impl EpochError {
    pub fn code(self) -> i32 {
        match self {
            EpochError::Invalid => EPOCH_ERR_INVALID,
            EpochError::Stale => EPOCH_ERR_STALE,
            EpochError::BareRecord => EPOCH_ERR_BARE_RECORD,
        }
    }
}

thread_local! {
    static LAST_EPOCH_ERROR: Cell<i32> = const { Cell::new(EPOCH_OK) };
    static PACK_COUNT: Cell<u64> = const { Cell::new(0) };
}

/// Process-wide counter so a freshly exec'd native binary still starts at 0
/// and dual-backend tests can observe "no pack happened" via a delta.
static PACK_COUNT_GLOBAL: AtomicU64 = AtomicU64::new(0);

pub fn set_epoch_error(err: EpochError) {
    LAST_EPOCH_ERROR.with(|c| c.set(err.code()));
}

pub fn clear_epoch_error() {
    LAST_EPOCH_ERROR.with(|c| c.set(EPOCH_OK));
}

#[no_mangle]
pub extern "C" fn mimi_flow_last_error() -> i32 {
    LAST_EPOCH_ERROR.with(|c| c.get())
}

#[no_mangle]
pub extern "C" fn mimi_flow_pack_count() -> i64 {
    PACK_COUNT.with(|c| c.get() as i64)
}

fn bump_pack_count() {
    PACK_COUNT.with(|c| c.set(c.get().saturating_add(1)));
    PACK_COUNT_GLOBAL.fetch_add(1, Ordering::Relaxed);
}

struct Slot {
    epoch: TransitionEpoch,
    payload: i64,
    live: bool,
}

struct Table {
    slots: Vec<Slot>,
    free: Vec<u32>,
}

impl Table {
    fn new() -> Self {
        // Index 0 is reserved so pack(0, _) never collides with null handle 0.
        Self {
            slots: vec![Slot {
                epoch: 0,
                payload: 0,
                live: false,
            }],
            free: Vec::new(),
        }
    }
}

fn pack(index: u32, epoch: TransitionEpoch) -> i64 {
    let tag = epoch as u32;
    ((tag as i64) << 32) | (index as i64)
}

fn unpack(handle: i64) -> Result<(u32, u32), EpochError> {
    if handle == 0 {
        return Err(EpochError::Invalid);
    }
    let index = handle as u32;
    let tag = (handle >> 32) as u32;
    if index == 0 {
        return Err(EpochError::Invalid);
    }
    Ok((index, tag))
}

static FLOW_TABLE: std::sync::OnceLock<Mutex<Table>> = std::sync::OnceLock::new();

fn table() -> &'static Mutex<Table> {
    FLOW_TABLE.get_or_init(|| Mutex::new(Table::new()))
}

fn lock_table() -> std::sync::MutexGuard<'static, Table> {
    table().lock().unwrap_or_else(|e| e.into_inner())
}

fn lookup<'a>(tbl: &'a Table, handle: i64) -> Result<(u32, &'a Slot), EpochError> {
    let (index, tag) = unpack(handle)?;
    let slot = tbl.slots.get(index as usize).ok_or(EpochError::Invalid)?;
    if !slot.live {
        return Err(EpochError::Stale);
    }
    if (slot.epoch as u32) != tag {
        return Err(EpochError::Stale);
    }
    Ok((index, slot))
}

/// Pack a payload token with a fresh [`TransitionEpoch`]. This is the only
/// way a Flow value may cross Channel / FFI / actor mailbox.
#[no_mangle]
pub extern "C" fn mimi_flow_pack(payload: i64) -> i64 {
    clear_epoch_error();
    bump_pack_count();
    let mut tbl = lock_table();
    let index = if let Some(free) = tbl.free.pop() {
        free
    } else {
        let i = tbl.slots.len() as u32;
        tbl.slots.push(Slot {
            epoch: 0,
            payload: 0,
            live: false,
        });
        i
    };
    let slot = &mut tbl.slots[index as usize];
    slot.epoch = EPOCH_INITIAL;
    slot.payload = payload;
    slot.live = true;
    pack(index, slot.epoch)
}

/// Current epoch of a packed Flow. 0 and [`mimi_flow_last_error`] on failure.
#[no_mangle]
pub extern "C" fn mimi_flow_epoch(handle: i64) -> i64 {
    clear_epoch_error();
    let tbl = lock_table();
    match lookup(&tbl, handle) {
        Ok((_, slot)) => slot.epoch as i64,
        Err(err) => {
            set_epoch_error(err);
            0
        }
    }
}

/// Accept a packed Flow only if `expected` equals the live epoch.
/// Peer holding an older epoch → [`EPOCH_ERR_STALE`].
#[no_mangle]
pub extern "C" fn mimi_flow_check_epoch(handle: i64, expected: i64) -> i32 {
    clear_epoch_error();
    let tbl = lock_table();
    match lookup(&tbl, handle) {
        Ok((_, slot)) => {
            if slot.epoch != expected as TransitionEpoch {
                set_epoch_error(EpochError::Stale);
                EPOCH_ERR_STALE
            } else {
                EPOCH_OK
            }
        }
        Err(err) => {
            set_epoch_error(err);
            err.code()
        }
    }
}

fn lookup_mut(tbl: &mut Table, handle: i64) -> Result<(u32, &mut Slot), EpochError> {
    let (index, tag) = unpack(handle)?;
    let slot = tbl
        .slots
        .get_mut(index as usize)
        .ok_or(EpochError::Invalid)?;
    if !slot.live {
        return Err(EpochError::Stale);
    }
    if (slot.epoch as u32) != tag {
        return Err(EpochError::Stale);
    }
    Ok((index, slot))
}

/// `recover` of an escaped Flow: consume the current epoch, publish `+1`.
/// Returns the new handle (same slot, new epoch). 0 on failure.
#[no_mangle]
pub extern "C" fn mimi_flow_bump_epoch(handle: i64) -> i64 {
    clear_epoch_error();
    let mut tbl = lock_table();
    match lookup_mut(&mut tbl, handle) {
        Ok((index, slot)) => {
            slot.epoch = slot.epoch.saturating_add(1);
            pack(index, slot.epoch)
        }
        Err(err) => {
            set_epoch_error(err);
            0
        }
    }
}

/// Read the payload token. Does not consume the packed Flow.
#[no_mangle]
pub extern "C" fn mimi_flow_unpack(handle: i64) -> i64 {
    clear_epoch_error();
    let tbl = lock_table();
    match lookup(&tbl, handle) {
        Ok((_, slot)) => slot.payload,
        Err(err) => {
            set_epoch_error(err);
            0
        }
    }
}

/// Drop a packed Flow. Subsequent use of the handle is [`EPOCH_ERR_STALE`].
#[no_mangle]
pub extern "C" fn mimi_flow_drop(handle: i64) -> i32 {
    clear_epoch_error();
    let mut tbl = lock_table();
    let index = match lookup_mut(&mut tbl, handle) {
        Ok((index, slot)) => {
            slot.live = false;
            slot.epoch = slot.epoch.saturating_add(1);
            slot.payload = 0;
            index
        }
        Err(err) => {
            set_epoch_error(err);
            return err.code();
        }
    };
    tbl.free.push(index);
    EPOCH_OK
}

/// FFI must not treat a bare Flow record pointer as a safe handle.
/// Any non-packed token is [`EPOCH_ERR_BARE_RECORD`].
#[no_mangle]
pub extern "C" fn mimi_flow_reject_bare_record(_raw: i64) -> i32 {
    set_epoch_error(EpochError::BareRecord);
    EPOCH_ERR_BARE_RECORD
}
