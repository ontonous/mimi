//! Nominal generational handles with runtime ownership and concurrent leases.
//!
//! 0.31.31 (COMPONENT-HANDLE-001): Opaque resources crossing the component
//! boundary are never bare integers. A [`Handle`] carries:
//!
//! - a **kind** tag (which resource family it names),
//! - a **runtime** tag (which runtime owns the slot),
//! - a **generation** counter (bumped on every release so stale handles from
//!   an earlier occupant of the same slot are rejected — ABA defense),
//! - a **slot index** into the owning [`HandleRegistry`].
//!
//! Acquiring a handle grants **leases**. A slot cannot be destroyed while any
//! lease is outstanding; destruction bumps the generation so every previously
//! issued handle for that slot becomes stale.
//!
//! This is a pure-Rust, value-semantics state machine (Flow paradigm §20):
//! all mutation is guarded by a single lock and every operation returns a
//! [`Result`] rather than panicking. No `unsafe`.

use std::sync::Mutex;

/// The resource family a handle names.
///
/// Kind is part of handle identity: a `List` handle can never be used where a
/// `Map` handle is expected, even if the packed bits happen to collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandleKind {
    List,
    Map,
    Set,
    /// String / buffer resources.
    Buffer,
    /// Async task / future.
    Task,
    /// Long-lived callback subscription.
    Subscription,
    /// Opaque foreign resource (FFI, e.g. a C library context).
    Foreign,
}

impl HandleKind {
    /// Stable numeric tag (used for packing / wire identity).
    pub fn tag(self) -> u8 {
        match self {
            HandleKind::List => 1,
            HandleKind::Map => 2,
            HandleKind::Set => 3,
            HandleKind::Buffer => 4,
            HandleKind::Task => 5,
            HandleKind::Subscription => 6,
            HandleKind::Foreign => 7,
        }
    }
}

/// Which runtime owns the slot behind a handle.
///
/// A handle acquired from the interpreter runtime must not be redeemed against
/// the native (codegen) runtime, and vice versa — that is a category error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeId {
    /// Tree-walking interpreter runtime.
    Interp,
    /// Native (LLVM codegen) runtime.
    Native,
}

impl RuntimeId {
    pub fn tag(self) -> u8 {
        match self {
            RuntimeId::Interp => 1,
            RuntimeId::Native => 2,
        }
    }
}

/// A nominal generational handle.
///
/// `Copy` because a handle is a plain identity token, not the resource itself.
/// The registry — not the handle — enforces ownership and lease invariants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle {
    kind: HandleKind,
    runtime: RuntimeId,
    /// Slot index into the registry's slab.
    index: u32,
    /// Generation the handle was minted at.
    generation: u32,
}

impl Handle {
    pub fn kind(&self) -> HandleKind {
        self.kind
    }

    pub fn runtime(&self) -> RuntimeId {
        self.runtime
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Pack into a 64-bit opaque identifier for wire transport.
    ///
    /// Layout (MSB→LSB): `[kind:8][runtime:8][generation:16][index:32]`.
    /// This is an *identifier*, never a pointer — safe to serialize
    /// (COMPONENT-WIRE-001).
    pub fn to_u64(&self) -> u64 {
        ((self.kind.tag() as u64) << 56)
            | ((self.runtime.tag() as u64) << 48)
            | (((self.generation & 0xFFFF) as u64) << 32)
            | (self.index as u64)
    }

    /// Unpack from a 64-bit opaque identifier.
    ///
    /// Returns `None` if the kind or runtime tags are not recognized.
    /// The generation is truncated to 16 bits (matching `to_u64`).
    pub fn from_u64(packed: u64) -> Option<Self> {
        let kind_tag = (packed >> 56) as u8;
        let runtime_tag = ((packed >> 48) & 0xFF) as u8;
        let generation = ((packed >> 32) & 0xFFFF) as u32;
        let index = (packed & 0xFFFF_FFFF) as u32;

        let kind = match kind_tag {
            1 => HandleKind::List,
            2 => HandleKind::Map,
            3 => HandleKind::Set,
            4 => HandleKind::Buffer,
            5 => HandleKind::Task,
            6 => HandleKind::Subscription,
            7 => HandleKind::Foreign,
            _ => return None,
        };
        let runtime = match runtime_tag {
            1 => RuntimeId::Interp,
            2 => RuntimeId::Native,
            _ => return None,
        };

        Some(Handle {
            kind,
            runtime,
            index,
            generation,
        })
    }
}

/// Reasons a handle operation can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    /// Slot index out of range or never allocated.
    UnknownSlot,
    /// Generation mismatch: the handle refers to a freed/reused slot (ABA).
    StaleGeneration { expected: u32, got: u32 },
    /// Handle kind does not match the slot's kind.
    WrongKind {
        expected: HandleKind,
        got: HandleKind,
    },
    /// Handle runtime does not match the slot's owning runtime.
    WrongRuntime { expected: RuntimeId, got: RuntimeId },
    /// Attempted to destroy a slot that still has outstanding leases.
    LeasesOutstanding(u32),
    /// Attempted to release a lease on a slot with none outstanding.
    NoLease,
    /// Internal lock was poisoned.
    Poisoned,
}

