//! Storage and descriptor registry for the unified ManagedAnyPayloadV2
//! envelope. This file is staged under ignored devdocs only.
//!
//! The compiler validates the recursive TypeDesc closure and binds its
//! canonical BLAKE3 hash to generated recursive clone/drop glue. The runtime
//! receives that exact identity plus trusted native callback pointers; it
//! cannot reconstruct a TypeDesc closure from the hash alone. A descriptor
//! registration pins one immutable pair of callbacks under the complete
//! `(tag, type_id, hash, size, align)` key. The canonical Core i32 scalar is
//! the narrow exception: the runtime replaces supplied callbacks with its
//! fixed memcpy/no-op pair before registration. Other exact keys cannot be
//! rebound to different function pointers.

use std::alloc::{alloc, dealloc, Layout};
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(standalone)]
use super::libc as system_libc;
#[cfg(not(standalone))]
use libc as system_libc;

use super::blake3::Blake3Hasher;

pub(super) const MANAGED_ANY_PAYLOAD_V2_VERSION: u32 = 2;
pub(super) const TYPE_ID_BYTES: usize = 35;
pub(super) const DESCRIPTOR_HASH_BYTES: usize = 32;
pub(super) const SELECTION_SCHEMA_VERSION: u32 = 1;
const REGISTRY_HASH_DOMAIN: &[u8] = b"mimi-managed-any-payload-v2:admitted-descriptor-set:v1\n";
/// The one Core i32 TypeDesc admitted as the ordinary Map/Any V2 miss value.
/// The hash is the V2 recursive TypeDesc-closure digest for the canonical
/// Copy scalar descriptor emitted by `MirTypeCatalog`.
pub(super) const CANONICAL_I32_TYPE_ID: &[u8; TYPE_ID_BYTES] =
    b"rt:1eb1c4ed966863450a50a89ef0d38924";
pub(super) const CANONICAL_I32_TYPE_DESC_HASH: [u8; DESCRIPTOR_HASH_BYTES] = [
    0xf2, 0x7c, 0x00, 0x72, 0x7c, 0x62, 0x5b, 0x72, 0xf9, 0xe7, 0x8b, 0x51, 0x7a, 0x2e, 0xe5, 0x73,
    0x1d, 0xf2, 0xec, 0x6d, 0x17, 0xdd, 0xb4, 0xc1, 0xd6, 0xe2, 0xcf, 0x87, 0x5e, 0x40, 0x17, 0x5b,
];

/// Stable tag values in the owner envelope. Aggregate payloads use the same
/// pinned descriptor/callback mechanism as scalars; their compiler-generated
/// recursive glue owns rollback of any children cloned before a failure.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ManagedAnyPayloadV2Tag {
    I32 = 1,
    String = 2,
    Bool = 3,
    F64 = 4,
    Aggregate = 5,
}

impl TryFrom<i32> for ManagedAnyPayloadV2Tag {
    type Error = ManagedAnyPayloadV2Error;

    fn try_from(tag: i32) -> Result<Self, Self::Error> {
        match tag {
            1 => Ok(Self::I32),
            2 => Ok(Self::String),
            3 => Ok(Self::Bool),
            4 => Ok(Self::F64),
            5 => Ok(Self::Aggregate),
            _ => Err(ManagedAnyPayloadV2Error::InvalidTag),
        }
    }
}

/// Exactly 35 ASCII bytes: `rt:` followed by 32 lower-case hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct ManagedAnyPayloadV2TypeId([u8; TYPE_ID_BYTES]);

impl ManagedAnyPayloadV2TypeId {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, ManagedAnyPayloadV2Error> {
        if bytes.len() != TYPE_ID_BYTES
            || !bytes.starts_with(b"rt:")
            || !bytes[3..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(ManagedAnyPayloadV2Error::InvalidTypeId);
        }
        let mut canonical = [0; TYPE_ID_BYTES];
        canonical.copy_from_slice(bytes);
        Ok(Self(canonical))
    }

    pub(super) const fn as_bytes(&self) -> &[u8; TYPE_ID_BYTES] {
        &self.0
    }
}