impl std::fmt::Display for HandleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandleError::UnknownSlot => write!(f, "unknown handle slot"),
            HandleError::StaleGeneration { expected, got } => {
                write!(f, "stale handle: slot generation {expected}, handle {got}")
            }
            HandleError::WrongKind { expected, got } => {
                write!(
                    f,
                    "wrong handle kind: slot is {expected:?}, handle is {got:?}"
                )
            }
            HandleError::WrongRuntime { expected, got } => {
                write!(
                    f,
                    "wrong runtime: slot owned by {expected:?}, handle from {got:?}"
                )
            }
            HandleError::LeasesOutstanding(n) => {
                write!(f, "cannot destroy: {n} lease(s) outstanding")
            }
            HandleError::NoLease => write!(f, "no lease to release"),
            HandleError::Poisoned => write!(f, "handle registry lock poisoned"),
        }
    }
}

impl std::error::Error for HandleError {}

/// A registry slot: either occupied or free (holding the next generation).
#[derive(Debug)]
struct Slot {
    /// `None` when free. When free, `generation` holds the value to mint the
    /// *next* occupant with.
    kind: Option<HandleKind>,
    runtime: RuntimeId,
    generation: u32,
    /// Outstanding leases; slot cannot be destroyed while > 0.
    leases: u32,
}

/// A generational handle registry owned by one runtime.
///
/// Thread-safe: all mutation goes through an internal `Mutex`. Multiple
/// threads may `acquire`/`release_lease`/`destroy` concurrently; the
/// generation counter provides ABA safety.
#[derive(Debug)]
pub struct HandleRegistry {
    runtime: RuntimeId,
    inner: Mutex<RegistryInner>,
}

#[derive(Debug)]
struct RegistryInner {
    slots: Vec<Slot>,
    /// Indices of free slots available for reuse.
    free: Vec<u32>,
}

impl HandleRegistry {
    /// Create a registry owned by `runtime`.
    pub fn new(runtime: RuntimeId) -> Self {
        Self {
            runtime,
            inner: Mutex::new(RegistryInner {
                slots: Vec::new(),
                free: Vec::new(),
            }),
        }
    }

    /// Allocate a slot for a resource of `kind` and return a handle holding
    /// one lease.
    pub fn acquire(&self, kind: HandleKind) -> Result<Handle, HandleError> {
        let mut inner = self.inner.lock().map_err(|_| HandleError::Poisoned)?;
        let runtime = self.runtime;
        if let Some(index) = inner.free.pop() {
            let slot = &mut inner.slots[index as usize];
            slot.kind = Some(kind);
            slot.runtime = runtime;
            slot.leases = 1;
            let generation = slot.generation;
            Ok(Handle {
                kind,
                runtime,
                index,
                generation,
            })
        } else {
            let index = inner.slots.len() as u32;
            inner.slots.push(Slot {
                kind: Some(kind),
                runtime,
                generation: 0,
                leases: 1,
            });
            Ok(Handle {
                kind,
                runtime,
                index,
                generation: 0,
            })
        }
    }