/// Return whether an ABI identity is the complete canonical Core i32 scalar
/// descriptor, including its runtime layout. This is shared by miss admission
/// and callback canonicalization so neither boundary can accept a looser
/// identity than the other.
pub(super) fn is_canonical_i32_identity(identity: ManagedAnyPayloadV2Identity) -> bool {
    has_canonical_i32_type_desc(identity)
        && identity.size == std::mem::size_of::<i32>()
        && identity.align == std::mem::align_of::<i32>()
}

fn has_canonical_i32_type_desc(identity: ManagedAnyPayloadV2Identity) -> bool {
    identity.tag == ManagedAnyPayloadV2Tag::I32
        && identity.type_id.as_bytes() == CANONICAL_I32_TYPE_ID
        && identity.descriptor_hash == CANONICAL_I32_TYPE_DESC_HASH
}

/// Exact payload descriptor identity. `size` and `align` are the source
/// TypeDesc physical layout, not just the allocator's rounded allocation size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct ManagedAnyPayloadV2Identity {
    pub(super) tag: ManagedAnyPayloadV2Tag,
    pub(super) type_id: ManagedAnyPayloadV2TypeId,
    pub(super) descriptor_hash: [u8; DESCRIPTOR_HASH_BYTES],
    pub(super) size: usize,
    pub(super) align: usize,
}

/// Recursive Clone glue ABI: 0 means fully initialized; nonzero means the
/// destination is wholly uninitialized and owns no resource, including
/// recursively cloned children. Glue must not unwind and must be safe to call
/// concurrently for independent owners.
pub(super) type ManagedAnyPayloadV2CloneGlue =
    unsafe extern "C" fn(destination: *mut u8, source: *const u8) -> i32;

/// Recursive Drop glue ABI: must release the complete initialized payload,
/// never panic/unwind, and be safe to call concurrently on independent owners.
pub(super) type ManagedAnyPayloadV2DropGlue = unsafe extern "C" fn(payload: *mut u8);

#[derive(Clone, Copy)]
pub(super) struct ManagedAnyPayloadV2Glue {
    pub(super) clone: ManagedAnyPayloadV2CloneGlue,
    pub(super) drop: ManagedAnyPayloadV2DropGlue,
}

/// Immutable callback authority, pinned in the descriptor registry and by
/// every live payload. The runtime trusts generated callbacks after compiler
/// validation; an exported C ABI cannot authenticate arbitrary function
/// pointers supplied by an adversarial host.
struct ManagedAnyPayloadV2Descriptor {
    version: u32,
    identity: ManagedAnyPayloadV2Identity,
    layout: Layout,
    glue: ManagedAnyPayloadV2Glue,
}

/// A borrowed view of the canonical descriptor-selection frame passed to
/// heterogeneous Map Set and collection operations. The raw frame stays
/// caller-owned for the duration of the helper; parsing and membership checks
/// do not allocate or retain pointers after the call.
#[derive(Debug)]
pub(super) struct ManagedAnyPayloadV2Selection<'a> {
    records: &'a [u8],
    count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManagedAnyPayloadV2SelectionError {
    InvalidEncoding,
    RegistryHashMismatch,
}

#[derive(Clone, Copy)]
struct SelectionRecord<'a> {
    type_id: ManagedAnyPayloadV2TypeId,
    tag: ManagedAnyPayloadV2Tag,
    descriptor_hash: &'a [u8; DESCRIPTOR_HASH_BYTES],
}

impl<'a> ManagedAnyPayloadV2Selection<'a> {
    /// Validate schema v1 bytes and the Core registry hash. Empty is allowed
    /// only for read/serialization operations whose root may contain no Any
    /// values; Set always requires a non-empty descriptor authority.
    pub(super) fn parse(
        bytes: &'a [u8],
        expected_hash: &[u8; DESCRIPTOR_HASH_BYTES],
        allow_empty: bool,
    ) -> Result<Self, ManagedAnyPayloadV2SelectionError> {
        if bytes.len() < 8 {
            return Err(ManagedAnyPayloadV2SelectionError::InvalidEncoding);
        }
        let version = u32::from_be_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|_| ManagedAnyPayloadV2SelectionError::InvalidEncoding)?,
        );
        let count = u32::from_be_bytes(
            bytes[4..8]
                .try_into()
                .map_err(|_| ManagedAnyPayloadV2SelectionError::InvalidEncoding)?,
        ) as usize;
        if version != SELECTION_SCHEMA_VERSION || (count == 0 && !allow_empty) {
            return Err(ManagedAnyPayloadV2SelectionError::InvalidEncoding);
        }

        // The shortest canonical record is the 8-byte ID length, 35-byte ID,
        // 4-byte tag, and 32-byte hash. Bound the count by input length before
        // iterating so a forged count cannot cause unbounded work.
        if count > (bytes.len() - 8) / (8 + TYPE_ID_BYTES + 4 + DESCRIPTOR_HASH_BYTES) {
            return Err(ManagedAnyPayloadV2SelectionError::InvalidEncoding);
        }

        let selection = Self {
            records: &bytes[8..],
            count,
        };
        let mut cursor = 0usize;
        let mut previous: Option<ManagedAnyPayloadV2TypeId> = None;
        for _ in 0..count {
            let (record, next) = read_selection_record(selection.records, cursor)
                .ok_or(ManagedAnyPayloadV2SelectionError::InvalidEncoding)?;
            if previous.is_some_and(|prior| prior.as_bytes() >= record.type_id.as_bytes()) {
                return Err(ManagedAnyPayloadV2SelectionError::InvalidEncoding);
            }
            previous = Some(record.type_id);
            cursor = next;
        }
        if cursor != selection.records.len() {
            return Err(ManagedAnyPayloadV2SelectionError::InvalidEncoding);
        }

        let mut hasher = Blake3Hasher::new();
        hasher.update(REGISTRY_HASH_DOMAIN);
        hasher.update(selection.records);
        if &hasher.finalize() != expected_hash {
            return Err(ManagedAnyPayloadV2SelectionError::RegistryHashMismatch);
        }
        Ok(selection)
    }

    pub(super) fn contains(&self, identity: ManagedAnyPayloadV2Identity) -> bool {
        let mut cursor = 0usize;
        for _ in 0..self.count {
            let Some((record, next)) = read_selection_record(self.records, cursor) else {
                return false;
            };
            if record.type_id == identity.type_id
                && record.tag == identity.tag
                && record.descriptor_hash == &identity.descriptor_hash
            {
                return true;
            }
            cursor = next;
        }
        false
    }
}

fn read_selection_record(bytes: &[u8], cursor: usize) -> Option<(SelectionRecord<'_>, usize)> {
    let id_len_end = cursor.checked_add(8)?;
    let id_len_bytes: [u8; 8] = bytes.get(cursor..id_len_end)?.try_into().ok()?;
    let id_len = usize::try_from(u64::from_be_bytes(id_len_bytes)).ok()?;
    if id_len != TYPE_ID_BYTES {
        return None;
    }
    let id_end = id_len_end.checked_add(id_len)?;
    let type_id = ManagedAnyPayloadV2TypeId::parse(bytes.get(id_len_end..id_end)?).ok()?;
    let tag_end = id_end.checked_add(4)?;
    let tag_code = i32::from_be_bytes(bytes.get(id_end..tag_end)?.try_into().ok()?);
    let tag = ManagedAnyPayloadV2Tag::try_from(tag_code).ok()?;
    let hash_end = tag_end.checked_add(DESCRIPTOR_HASH_BYTES)?;
    let descriptor_hash: &[u8; DESCRIPTOR_HASH_BYTES] =
        bytes.get(tag_end..hash_end)?.try_into().ok()?;
    Some((
        SelectionRecord {
            type_id,
            tag,
            descriptor_hash,
        },
        hash_end,
    ))
}

static DESCRIPTORS: OnceLock<
    Mutex<HashMap<ManagedAnyPayloadV2Identity, Arc<ManagedAnyPayloadV2Descriptor>>>,
> = OnceLock::new();

/// Release the process-global descriptor table before the native process
/// exits.  The table is intentionally shared by independent V2 owners during
/// execution, but leaving its HashMap bucket and Arc descriptor allocations
/// behind turns a clean program into Memcheck `possibly lost` evidence: an Arc
/// stores an interior pointer to its allocation, which Valgrind cannot classify
/// as definitely reachable through the OnceLock.  This callback only runs at
/// process teardown, after generated code has dropped every live root/token;
/// normal runtime calls never observe it concurrently with user execution.
pub(super) extern "C" fn clear_descriptors_at_exit() {
    let Some(table) = DESCRIPTORS.get() else {
        return;
    };
    if let Ok(mut registry) = table.lock() {
        registry.clear();
        registry.shrink_to_fit();
    }
}