    /// Validate a handle against its slot without changing lease count.
    fn resolve<'a>(inner: &'a mut RegistryInner, h: &Handle) -> Result<&'a mut Slot, HandleError> {
        let slot = inner
            .slots
            .get_mut(h.index as usize)
            .ok_or(HandleError::UnknownSlot)?;
        let kind = slot.kind.ok_or(HandleError::UnknownSlot)?;
        if slot.generation != h.generation {
            return Err(HandleError::StaleGeneration {
                expected: slot.generation,
                got: h.generation,
            });
        }
        if kind != h.kind {
            return Err(HandleError::WrongKind {
                expected: kind,
                got: h.kind,
            });
        }
        if slot.runtime != h.runtime {
            return Err(HandleError::WrongRuntime {
                expected: slot.runtime,
                got: h.runtime,
            });
        }
        Ok(slot)
    }

    /// Take an additional lease on a live handle.
    pub fn lease(&self, h: &Handle) -> Result<(), HandleError> {
        let mut inner = self.inner.lock().map_err(|_| HandleError::Poisoned)?;
        let slot = Self::resolve(&mut inner, h)?;
        slot.leases += 1;
        Ok(())
    }

    /// Release one lease. Returns the remaining lease count.
    pub fn release_lease(&self, h: &Handle) -> Result<u32, HandleError> {
        let mut inner = self.inner.lock().map_err(|_| HandleError::Poisoned)?;
        let slot = Self::resolve(&mut inner, h)?;
        if slot.leases == 0 {
            return Err(HandleError::NoLease);
        }
        slot.leases -= 1;
        Ok(slot.leases)
    }

    /// Number of outstanding leases on a handle's slot.
    pub fn lease_count(&self, h: &Handle) -> Result<u32, HandleError> {
        let mut inner = self.inner.lock().map_err(|_| HandleError::Poisoned)?;
        let slot = Self::resolve(&mut inner, h)?;
        Ok(slot.leases)
    }

    /// True if the handle is still live (valid generation, kind, runtime).
    pub fn is_live(&self, h: &Handle) -> bool {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        Self::resolve(&mut inner, h).is_ok()
    }

    /// Destroy the slot behind a handle. Fails if leases are outstanding.
    ///
    /// On success the generation is bumped so every previously minted handle
    /// for this slot becomes stale (ABA defense), and the slot is returned to
    /// the free list.
    pub fn destroy(&self, h: &Handle) -> Result<(), HandleError> {
        let mut inner = self.inner.lock().map_err(|_| HandleError::Poisoned)?;
        {
            let slot = Self::resolve(&mut inner, h)?;
            if slot.leases != 0 {
                return Err(HandleError::LeasesOutstanding(slot.leases));
            }
        }
        let index = h.index;
        let slot = &mut inner.slots[index as usize];
        slot.kind = None;
        slot.generation = slot.generation.wrapping_add(1);
        inner.free.push(index);
        Ok(())
    }

    /// Number of live (occupied) slots.
    pub fn live_count(&self) -> usize {
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return 0,
        };
        inner.slots.iter().filter(|s| s.kind.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_grants_one_lease() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let h = reg.acquire(HandleKind::List).unwrap();
        assert_eq!(h.kind(), HandleKind::List);
        assert_eq!(reg.lease_count(&h).unwrap(), 1);
        assert!(reg.is_live(&h));
    }

    #[test]
    fn destroy_requires_zero_leases() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let h = reg.acquire(HandleKind::Map).unwrap();
        // one lease outstanding
        assert_eq!(reg.destroy(&h), Err(HandleError::LeasesOutstanding(1)));
        assert_eq!(reg.release_lease(&h).unwrap(), 0);
        assert!(reg.destroy(&h).is_ok());
        assert!(!reg.is_live(&h));
    }

    #[test]
    fn stale_generation_rejected_aba() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let h1 = reg.acquire(HandleKind::Set).unwrap();
        reg.release_lease(&h1).unwrap();
        reg.destroy(&h1).unwrap();
        // reuse the slot; new occupant gets a bumped generation
        let h2 = reg.acquire(HandleKind::Set).unwrap();
        assert_ne!(h1.generation(), h2.generation());
        // old handle is now stale
        assert!(matches!(
            reg.lease_count(&h1),
            Err(HandleError::StaleGeneration { .. })
        ));
        // new handle works
        assert_eq!(reg.lease_count(&h2).unwrap(), 1);
    }

    #[test]
    fn wrong_kind_rejected() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let h = reg.acquire(HandleKind::List).unwrap();
        // forge a handle with the same slot/generation but wrong kind
        let forged = Handle {
            kind: HandleKind::Map,
            runtime: RuntimeId::Interp,
            index: 0,
            generation: h.generation(),
        };
        assert!(matches!(
            reg.lease_count(&forged),
            Err(HandleError::WrongKind { .. })
        ));
    }

    #[test]
    fn wrong_runtime_rejected() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let h = reg.acquire(HandleKind::Foreign).unwrap();
        let forged = Handle {
            kind: HandleKind::Foreign,
            runtime: RuntimeId::Native,
            index: 0,
            generation: h.generation(),
        };
        assert!(matches!(
            reg.lease_count(&forged),
            Err(HandleError::WrongRuntime { .. })
        ));
    }

    #[test]
    fn multiple_leases_block_destroy() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let h = reg.acquire(HandleKind::Buffer).unwrap();
        reg.lease(&h).unwrap(); // now 2
        assert_eq!(reg.lease_count(&h).unwrap(), 2);
        assert_eq!(reg.release_lease(&h).unwrap(), 1);
        assert_eq!(reg.destroy(&h), Err(HandleError::LeasesOutstanding(1)));
        assert_eq!(reg.release_lease(&h).unwrap(), 0);
        assert!(reg.destroy(&h).is_ok());
    }

    #[test]
    fn release_without_lease_rejected() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let h = reg.acquire(HandleKind::Task).unwrap();
        reg.release_lease(&h).unwrap();
        assert_eq!(reg.release_lease(&h), Err(HandleError::NoLease));
    }

    #[test]
    fn unknown_slot_rejected() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let forged = Handle {
            kind: HandleKind::List,
            runtime: RuntimeId::Interp,
            index: 999,
            generation: 0,
        };
        assert_eq!(reg.lease_count(&forged), Err(HandleError::UnknownSlot));
    }

    #[test]
    fn to_u64_encodes_identity_not_pointer() {
        let reg = HandleRegistry::new(RuntimeId::Native);
        let h = reg.acquire(HandleKind::Map).unwrap();
        let packed = h.to_u64();
        // kind tag in top byte
        assert_eq!((packed >> 56) as u8, HandleKind::Map.tag());
        assert_eq!(((packed >> 48) & 0xFF) as u8, RuntimeId::Native.tag());
    }

    #[test]
    fn from_u64_roundtrip() {
        let reg = HandleRegistry::new(RuntimeId::Native);
        let h = reg.acquire(HandleKind::Foreign).unwrap();
        let packed = h.to_u64();
        let unpacked = Handle::from_u64(packed).expect("should unpack");
        assert_eq!(unpacked.kind(), HandleKind::Foreign);
        assert_eq!(unpacked.runtime(), RuntimeId::Native);
        assert_eq!(unpacked.generation(), h.generation());
        assert_eq!(unpacked, h);
    }

    #[test]
    fn from_u64_rejects_bad_kind() {
        // kind tag 0 is invalid
        let packed: u64 = 0x0002_0000_0000_0000;
        assert!(Handle::from_u64(packed).is_none());
    }

    #[test]
    fn from_u64_rejects_bad_runtime() {
        // runtime tag 0 is invalid
        let packed: u64 = 0x0100_0000_0000_0000;
        assert!(Handle::from_u64(packed).is_none());
    }

    #[test]
    fn slot_reuse_from_free_list() {
        let reg = HandleRegistry::new(RuntimeId::Interp);
        let h1 = reg.acquire(HandleKind::List).unwrap();
        reg.release_lease(&h1).unwrap();
        reg.destroy(&h1).unwrap();
        assert_eq!(reg.live_count(), 0);
        let h2 = reg.acquire(HandleKind::Map).unwrap();
        // reused the same slot index
        assert_eq!(h1.index, h2.index);
        assert_eq!(reg.live_count(), 1);
    }

    #[test]
    fn concurrent_acquire_release_no_corruption() {
        use std::sync::Arc;
        use std::thread;
        let reg = Arc::new(HandleRegistry::new(RuntimeId::Native));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let reg = Arc::clone(&reg);
            threads.push(thread::spawn(move || {
                for _ in 0..500 {
                    let h = reg.acquire(HandleKind::Foreign).unwrap();
                    reg.lease(&h).unwrap();
                    reg.release_lease(&h).unwrap();
                    reg.release_lease(&h).unwrap();
                    reg.destroy(&h).unwrap();
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        // all slots freed
        assert_eq!(reg.live_count(), 0);
    }
}