fn descriptors(
) -> &'static Mutex<HashMap<ManagedAnyPayloadV2Identity, Arc<ManagedAnyPayloadV2Descriptor>>> {
    DESCRIPTORS.get_or_init(|| {
        // SAFETY: the callback has C ABI, takes no arguments, and consults
        // the same OnceLock without allocating.  Registering it while the
        // table is first initialized guarantees one callback per process.
        unsafe {
            let _ = system_libc::atexit(clear_descriptors_at_exit);
        }
        Mutex::new(HashMap::new())
    })
}

fn register_descriptor(
    identity: ManagedAnyPayloadV2Identity,
    requested_glue: ManagedAnyPayloadV2Glue,
) -> Result<Arc<ManagedAnyPayloadV2Descriptor>, ManagedAnyPayloadV2Error> {
    // Core i32 is a Copy scalar with fixed memcpy/no-op ownership behavior.
    // Do not let the first caller of this public runtime ABI pin arbitrary
    // callbacks under the canonical i32 TypeDesc identity. Canonicalize
    // before both the existing-key comparison and first insertion.
    let glue = if has_canonical_i32_type_desc(identity) {
        if identity.size != std::mem::size_of::<i32>()
            || identity.align != std::mem::align_of::<i32>()
        {
            return Err(ManagedAnyPayloadV2Error::TypeMismatch);
        }
        canonical_i32_glue()
    } else {
        requested_glue
    };
    let layout = checked_layout(identity.size, identity.align)?;
    let mut registry = descriptors()
        .lock()
        .map_err(|_| ManagedAnyPayloadV2Error::Poisoned)?;
    if let Some(pinned) = registry.get(&identity) {
        if !same_glue(pinned.glue, glue) {
            return Err(ManagedAnyPayloadV2Error::CallbackConflict);
        }
        return Ok(Arc::clone(pinned));
    }

    registry
        .try_reserve(1)
        .map_err(|_| ManagedAnyPayloadV2Error::Capacity)?;
    let descriptor = Arc::new(ManagedAnyPayloadV2Descriptor {
        version: MANAGED_ANY_PAYLOAD_V2_VERSION,
        identity,
        layout,
        glue,
    });
    registry.insert(identity, Arc::clone(&descriptor));
    Ok(descriptor)
}

pub(super) fn canonical_i32_glue() -> ManagedAnyPayloadV2Glue {
    ManagedAnyPayloadV2Glue {
        clone: clone_canonical_i32,
        drop: drop_canonical_i32,
    }
}

unsafe extern "C" fn clone_canonical_i32(destination: *mut u8, source: *const u8) -> i32 {
    // SAFETY: callers reach this glue only through the exact Core i32
    // descriptor, and the owner runtime supplies checked 4-byte layouts.
    unsafe { std::ptr::copy_nonoverlapping(source.cast::<i32>(), destination.cast::<i32>(), 1) };
    0
}

unsafe extern "C" fn drop_canonical_i32(_payload: *mut u8) {}

fn same_glue(left: ManagedAnyPayloadV2Glue, right: ManagedAnyPayloadV2Glue) -> bool {
    // Function pointer casts compare the executable addresses supplied by the
    // trusted compiler-native caller. This is identity consistency, not
    // authentication of an arbitrary C host pointer.
    left.clone as usize == right.clone as usize && left.drop as usize == right.drop as usize
}

/// One private aligned copy of a typed Any payload. Its descriptor, identity,
/// and callbacks stay pinned together through every owner clone and drop.
pub(super) struct ManagedAnyPayloadV2 {
    descriptor: Arc<ManagedAnyPayloadV2Descriptor>,
    payload: NonNull<u8>,
}

// SAFETY: compiler whole-program preflight admits only immutable, non-linear,
// recursively cloneable payloads. Payload bytes become immutable after
// successful construction. Pinned generated callbacks are required to
// read/clone independent values concurrently and drop them on any freeing
// thread without TLS dependence; canonical Core i32 uses runtime-owned glue.
// Each callback runs only after an Arc snapshot pins the descriptor and
// allocation; the runtime never shares a mutable payload pointer with a
// caller. For other identities, runtime cannot infer linearity or authenticate
// arbitrary C pointers from the raw hash, so those invariants rely on the
// reserved compiler-native ABI gate.
unsafe impl Send for ManagedAnyPayloadV2 {}
unsafe impl Sync for ManagedAnyPayloadV2 {}

impl ManagedAnyPayloadV2 {
    /// Clone a borrowed input into a new private owner allocation. Callbacks
    /// execute only after descriptor registration has released its lock.
    pub(super) unsafe fn clone_from_borrowed(
        identity: ManagedAnyPayloadV2Identity,
        glue: ManagedAnyPayloadV2Glue,
        source: *const u8,
    ) -> Result<Self, ManagedAnyPayloadV2Error> {
        checked_layout(identity.size, identity.align)?;
        if source.is_null() || !(source as usize).is_multiple_of(identity.align) {
            return Err(ManagedAnyPayloadV2Error::InvalidPayloadPointer);
        }
        let descriptor = register_descriptor(identity, glue)?;
        let payload = allocate(descriptor.layout)?;
        // SAFETY: the caller provides a readable value with this exact
        // descriptor layout; the compiler-native callback is pinned above.
        let status = unsafe { (descriptor.glue.clone)(payload.as_ptr(), source) };
        if status != 0 {
            // The callback contract guarantees there is nothing to drop here.
            unsafe { dealloc(payload.as_ptr(), descriptor.layout) };
            return Err(ManagedAnyPayloadV2Error::CloneFailed(status));
        }
        Ok(Self {
            descriptor,
            payload,
        })
    }

    pub(super) fn identity(&self) -> ManagedAnyPayloadV2Identity {
        self.descriptor.identity
    }

    /// Clone the actual runtime-selected descriptor without accepting a new
    /// callback from the caller.
    pub(super) fn try_clone(&self) -> Result<Self, ManagedAnyPayloadV2Error> {
        if self.descriptor.version != MANAGED_ANY_PAYLOAD_V2_VERSION {
            return Err(ManagedAnyPayloadV2Error::VersionMismatch);
        }
        let payload = allocate(self.descriptor.layout)?;
        // SAFETY: descriptor and layout are pinned by the source owner.
        let status =
            unsafe { (self.descriptor.glue.clone)(payload.as_ptr(), self.payload.as_ptr()) };
        if status != 0 {
            unsafe { dealloc(payload.as_ptr(), self.descriptor.layout) };
            return Err(ManagedAnyPayloadV2Error::CloneFailed(status));
        }
        Ok(Self {
            descriptor: Arc::clone(&self.descriptor),
            payload,
        })
    }

    /// Exact-check all descriptor components, then recursively clone into an
    /// uninitialized output slot. The source remains live and unchanged.
    pub(super) unsafe fn unpack_clone_to(
        &self,
        expected: ManagedAnyPayloadV2Identity,
        output: *mut u8,
        output_size: usize,
        output_align: usize,
    ) -> Result<(), ManagedAnyPayloadV2Error> {
        if self.descriptor.version != MANAGED_ANY_PAYLOAD_V2_VERSION {
            return Err(ManagedAnyPayloadV2Error::VersionMismatch);
        }
        if self.descriptor.identity != expected {
            return Err(ManagedAnyPayloadV2Error::TypeMismatch);
        }
        if output_size != expected.size || output_align != expected.align {
            return Err(ManagedAnyPayloadV2Error::LayoutMismatch);
        }
        if output.is_null() || !(output as usize).is_multiple_of(output_align) {
            return Err(ManagedAnyPayloadV2Error::InvalidOutputPointer);
        }
        // SAFETY: exact identity/layout was established above. The callback
        // obeys the all-or-nothing initialization contract.
        let status = unsafe { (self.descriptor.glue.clone)(output, self.payload.as_ptr()) };
        if status == 0 {
            Ok(())
        } else {
            Err(ManagedAnyPayloadV2Error::CloneFailed(status))
        }
    }

    pub(super) fn payload_ptr(&self) -> *const u8 {
        self.payload.as_ptr()
    }
}

impl Drop for ManagedAnyPayloadV2 {
    fn drop(&mut self) {
        // SAFETY: every payload was fully initialized by a successful Clone
        // callback and is dropped after owner/domain/root locks have ended.
        unsafe { (self.descriptor.glue.drop)(self.payload.as_ptr()) };
        unsafe { dealloc(self.payload.as_ptr(), self.descriptor.layout) };
    }
}

fn checked_layout(size: usize, align: usize) -> Result<Layout, ManagedAnyPayloadV2Error> {
    if size == 0 {
        return Err(ManagedAnyPayloadV2Error::InvalidLayout);
    }
    Layout::from_size_align(size, align).map_err(|_| ManagedAnyPayloadV2Error::InvalidLayout)
}

fn allocate(layout: Layout) -> Result<NonNull<u8>, ManagedAnyPayloadV2Error> {
    // SAFETY: the descriptor's layout was checked and has nonzero size.
    NonNull::new(unsafe { alloc(layout) }).ok_or(ManagedAnyPayloadV2Error::Capacity)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManagedAnyPayloadV2Error {
    InvalidTag,
    InvalidTypeId,
    InvalidLayout,
    InvalidPayloadPointer,
    InvalidOutputPointer,
    LayoutMismatch,
    VersionMismatch,
    TypeMismatch,
    UnsupportedAggregate,
    CallbackConflict,
    Capacity,
    Poisoned,
    CloneFailed(i32),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FAILED_CLONE_DROP_CALLS: AtomicUsize = AtomicUsize::new(0);
    static AGGREGATE_CHILD_ROLLBACKS: AtomicUsize = AtomicUsize::new(0);
    static AGGREGATE_CHILD_DROP_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn clone_bytes(dst: *mut u8, src: *const u8) -> i32 {
        unsafe { *dst = *src };
        0
    }

    unsafe extern "C" fn drop_bytes(_payload: *mut u8) {}

    unsafe extern "C" fn drop_bytes_alt(_payload: *mut u8) {}

    unsafe extern "C" fn clone_fail(_dst: *mut u8, _src: *const u8) -> i32 {
        19
    }

    unsafe extern "C" fn drop_aggregate_child(_payload: *mut u8) {
        AGGREGATE_CHILD_DROP_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    /// Models compiler-emitted aggregate glue that has cloned one child before
    /// a later child fails. It calls that child's Drop glue itself, then
    /// returns with no owned child and an uninitialized outer destination.
    unsafe extern "C" fn clone_aggregate_fail_after_child(dst: *mut u8, src: *const u8) -> i32 {
        unsafe { *dst = *src };
        unsafe { drop_aggregate_child(dst) };
        AGGREGATE_CHILD_ROLLBACKS.fetch_add(1, Ordering::SeqCst);
        23
    }

    unsafe extern "C" fn counted_drop(_payload: *mut u8) {
        FAILED_CLONE_DROP_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    fn identity(tag: ManagedAnyPayloadV2Tag) -> ManagedAnyPayloadV2Identity {
        ManagedAnyPayloadV2Identity {
            tag,
            type_id: ManagedAnyPayloadV2TypeId::parse(b"rt:0123456789abcdef0123456789abcdef")
                .unwrap(),
            descriptor_hash: [7; 32],
            size: 1,
            align: 1,
        }
    }

    #[test]
    fn tag_and_canonical_type_id_contract_is_explicit() {
        assert_eq!(
            ManagedAnyPayloadV2Tag::try_from(3),
            Ok(ManagedAnyPayloadV2Tag::Bool)
        );
        assert_eq!(
            ManagedAnyPayloadV2Tag::try_from(5),
            Ok(ManagedAnyPayloadV2Tag::Aggregate)
        );
        assert_eq!(
            ManagedAnyPayloadV2Tag::try_from(6),
            Err(ManagedAnyPayloadV2Error::InvalidTag)
        );
        assert!(ManagedAnyPayloadV2TypeId::parse(b"rt:0123456789abcdef0123456789abcdef").is_ok());
        assert!(ManagedAnyPayloadV2TypeId::parse(b"rt:0123456789ABCDEF0123456789ABCDEF").is_err());
    }

    #[test]
    fn canonical_i32_runtime_receipt_matches_an_independent_core_typedesc() {
        use crate::core::ir::{PrimitiveType, ResolvedType, ResolvedTypeTable};
        use crate::core::mir::types::MirTypeCatalog;

        fn update_framed(hasher: &mut Blake3Hasher, bytes: &[u8]) {
            hasher.update(&(bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        }

        let core_i32_id = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        assert_eq!(core_i32_id.as_str().as_bytes(), &CANONICAL_I32_TYPE_ID[..]);

        let mut resolved = ResolvedTypeTable::new();
        let interned_i32 = resolved
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("Core i32 TypeDesc");
        assert_eq!(interned_i32, core_i32_id);
        let catalog = MirTypeCatalog::from_resolved_types(&resolved).expect("Core i32 catalog");
        let descriptor = catalog.get(&core_i32_id).expect("canonical i32 descriptor");
        assert_eq!(
            descriptor.canonical_text(),
            format!(
                "{} kind=primitive(i32) layout=scalar native_record_abi=none session_protocol=- ownership=copy abi=i32 glue=move_out=noop clone=noop drop=noop drop_plan=- variant_drop_plan=- drop=false clone=false\n",
                core_i32_id.as_str()
            )
        );

        // Independently build the existing one-node V2 TypeDesc-closure hash
        // stream from the Core descriptor. Primitive i32 has no checker,
        // layout, protocol, or glue-child edges.
        let mut hasher = Blake3Hasher::new();
        hasher.update(b"mimi-managed-any-payload-v2:typedesc-closure:v1\n");
        update_framed(&mut hasher, core_i32_id.as_str().as_bytes());
        update_framed(&mut hasher, descriptor.canonical_text().as_bytes());
        for section in [
            b"checker".as_slice(),
            b"layout".as_slice(),
            b"session-protocol".as_slice(),
            b"drop-plan".as_slice(),
            b"variant-drop-plans".as_slice(),
        ] {
            update_framed(&mut hasher, section);
            hasher.update(&0_u64.to_be_bytes());
        }
        assert_eq!(hasher.finalize(), CANONICAL_I32_TYPE_DESC_HASH);
    }

    #[test]
    fn selection_frame_checks_version_order_hash_and_owner_membership() {
        let expected = identity(ManagedAnyPayloadV2Tag::Bool);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&SELECTION_SCHEMA_VERSION.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&(TYPE_ID_BYTES as u64).to_be_bytes());
        bytes.extend_from_slice(expected.type_id.as_bytes());
        bytes.extend_from_slice(&(expected.tag as i32).to_be_bytes());
        bytes.extend_from_slice(&expected.descriptor_hash);
        let mut hasher = Blake3Hasher::new();
        hasher.update(REGISTRY_HASH_DOMAIN);
        hasher.update(&bytes[8..]);
        let registry_hash = hasher.finalize();

        let selection = ManagedAnyPayloadV2Selection::parse(&bytes, &registry_hash, false).unwrap();
        assert!(selection.contains(expected));
        let mut wrong_hash = registry_hash;
        wrong_hash[0] ^= 1;
        assert_eq!(
            ManagedAnyPayloadV2Selection::parse(&bytes, &wrong_hash, false).unwrap_err(),
            ManagedAnyPayloadV2SelectionError::RegistryHashMismatch
        );
        assert!(ManagedAnyPayloadV2Selection::parse(&bytes, &registry_hash, true).is_ok());
        let empty = [0u8; 8];
        assert!(ManagedAnyPayloadV2Selection::parse(&empty, &registry_hash, false).is_err());
    }

    #[test]
    fn aggregate_tag_uses_the_same_pinned_recursive_glue_contract() {
        let descriptor_identity = identity(ManagedAnyPayloadV2Tag::Aggregate);
        let source = 0xa5u8;
        let owner = unsafe {
            ManagedAnyPayloadV2::clone_from_borrowed(
                descriptor_identity,
                ManagedAnyPayloadV2Glue {
                    clone: clone_bytes,
                    drop: drop_bytes,
                },
                &source,
            )
        }
        .unwrap();
        let clone = owner.try_clone().unwrap();
        assert_eq!(owner.identity(), descriptor_identity);
        assert_eq!(unsafe { *clone.payload_ptr() }, source);
        drop(clone);
        drop(owner);
    }

    #[test]
    fn failed_aggregate_glue_rolls_back_children_before_returning() {
        AGGREGATE_CHILD_ROLLBACKS.store(0, Ordering::SeqCst);
        AGGREGATE_CHILD_DROP_CALLS.store(0, Ordering::SeqCst);
        FAILED_CLONE_DROP_CALLS.store(0, Ordering::SeqCst);
        let mut descriptor_identity = identity(ManagedAnyPayloadV2Tag::Aggregate);
        descriptor_identity.descriptor_hash = [93; 32];
        let source = 0u8;
        let result = unsafe {
            ManagedAnyPayloadV2::clone_from_borrowed(
                descriptor_identity,
                ManagedAnyPayloadV2Glue {
                    clone: clone_aggregate_fail_after_child,
                    drop: counted_drop,
                },
                &source,
            )
        };
        assert!(matches!(
            result,
            Err(ManagedAnyPayloadV2Error::CloneFailed(23))
        ));
        assert_eq!(AGGREGATE_CHILD_ROLLBACKS.load(Ordering::SeqCst), 1);
        assert_eq!(AGGREGATE_CHILD_DROP_CALLS.load(Ordering::SeqCst), 1);
        // Runtime deallocates the uninitialized top-level allocation without
        // calling the aggregate Drop callback after failed Clone glue.
        assert_eq!(FAILED_CLONE_DROP_CALLS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn same_descriptor_key_cannot_change_pinned_drop_glue() {
        let descriptor_identity = identity(ManagedAnyPayloadV2Tag::Bool);
        let first = register_descriptor(
            descriptor_identity,
            ManagedAnyPayloadV2Glue {
                clone: clone_bytes,
                drop: drop_bytes,
            },
        )
        .unwrap();
        let second = register_descriptor(
            descriptor_identity,
            ManagedAnyPayloadV2Glue {
                clone: clone_bytes,
                drop: drop_bytes,
            },
        )
        .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(
            register_descriptor(
                descriptor_identity,
                ManagedAnyPayloadV2Glue {
                    clone: clone_bytes,
                    drop: drop_bytes_alt,
                }
            ),
            Err(ManagedAnyPayloadV2Error::CallbackConflict)
        ));
    }

    #[test]
    fn bool_false_remains_a_tag_three_descriptor() {
        let descriptor_identity = identity(ManagedAnyPayloadV2Tag::Bool);
        let source = 0u8;
        let owner = unsafe {
            ManagedAnyPayloadV2::clone_from_borrowed(
                descriptor_identity,
                ManagedAnyPayloadV2Glue {
                    clone: clone_bytes,
                    drop: drop_bytes,
                },
                &source,
            )
        }
        .unwrap();
        assert_eq!(owner.identity().tag, ManagedAnyPayloadV2Tag::Bool);
        assert_eq!(unsafe { *owner.payload_ptr() }, 0);
        assert_ne!(owner.identity().tag, ManagedAnyPayloadV2Tag::I32);
    }

    #[test]
    fn failed_recursive_clone_does_not_run_drop_on_uninitialized_storage() {
        FAILED_CLONE_DROP_CALLS.store(0, Ordering::SeqCst);
        let mut descriptor_identity = identity(ManagedAnyPayloadV2Tag::Bool);
        descriptor_identity.descriptor_hash = [91; 32];
        let source = 0u8;
        let result = unsafe {
            ManagedAnyPayloadV2::clone_from_borrowed(
                descriptor_identity,
                ManagedAnyPayloadV2Glue {
                    clone: clone_fail,
                    drop: counted_drop,
                },
                &source,
            )
        };
        assert!(matches!(
            result,
            Err(ManagedAnyPayloadV2Error::CloneFailed(19))
        ));
        assert_eq!(FAILED_CLONE_DROP_CALLS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn pinned_clone_glue_supports_concurrent_reads_and_foreign_thread_drop() {
        let mut descriptor_identity = identity(ManagedAnyPayloadV2Tag::Bool);
        descriptor_identity.descriptor_hash = [92; 32];
        let source = 0u8;
        let owner = Arc::new(
            unsafe {
                ManagedAnyPayloadV2::clone_from_borrowed(
                    descriptor_identity,
                    ManagedAnyPayloadV2Glue {
                        clone: clone_bytes,
                        drop: drop_bytes,
                    },
                    &source,
                )
            }
            .unwrap(),
        );
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..4 {
                let owner = Arc::clone(&owner);
                workers.push(scope.spawn(move || {
                    let cloned = owner.try_clone().unwrap();
                    assert_eq!(unsafe { *cloned.payload_ptr() }, 0);
                    drop(cloned);
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        assert_eq!(owner.identity(), descriptor_identity);
    }
}
