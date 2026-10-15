//! Canonical owner registry for the closed `Any` payload profile.
//!
//! The table is one singleton per loaded Mimi runtime image. Its token is a
//! dedicated Rust type; it is never a `ValueHandle`. The low word contains an
//! Any class tag and a one-based slot index, while the high word contains the
//! complete `u32` slot generation. Descriptor identity stays in the live slot
//! sidecar and is pinned there by `Arc`, so replacing a current descriptor
//! version cannot change the meaning or destructor of an existing owner.
//!
//! Version 1 remains closed to `I32` and owned `String`. Version 2 payloads
//! enter this same generation-bearing token table as a distinct owner kind;
//! their actual `(tag, type-id, hash, size, align)` and glue are pinned by the
//! V2 payload descriptor registry. No second token table is permitted.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

const ANY_CLASS_TAG: u32 = 1 << 31;
const SLOT_INDEX_MASK: u32 = ANY_CLASS_TAG - 1;
const MAX_SLOT_INDEX: u32 = SLOT_INDEX_MASK;
const FIRST_GENERATION: u32 = 1;
const DESCRIPTOR_V1: u32 = 1;

/// Opaque owner token for a single typed `Any` value.
///
/// `wire_bits` exists for future checked ABI bridges. There is deliberately no
/// conversion to or from the runtime's legacy `ValueHandle` type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct AnyOwnerToken(u64);

impl AnyOwnerToken {
    /// Parse the packed wire form, rejecting malformed class, slot, and
    /// generation fields before the token reaches a registry operation.
    pub(crate) fn from_wire_bits(bits: u64) -> Result<Self, AnyOwnerError> {
        decode_token(bits)?;
        Ok(Self(bits))
    }

    /// Return the canonical packed representation without changing its type.
    pub(crate) const fn wire_bits(self) -> u64 {
        self.0
    }

    fn from_parts(slot_index: u32, generation: u32) -> Result<Self, AnyOwnerError> {
        if slot_index == 0 || slot_index > MAX_SLOT_INDEX || generation == 0 {
            return Err(AnyOwnerError::InvalidToken);
        }
        let low_word = ANY_CLASS_TAG | slot_index;
        Ok(Self(((generation as u64) << 32) | u64::from(low_word)))
    }

    fn fields(self) -> Result<TokenFields, AnyOwnerError> {
        decode_token(self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TokenFields {
    slot_index: u32,
    generation: u32,
}

fn decode_token(bits: u64) -> Result<TokenFields, AnyOwnerError> {
    let low_word = bits as u32;
    let generation = (bits >> 32) as u32;
    if low_word & ANY_CLASS_TAG == 0 || generation == 0 {
        return Err(AnyOwnerError::InvalidToken);
    }
    let slot_index = low_word & SLOT_INDEX_MASK;
    if slot_index == 0 {
        return Err(AnyOwnerError::InvalidToken);
    }
    Ok(TokenFields {
        slot_index,
        generation,
    })
}

/// The deliberately small set of payload shapes currently admitted to `Any`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnyOwnerKind {
    I32,
    OwnedString,
    /// V2 has a dynamic payload tag; the exact descriptor lives on its payload.
    ManagedV2,
}

impl AnyOwnerKind {
    const fn descriptor_index(self) -> Option<usize> {
        match self {
            Self::I32 => Some(0),
            Self::OwnedString => Some(1),
            Self::ManagedV2 => None,
        }
    }
}

/// Fail-closed errors from the canonical owner table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnyOwnerError {
    InvalidToken,
    StaleToken,
    DescriptorMismatch,
    Capacity,
    Poisoned,
    UnsupportedAggregate,
    CallbackConflict,
    CloneFailed(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DescriptorIdentity {
    pub(crate) kind: AnyOwnerKind,
    pub(crate) version: u32,
}

enum AnyOwnerPayload {
    I32(i32),
    OwnedString(String),
    ManagedV2(super::managed_any_payload_v2::ManagedAnyPayloadV2),
}

impl AnyOwnerPayload {
    const fn kind(&self) -> AnyOwnerKind {
        match self {
            Self::I32(_) => AnyOwnerKind::I32,
            Self::OwnedString(_) => AnyOwnerKind::OwnedString,
            Self::ManagedV2(_) => AnyOwnerKind::ManagedV2,
        }
    }
}

type CloneGlue =
    dyn Fn(&AnyOwnerPayload) -> Result<AnyOwnerPayload, AnyOwnerError> + Send + Sync + 'static;
type DropGlue = dyn Fn(AnyOwnerPayload) + Send + Sync + 'static;

/// Immutable descriptor version. A live owner pins this exact object, even
/// after the registry begins allocating with a later version.
struct DescriptorVersion {
    identity: DescriptorIdentity,
    clone_glue: Arc<CloneGlue>,
    drop_glue: Arc<DropGlue>,
}

impl DescriptorVersion {
    fn new(
        kind: AnyOwnerKind,
        version: u32,
        clone_glue: Arc<CloneGlue>,
        drop_glue: Arc<DropGlue>,
    ) -> Self {
        Self {
            identity: DescriptorIdentity { kind, version },
            clone_glue,
            drop_glue,
        }
    }
}

/// The registry slot owns one Arc to this immutable value. Temporary snapshots
/// used by `clone_owner` and `copy_string` keep it alive while the table lock is
/// released. The last Arc runs the descriptor's drop glue outside that lock.
struct StoredOwner {
    descriptor: Arc<DescriptorVersion>,
    payload: Option<AnyOwnerPayload>,
}

impl StoredOwner {
    fn payload(&self) -> &AnyOwnerPayload {
        // The payload is taken only by this value's Drop implementation.
        self.payload.as_ref().expect("live Any owner has a payload")
    }
}

impl Drop for StoredOwner {
    fn drop(&mut self) {
        if let Some(payload) = self.payload.take() {
            (self.descriptor.drop_glue)(payload);
        }
    }
}

struct Slot {
    generation: u32,
    retired: bool,
    owner: Option<Arc<StoredOwner>>,
}

struct Registry {
    current_descriptors: [Arc<DescriptorVersion>; 2],
    slots: Vec<Slot>,
    max_slots: u32,
}

impl Registry {
    fn new() -> Self {
        Self::with_slot_limit(MAX_SLOT_INDEX).expect("the production slot limit is valid")
    }

    fn with_slot_limit(max_slots: u32) -> Result<Self, AnyOwnerError> {
        if max_slots > MAX_SLOT_INDEX {
            return Err(AnyOwnerError::Capacity);
        }
        Ok(Self {
            current_descriptors: [
                Arc::new(DescriptorVersion::new(
                    AnyOwnerKind::I32,
                    DESCRIPTOR_V1,
                    Arc::new(clone_i32),
                    Arc::new(drop_payload),
                )),
                Arc::new(DescriptorVersion::new(
                    AnyOwnerKind::OwnedString,
                    DESCRIPTOR_V1,
                    Arc::new(clone_owned_string),
                    Arc::new(drop_payload),
                )),
            ],
            slots: Vec::new(),
            max_slots,
        })
    }

    fn current_descriptor(&self, kind: AnyOwnerKind) -> &Arc<DescriptorVersion> {
        &self.current_descriptors[kind.descriptor_index().expect("V2 has no v1 descriptor")]
    }

    fn allocate(&mut self, owner: Arc<StoredOwner>) -> Result<AnyOwnerToken, AnyOwnerError> {
        let descriptor = &owner.descriptor;
        let payload = owner.payload();
        if descriptor.identity.version == 0 || descriptor.identity.kind != payload.kind() {
            return Err(AnyOwnerError::DescriptorMismatch);
        }

        let available = self
            .slots
            .iter()
            .position(|slot| !slot.retired && slot.owner.is_none());
        let (slot_position, is_new_slot) = match available {
            Some(position) => (position, false),
            None => {
                if self.slots.len() >= self.max_slots as usize {
                    return Err(AnyOwnerError::Capacity);
                }
                self.slots
                    .try_reserve(1)
                    .map_err(|_| AnyOwnerError::Capacity)?;
                let position = self.slots.len();
                (position, true)
            }
        };

        let slot_index = u32::try_from(slot_position + 1).map_err(|_| AnyOwnerError::Capacity)?;
        let generation = if is_new_slot {
            FIRST_GENERATION
        } else {
            self.slots[slot_position].generation
        };
        let token = AnyOwnerToken::from_parts(slot_index, generation)?;

        if is_new_slot {
            self.slots.push(Slot {
                generation,
                retired: false,
                owner: Some(owner),
            });
        } else {
            let slot = &mut self.slots[slot_position];
            slot.owner = Some(owner);
        }
        Ok(token)
    }

    fn live_owner(&self, token: AnyOwnerToken) -> Result<&Arc<StoredOwner>, AnyOwnerError> {
        let fields = token.fields()?;
        let Some(slot) = self.slots.get((fields.slot_index - 1) as usize) else {
            return Err(AnyOwnerError::InvalidToken);
        };
        if slot.generation != fields.generation {
            return Err(AnyOwnerError::StaleToken);
        }
        if slot.retired {
            return Err(AnyOwnerError::StaleToken);
        }
        let Some(owner) = slot.owner.as_ref() else {
            return Err(AnyOwnerError::StaleToken);
        };
        if owner.descriptor.identity.version == 0
            || owner.descriptor.identity.kind != owner.payload().kind()
        {
            return Err(AnyOwnerError::DescriptorMismatch);
        }
        Ok(owner)
    }

    fn detach(&mut self, token: AnyOwnerToken) -> Result<Arc<StoredOwner>, AnyOwnerError> {
        let fields = token.fields()?;
        let Some(slot) = self.slots.get_mut((fields.slot_index - 1) as usize) else {
            return Err(AnyOwnerError::InvalidToken);
        };
        if slot.generation != fields.generation || slot.retired || slot.owner.is_none() {
            return Err(AnyOwnerError::StaleToken);
        }

        let owner = slot.owner.take().expect("the slot was checked as live");
        if slot.generation == u32::MAX {
            // Never wrap to a generation that an old token could reuse.
            slot.retired = true;
        } else {
            slot.generation += 1;
        }
        Ok(owner)
    }

    fn install_descriptor_version(
        &mut self,
        descriptor: Arc<DescriptorVersion>,
    ) -> Result<Arc<DescriptorVersion>, AnyOwnerError> {
        let Some(index) = descriptor.identity.kind.descriptor_index() else {
            return Err(AnyOwnerError::DescriptorMismatch);
        };
        if descriptor.identity.version == 0
            || descriptor.identity.version <= self.current_descriptors[index].identity.version
        {
            return Err(AnyOwnerError::DescriptorMismatch);
        }
        Ok(std::mem::replace(
            &mut self.current_descriptors[index],
            descriptor,
        ))
    }
}

fn clone_i32(payload: &AnyOwnerPayload) -> Result<AnyOwnerPayload, AnyOwnerError> {
    match payload {
        AnyOwnerPayload::I32(value) => Ok(AnyOwnerPayload::I32(*value)),
        AnyOwnerPayload::OwnedString(_) | AnyOwnerPayload::ManagedV2(_) => {
            Err(AnyOwnerError::DescriptorMismatch)
        }
    }
}

fn clone_owned_string(payload: &AnyOwnerPayload) -> Result<AnyOwnerPayload, AnyOwnerError> {
    match payload {
        AnyOwnerPayload::OwnedString(value) => {
            Ok(AnyOwnerPayload::OwnedString(copy_string_fallibly(value)?))
        }
        AnyOwnerPayload::I32(_) | AnyOwnerPayload::ManagedV2(_) => {
            Err(AnyOwnerError::DescriptorMismatch)
        }
    }
}

fn clone_managed_v2(payload: &AnyOwnerPayload) -> Result<AnyOwnerPayload, AnyOwnerError> {
    match payload {
        AnyOwnerPayload::ManagedV2(value) => value
            .try_clone()
            .map(AnyOwnerPayload::ManagedV2)
            .map_err(map_managed_v2_error),
        AnyOwnerPayload::I32(_) | AnyOwnerPayload::OwnedString(_) => {
            Err(AnyOwnerError::DescriptorMismatch)
        }
    }
}

fn map_managed_v2_error(
    error: super::managed_any_payload_v2::ManagedAnyPayloadV2Error,
) -> AnyOwnerError {
    use super::managed_any_payload_v2::ManagedAnyPayloadV2Error as V2;
    match error {
        V2::Capacity => AnyOwnerError::Capacity,
        V2::Poisoned => AnyOwnerError::Poisoned,
        V2::UnsupportedAggregate => AnyOwnerError::UnsupportedAggregate,
        V2::CallbackConflict => AnyOwnerError::CallbackConflict,
        V2::CloneFailed(status) => AnyOwnerError::CloneFailed(status),
        V2::InvalidTag
        | V2::InvalidTypeId
        | V2::InvalidLayout
        | V2::InvalidPayloadPointer
        | V2::InvalidOutputPointer
        | V2::LayoutMismatch
        | V2::VersionMismatch
        | V2::TypeMismatch => AnyOwnerError::DescriptorMismatch,
    }
}

fn managed_v2_descriptor() -> Arc<DescriptorVersion> {
    static DESCRIPTOR: OnceLock<Arc<DescriptorVersion>> = OnceLock::new();
    Arc::clone(DESCRIPTOR.get_or_init(|| {
        Arc::new(DescriptorVersion::new(
            AnyOwnerKind::ManagedV2,
            super::managed_any_payload_v2::MANAGED_ANY_PAYLOAD_V2_VERSION,
            Arc::new(clone_managed_v2),
            Arc::new(drop_payload),
        ))
    }))
}

fn drop_payload(payload: AnyOwnerPayload) {
    drop(payload);
}

fn copy_string_fallibly(value: &str) -> Result<String, AnyOwnerError> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|_| AnyOwnerError::Capacity)?;
    copy.push_str(value);
    Ok(copy)
}

static OWNER_REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

fn owner_registry() -> &'static Mutex<Registry> {
    OWNER_REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
}

fn lock_registry(table: &Mutex<Registry>) -> Result<MutexGuard<'_, Registry>, AnyOwnerError> {
    table.lock().map_err(|_| AnyOwnerError::Poisoned)
}

fn allocate_payload_on(
    table: &Mutex<Registry>,
    kind: AnyOwnerKind,
    payload: AnyOwnerPayload,
) -> Result<AnyOwnerToken, AnyOwnerError> {
    let mut registry = lock_registry(table)?;
    let descriptor = Arc::clone(registry.current_descriptor(kind));
    let owner = Arc::new(StoredOwner {
        descriptor,
        payload: Some(payload),
    });
    let result = registry.allocate(Arc::clone(&owner));
    drop(registry);
    drop(owner);
    result
}

/// Allocate a fresh owner for one `i32` value.
pub(crate) fn allocate_i32(value: i32) -> Result<AnyOwnerToken, AnyOwnerError> {
    allocate_i32_on(owner_registry(), value)
}

fn allocate_i32_on(table: &Mutex<Registry>, value: i32) -> Result<AnyOwnerToken, AnyOwnerError> {
    allocate_payload_on(table, AnyOwnerKind::I32, AnyOwnerPayload::I32(value))
}

/// Copy a UTF-8 string into a fresh owned `Any` slot.
pub(crate) fn allocate_string(value: &str) -> Result<AnyOwnerToken, AnyOwnerError> {
    allocate_string_on(owner_registry(), value)
}

fn allocate_string_on(
    table: &Mutex<Registry>,
    value: &str,
) -> Result<AnyOwnerToken, AnyOwnerError> {
    let owned = copy_string_fallibly(value)?;
    allocate_payload_on(
        table,
        AnyOwnerKind::OwnedString,
        AnyOwnerPayload::OwnedString(owned),
    )
}

/// Install a newly private V2 payload in the one generation-bearing owner
/// table. The payload's actual descriptor and callbacks remain pinned by its
/// own Arc receipt; this wrapper descriptor marks only the schema version.
pub(crate) fn allocate_managed_v2(
    payload: super::managed_any_payload_v2::ManagedAnyPayloadV2,
) -> Result<AnyOwnerToken, AnyOwnerError> {
    allocate_managed_v2_on(owner_registry(), payload)
}

fn allocate_managed_v2_on(
    table: &Mutex<Registry>,
    payload: super::managed_any_payload_v2::ManagedAnyPayloadV2,
) -> Result<AnyOwnerToken, AnyOwnerError> {
    let owner = Arc::new(StoredOwner {
        descriptor: managed_v2_descriptor(),
        payload: Some(AnyOwnerPayload::ManagedV2(payload)),
    });
    let result = {
        let mut registry = lock_registry(table)?;
        registry.allocate(Arc::clone(&owner))
    };
    drop(owner);
    result
}

fn allocate_with_descriptor_on(
    table: &Mutex<Registry>,
    descriptor: Arc<DescriptorVersion>,
    payload: AnyOwnerPayload,
) -> Result<AnyOwnerToken, AnyOwnerError> {
    let owner = Arc::new(StoredOwner {
        descriptor,
        payload: Some(payload),
    });
    let result = {
        let mut registry = lock_registry(table)?;
        registry.allocate(Arc::clone(&owner))
    };
    drop(owner);
    result
}

fn snapshot_owner_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
    expected_kind: Option<AnyOwnerKind>,
) -> Result<Arc<StoredOwner>, AnyOwnerError> {
    let registry = lock_registry(table)?;
    let owner = registry.live_owner(token)?;
    if expected_kind.is_some_and(|kind| owner.descriptor.identity.kind != kind) {
        return Err(AnyOwnerError::DescriptorMismatch);
    }
    Ok(Arc::clone(owner))
}

fn snapshot_owner_v2_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
) -> Result<Arc<StoredOwner>, AnyOwnerError> {
    let registry = lock_registry(table)?;
    let owner = registry.live_owner(token)?;
    if owner.descriptor.identity
        != (DescriptorIdentity {
            kind: AnyOwnerKind::ManagedV2,
            version: super::managed_any_payload_v2::MANAGED_ANY_PAYLOAD_V2_VERSION,
        })
    {
        return Err(AnyOwnerError::DescriptorMismatch);
    }
    if !matches!(owner.payload(), AnyOwnerPayload::ManagedV2(_)) {
        return Err(AnyOwnerError::DescriptorMismatch);
    }
    Ok(Arc::clone(owner))
}

/// The runtime-selected exact V2 descriptor on this live token. The returned
/// key is copied while the owner is pinned; no raw payload pointer escapes.
pub(crate) fn managed_v2_identity(
    token: AnyOwnerToken,
) -> Result<super::managed_any_payload_v2::ManagedAnyPayloadV2Identity, AnyOwnerError> {
    let owner = snapshot_owner_v2_on(owner_registry(), token)?;
    match owner.payload() {
        AnyOwnerPayload::ManagedV2(payload) => Ok(payload.identity()),
        AnyOwnerPayload::I32(_) | AnyOwnerPayload::OwnedString(_) => {
            Err(AnyOwnerError::DescriptorMismatch)
        }
    }
}

/// Clone a V2 owner according to its pinned actual descriptor. No requested
/// static type or caller-supplied callback participates in this operation.
pub(crate) fn clone_managed_v2_owner(token: AnyOwnerToken) -> Result<AnyOwnerToken, AnyOwnerError> {
    let source = snapshot_owner_v2_on(owner_registry(), token)?;
    let payload = match source.payload() {
        AnyOwnerPayload::ManagedV2(payload) => payload.try_clone().map_err(map_managed_v2_error)?,
        AnyOwnerPayload::I32(_) | AnyOwnerPayload::OwnedString(_) => {
            return Err(AnyOwnerError::DescriptorMismatch)
        }
    };
    allocate_managed_v2(payload)
}

/// Unpack only the exact requested descriptor, recursively cloning to caller
/// storage. The Arc snapshot protects the payload if another thread releases
/// the source token while the callback is running.
pub(crate) unsafe fn unpack_managed_v2_owner(
    token: AnyOwnerToken,
    expected: super::managed_any_payload_v2::ManagedAnyPayloadV2Identity,
    output: *mut u8,
    output_size: usize,
    output_align: usize,
) -> Result<(), AnyOwnerError> {
    let owner = snapshot_owner_v2_on(owner_registry(), token)?;
    match owner.payload() {
        AnyOwnerPayload::ManagedV2(payload) => unsafe {
            payload
                .unpack_clone_to(expected, output, output_size, output_align)
                .map_err(map_managed_v2_error)
        },
        AnyOwnerPayload::I32(_) | AnyOwnerPayload::OwnedString(_) => {
            Err(AnyOwnerError::DescriptorMismatch)
        }
    }
}

/// Release a V2 token while validating schema version and payload variant
/// under the same registry lock that detaches its generational slot.
pub(crate) fn release_managed_v2_owner(
    token: AnyOwnerToken,
    expected: super::managed_any_payload_v2::ManagedAnyPayloadV2Identity,
) -> Result<(), AnyOwnerError> {
    let owner = {
        let mut registry = lock_registry(owner_registry())?;
        let owner = registry.live_owner(token)?;
        if owner.descriptor.identity
            != (DescriptorIdentity {
                kind: AnyOwnerKind::ManagedV2,
                version: super::managed_any_payload_v2::MANAGED_ANY_PAYLOAD_V2_VERSION,
            })
            || !matches!(owner.payload(), AnyOwnerPayload::ManagedV2(payload) if payload.identity() == expected)
        {
            return Err(AnyOwnerError::DescriptorMismatch);
        }
        registry.detach(token)?
    };
    // The pinned callback runs after the owner-table mutex is released.
    drop(owner);
    Ok(())
}

/// Validate token shape, class, generation, liveness, and descriptor sidecar.
pub(crate) fn validate(token: AnyOwnerToken) -> Result<AnyOwnerKind, AnyOwnerError> {
    validate_on(owner_registry(), token)
}

fn validate_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
) -> Result<AnyOwnerKind, AnyOwnerError> {
    let registry = lock_registry(table)?;
    Ok(registry.live_owner(token)?.descriptor.identity.kind)
}

/// Read the exact descriptor identity pinned by a live typed owner token.
/// Validation includes token class, generation and payload/descriptor shape.
pub(crate) fn descriptor_identity(
    token: AnyOwnerToken,
) -> Result<DescriptorIdentity, AnyOwnerError> {
    descriptor_identity_on(owner_registry(), token)
}

/// Read an `i32`, rejecting owners with any other descriptor shape.
pub(crate) fn read_i32(token: AnyOwnerToken) -> Result<i32, AnyOwnerError> {
    read_i32_on(owner_registry(), token)
}

fn read_i32_on(table: &Mutex<Registry>, token: AnyOwnerToken) -> Result<i32, AnyOwnerError> {
    let owner = snapshot_owner_on(table, token, Some(AnyOwnerKind::I32))?;
    match owner.payload() {
        AnyOwnerPayload::I32(value) => Ok(*value),
        AnyOwnerPayload::OwnedString(_) | AnyOwnerPayload::ManagedV2(_) => {
            Err(AnyOwnerError::DescriptorMismatch)
        }
    }
}

/// Return a fallible owned copy of a stored string.
pub(crate) fn copy_string(token: AnyOwnerToken) -> Result<String, AnyOwnerError> {
    copy_string_on(owner_registry(), token)
}

fn copy_string_on(table: &Mutex<Registry>, token: AnyOwnerToken) -> Result<String, AnyOwnerError> {
    let owner = snapshot_owner_on(table, token, Some(AnyOwnerKind::OwnedString))?;
    let clone = (owner.descriptor.clone_glue)(owner.payload())?;
    match clone {
        AnyOwnerPayload::OwnedString(value) => Ok(value),
        AnyOwnerPayload::I32(_) | AnyOwnerPayload::ManagedV2(_) => {
            Err(AnyOwnerError::DescriptorMismatch)
        }
    }
}

/// Create an independent owner using the exact clone glue pinned by `token`.
pub(crate) fn clone_owner(token: AnyOwnerToken) -> Result<AnyOwnerToken, AnyOwnerError> {
    clone_owner_on(owner_registry(), token)
}

fn clone_owner_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
) -> Result<AnyOwnerToken, AnyOwnerError> {
    let source = snapshot_owner_on(table, token, None)?;
    let descriptor = Arc::clone(&source.descriptor);
    let payload = (descriptor.clone_glue)(source.payload())?;
    allocate_with_descriptor_on(table, descriptor, payload)
}

/// Errors from a closed ManagedAny v1 descriptor receipt. Expected tag and
/// version are checked against one acquired owner, never a prior tag query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnyOwnerReceiptError {
    KindMismatch,
    VersionMismatch,
    Owner(AnyOwnerError),
}

impl From<AnyOwnerError> for AnyOwnerReceiptError {
    fn from(error: AnyOwnerError) -> Self {
        Self::Owner(error)
    }
}

fn check_v1_receipt(
    owner: &StoredOwner,
    registered: DescriptorIdentity,
    expected_kind: Option<AnyOwnerKind>,
    expected_version: u32,
) -> Result<(), AnyOwnerReceiptError> {
    let actual = owner.descriptor.identity;
    if actual != registered {
        return Err(AnyOwnerError::DescriptorMismatch.into());
    }
    if expected_version != DESCRIPTOR_V1 || actual.version != DESCRIPTOR_V1 {
        return Err(AnyOwnerReceiptError::VersionMismatch);
    }
    if expected_kind.is_some_and(|kind| kind != actual.kind) {
        return Err(AnyOwnerReceiptError::KindMismatch);
    }
    Ok(())
}

fn snapshot_owner_v1_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_kind: Option<AnyOwnerKind>,
    expected_version: u32,
) -> Result<Arc<StoredOwner>, AnyOwnerReceiptError> {
    let registry = lock_registry(table)?;
    let owner = registry.live_owner(token)?;
    check_v1_receipt(owner, registered, expected_kind, expected_version)?;
    Ok(Arc::clone(owner))
}

pub(crate) fn read_i32_v1(
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_kind: AnyOwnerKind,
    expected_version: u32,
) -> Result<i32, AnyOwnerReceiptError> {
    read_i32_v1_on(
        owner_registry(),
        token,
        registered,
        expected_kind,
        expected_version,
    )
}

fn read_i32_v1_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_kind: AnyOwnerKind,
    expected_version: u32,
) -> Result<i32, AnyOwnerReceiptError> {
    let owner = snapshot_owner_v1_on(
        table,
        token,
        registered,
        Some(expected_kind),
        expected_version,
    )?;
    match owner.payload() {
        AnyOwnerPayload::I32(value) => Ok(*value),
        AnyOwnerPayload::OwnedString(_) | AnyOwnerPayload::ManagedV2(_) => {
            Err(AnyOwnerReceiptError::KindMismatch)
        }
    }
}

pub(crate) fn copy_string_v1(
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_kind: AnyOwnerKind,
    expected_version: u32,
) -> Result<String, AnyOwnerReceiptError> {
    copy_string_v1_on(
        owner_registry(),
        token,
        registered,
        expected_kind,
        expected_version,
    )
}

pub(crate) fn copy_string_v1_with_checkpoint(
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_kind: AnyOwnerKind,
    expected_version: u32,
    checkpoint: impl FnOnce() -> Result<(), AnyOwnerError>,
) -> Result<String, AnyOwnerReceiptError> {
    copy_string_v1_on_with_checkpoint(
        owner_registry(),
        token,
        registered,
        expected_kind,
        expected_version,
        checkpoint,
    )
}

fn copy_string_v1_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_kind: AnyOwnerKind,
    expected_version: u32,
) -> Result<String, AnyOwnerReceiptError> {
    copy_string_v1_on_with_checkpoint(
        table,
        token,
        registered,
        expected_kind,
        expected_version,
        || Ok(()),
    )
}

fn copy_string_v1_on_with_checkpoint(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_kind: AnyOwnerKind,
    expected_version: u32,
    checkpoint: impl FnOnce() -> Result<(), AnyOwnerError>,
) -> Result<String, AnyOwnerReceiptError> {
    let owner = snapshot_owner_v1_on(
        table,
        token,
        registered,
        Some(expected_kind),
        expected_version,
    )?;
    if !matches!(owner.payload(), AnyOwnerPayload::OwnedString(_)) {
        return Err(AnyOwnerReceiptError::KindMismatch);
    }
    checkpoint()?;
    match (owner.descriptor.clone_glue)(owner.payload())? {
        AnyOwnerPayload::OwnedString(value) => Ok(value),
        AnyOwnerPayload::I32(_) | AnyOwnerPayload::ManagedV2(_) => {
            Err(AnyOwnerError::DescriptorMismatch.into())
        }
    }
}

pub(crate) fn clone_owner_v1(
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_version: u32,
) -> Result<AnyOwnerToken, AnyOwnerReceiptError> {
    clone_owner_v1_on(owner_registry(), token, registered, expected_version)
}

pub(crate) fn clone_owner_v1_with_checkpoint(
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_version: u32,
    checkpoint: impl FnOnce() -> Result<(), AnyOwnerError>,
) -> Result<AnyOwnerToken, AnyOwnerReceiptError> {
    clone_owner_v1_on_with_checkpoint(
        owner_registry(),
        token,
        registered,
        expected_version,
        checkpoint,
    )
}

fn clone_owner_v1_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_version: u32,
) -> Result<AnyOwnerToken, AnyOwnerReceiptError> {
    clone_owner_v1_on_with_checkpoint(table, token, registered, expected_version, || Ok(()))
}

fn clone_owner_v1_on_with_checkpoint(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_version: u32,
    checkpoint: impl FnOnce() -> Result<(), AnyOwnerError>,
) -> Result<AnyOwnerToken, AnyOwnerReceiptError> {
    let owner = snapshot_owner_v1_on(table, token, registered, None, expected_version)?;
    checkpoint()?;
    let descriptor = Arc::clone(&owner.descriptor);
    let payload = (descriptor.clone_glue)(owner.payload())?;
    Ok(allocate_with_descriptor_on(table, descriptor, payload)?)
}

pub(crate) fn release_owner_v1(
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_version: u32,
) -> Result<(), AnyOwnerReceiptError> {
    release_owner_v1_on(owner_registry(), token, registered, expected_version)
}

fn release_owner_v1_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
    registered: DescriptorIdentity,
    expected_version: u32,
) -> Result<(), AnyOwnerReceiptError> {
    let owner = {
        let mut registry = lock_registry(table)?;
        let owner = registry.live_owner(token)?;
        check_v1_receipt(owner, registered, None, expected_version)?;
        registry.detach(token)?
    };
    // Release descriptor glue only after the same check-and-detach lease ends.
    drop(owner);
    Ok(())
}

/// Release an owner. Its descriptor's drop glue runs only after the registry
/// mutex has been released; duplicate and stale releases return an error.
pub(crate) fn release(token: AnyOwnerToken) -> Result<(), AnyOwnerError> {
    release_owner_on(owner_registry(), token)
}

fn release_owner_on(table: &Mutex<Registry>, token: AnyOwnerToken) -> Result<(), AnyOwnerError> {
    let owner = {
        let mut registry = lock_registry(table)?;
        registry.detach(token)?
    };
    drop(owner);
    Ok(())
}

/// Test-only raw message view into an actual Any String allocation. No owner
/// pin escapes this function; the pointer becomes invalid when token releases.
/// The subprocess supplies a payload containing NUL before calling abort.
/// This deliberately is not a production borrow/erasure ABI.
#[cfg(test)]
pub(crate) fn string_payload_pointer_for_test(
    token: AnyOwnerToken,
) -> Result<*const std::ffi::c_char, AnyOwnerError> {
    let owner = snapshot_owner_on(owner_registry(), token, Some(AnyOwnerKind::OwnedString))?;
    match owner.payload() {
        AnyOwnerPayload::OwnedString(value) => Ok(value.as_ptr().cast()),
        AnyOwnerPayload::I32(_) | AnyOwnerPayload::ManagedV2(_) => {
            Err(AnyOwnerError::DescriptorMismatch)
        }
    }
}

#[cfg(test)]
pub(crate) fn live_count() -> Result<usize, AnyOwnerError> {
    live_count_on(owner_registry())
}

#[cfg(test)]
fn live_count_on(table: &Mutex<Registry>) -> Result<usize, AnyOwnerError> {
    let registry = lock_registry(table)?;
    Ok(registry
        .slots
        .iter()
        .filter(|slot| slot.owner.is_some())
        .count())
}

#[cfg(test)]
fn register_descriptor_version_on(
    table: &Mutex<Registry>,
    descriptor: Arc<DescriptorVersion>,
) -> Result<(), AnyOwnerError> {
    let mut registry = lock_registry(table)?;
    let old = match registry.install_descriptor_version(descriptor) {
        Ok(old) => old,
        Err(error) => {
            drop(registry);
            return Err(error);
        }
    };
    drop(registry);
    drop(old);
    Ok(())
}

fn descriptor_identity_on(
    table: &Mutex<Registry>,
    token: AnyOwnerToken,
) -> Result<DescriptorIdentity, AnyOwnerError> {
    let registry = lock_registry(table)?;
    Ok(registry.live_owner(token)?.descriptor.identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Barrier, Weak};

    static V2_BLOCK_NEXT_CLONE: AtomicBool = AtomicBool::new(false);
    static V2_CLONE_GATE: OnceLock<Mutex<Option<(Arc<Barrier>, Arc<Barrier>)>>> = OnceLock::new();

    unsafe extern "C" fn clone_v2_byte(dst: *mut u8, src: *const u8) -> i32 {
        unsafe { *dst = *src };
        0
    }

    unsafe extern "C" fn drop_v2_byte(_payload: *mut u8) {}

    unsafe extern "C" fn clone_v2_with_release_gate(dst: *mut u8, src: *const u8) -> i32 {
        if V2_BLOCK_NEXT_CLONE.swap(false, Ordering::SeqCst) {
            let gates = V2_CLONE_GATE
                .get()
                .expect("clone gate installed")
                .lock()
                .unwrap()
                .as_ref()
                .cloned()
                .expect("clone gate configured");
            gates.0.wait();
            gates.1.wait();
        }
        unsafe { *dst = *src };
        0
    }

    fn v2_byte_payload(
        tag: crate::runtime::managed_any_payload_v2::ManagedAnyPayloadV2Tag,
        hash: u8,
        byte: u8,
    ) -> crate::runtime::managed_any_payload_v2::ManagedAnyPayloadV2 {
        use crate::runtime::managed_any_payload_v2::{
            ManagedAnyPayloadV2, ManagedAnyPayloadV2Glue, ManagedAnyPayloadV2Identity,
            ManagedAnyPayloadV2Tag, ManagedAnyPayloadV2TypeId,
        };
        let identity = ManagedAnyPayloadV2Identity {
            tag,
            type_id: ManagedAnyPayloadV2TypeId::parse(b"rt:0123456789abcdef0123456789abcdef")
                .unwrap(),
            descriptor_hash: [hash; 32],
            size: 1,
            align: 1,
        };
        unsafe {
            ManagedAnyPayloadV2::clone_from_borrowed(
                identity,
                ManagedAnyPayloadV2Glue {
                    clone: clone_v2_byte,
                    drop: drop_v2_byte,
                },
                &byte,
            )
        }
        .unwrap()
    }

    fn local_table(max_slots: u32) -> Mutex<Registry> {
        Mutex::new(Registry::with_slot_limit(max_slots).expect("test slot limit is valid"))
    }

    fn custom_descriptor(
        kind: AnyOwnerKind,
        version: u32,
        clone_glue: Arc<CloneGlue>,
        drop_glue: Arc<DropGlue>,
    ) -> Arc<DescriptorVersion> {
        Arc::new(DescriptorVersion::new(kind, version, clone_glue, drop_glue))
    }

    #[test]
    fn token_wire_fields_cover_class_and_signed_boundaries() {
        let positive_edge = AnyOwnerToken::from_parts(MAX_SLOT_INDEX, 0x7fff_ffff)
            .expect("maximum legal slot and pre-sign generation are valid");
        assert_eq!(positive_edge.wire_bits(), 0x7fff_ffff_ffff_ffff);
        assert_eq!(positive_edge.wire_bits() as i64, i64::MAX);

        let sign_edge = AnyOwnerToken::from_parts(MAX_SLOT_INDEX, 0x8000_0000)
            .expect("generation high bit is part of the full u32 generation");
        assert_eq!(sign_edge.wire_bits(), 0x8000_0000_ffff_ffff);
        assert!((sign_edge.wire_bits() as i64) < 0);
        assert_eq!(
            AnyOwnerToken::from_wire_bits(sign_edge.wire_bits()),
            Ok(sign_edge)
        );

        assert_eq!(
            AnyOwnerToken::from_wire_bits(0x0000_0001_0000_0001),
            Err(AnyOwnerError::InvalidToken),
            "a token without the Any class tag is rejected"
        );
        assert_eq!(
            AnyOwnerToken::from_wire_bits(0x0000_0000_8000_0001),
            Err(AnyOwnerError::InvalidToken),
            "zero generation is rejected"
        );
        assert_eq!(
            AnyOwnerToken::from_wire_bits(0x0000_0001_8000_0000),
            Err(AnyOwnerError::InvalidToken),
            "zero slot is rejected"
        );
        assert_eq!(
            AnyOwnerToken::from_parts(0, 1),
            Err(AnyOwnerError::InvalidToken)
        );
        assert_eq!(
            AnyOwnerToken::from_parts(1, 0),
            Err(AnyOwnerError::InvalidToken)
        );
    }

    #[test]
    fn stale_duplicate_and_forged_tokens_fail_closed() {
        let table = local_table(1);
        let first = allocate_i32_on(&table, 27).expect("first owner fits");
        assert_eq!(validate_on(&table, first), Ok(AnyOwnerKind::I32));
        assert_eq!(live_count_on(&table), Ok(1));
        assert_eq!(allocate_i32_on(&table, 28), Err(AnyOwnerError::Capacity));

        release_owner_on(&table, first).expect("live token releases");
        assert_eq!(
            release_owner_on(&table, first),
            Err(AnyOwnerError::StaleToken),
            "duplicate release is rejected"
        );
        let reused = allocate_i32_on(&table, 29).expect("released slot is reusable");
        assert_ne!(first, reused);
        assert_eq!(read_i32_on(&table, first), Err(AnyOwnerError::StaleToken));
        assert_eq!(read_i32_on(&table, reused), Ok(29));

        let absent_slot = AnyOwnerToken::from_parts(2, 1).expect("wire shape is valid");
        assert_eq!(
            validate_on(&table, absent_slot),
            Err(AnyOwnerError::InvalidToken),
            "a structurally valid token for an unallocated slot is forged"
        );
        let wrong_generation = AnyOwnerToken::from_parts(1, 1).expect("wire shape is valid");
        assert_eq!(
            validate_on(&table, wrong_generation),
            Err(AnyOwnerError::StaleToken)
        );
        release_owner_on(&table, reused).expect("clean up test owner");
        assert_eq!(live_count_on(&table), Ok(0));
    }

    #[test]
    fn v2_owner_uses_existing_generation_table_and_pins_actual_descriptor() {
        use super::super::managed_any_payload_v2::ManagedAnyPayloadV2Tag;

        let table = local_table(2);
        let source =
            allocate_managed_v2_on(&table, v2_byte_payload(ManagedAnyPayloadV2Tag::Bool, 93, 0))
                .unwrap();
        let source_identity = {
            let owner = snapshot_owner_v2_on(&table, source).unwrap();
            match owner.payload() {
                AnyOwnerPayload::ManagedV2(payload) => payload.identity(),
                _ => unreachable!(),
            }
        };
        assert_eq!(source_identity.tag, ManagedAnyPayloadV2Tag::Bool);
        assert_eq!(validate_on(&table, source), Ok(AnyOwnerKind::ManagedV2));
        assert_eq!(descriptor_identity_on(&table, source).unwrap().version, 2);

        let cloned = clone_owner_on(&table, source).unwrap();
        assert_ne!(source, cloned);
        {
            let clone = snapshot_owner_v2_on(&table, cloned).unwrap();
            match clone.payload() {
                AnyOwnerPayload::ManagedV2(payload) => {
                    assert_eq!(payload.identity(), source_identity);
                    assert_eq!(unsafe { *payload.payload_ptr() }, 0);
                }
                _ => unreachable!(),
            }
        }
        release_owner_on(&table, source).unwrap();
        assert_eq!(
            snapshot_owner_v2_on(&table, source).err(),
            Some(AnyOwnerError::StaleToken)
        );
        assert_eq!(
            snapshot_owner_v2_on(&table, cloned)
                .unwrap()
                .payload()
                .kind(),
            AnyOwnerKind::ManagedV2
        );
        release_owner_on(&table, cloned).unwrap();

        let reused = allocate_i32_on(&table, 1).unwrap();
        assert_eq!(reused.wire_bits() as u32, source.wire_bits() as u32);
        assert_ne!(reused.wire_bits(), source.wire_bits());
        release_owner_on(&table, reused).unwrap();
        assert_eq!(live_count_on(&table), Ok(0));
    }

    #[test]
    fn v2_clone_snapshot_survives_source_release_while_callback_is_running() {
        use super::super::managed_any_payload_v2::{
            ManagedAnyPayloadV2, ManagedAnyPayloadV2Glue, ManagedAnyPayloadV2Identity,
            ManagedAnyPayloadV2Tag, ManagedAnyPayloadV2TypeId,
        };

        let table = Arc::new(local_table(1));
        let identity = ManagedAnyPayloadV2Identity {
            tag: ManagedAnyPayloadV2Tag::Bool,
            type_id: ManagedAnyPayloadV2TypeId::parse(b"rt:0123456789abcdef0123456789abcdef")
                .unwrap(),
            descriptor_hash: [94; 32],
            size: 1,
            align: 1,
        };
        let payload = unsafe {
            ManagedAnyPayloadV2::clone_from_borrowed(
                identity,
                ManagedAnyPayloadV2Glue {
                    clone: clone_v2_with_release_gate,
                    drop: drop_v2_byte,
                },
                &77u8,
            )
        }
        .unwrap();
        let source = allocate_managed_v2_on(&table, payload).unwrap();
        let started = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        *V2_CLONE_GATE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some((Arc::clone(&started), Arc::clone(&resume)));
        V2_BLOCK_NEXT_CLONE.store(true, Ordering::SeqCst);

        let cloned = std::thread::scope(|scope| {
            let worker_table = Arc::clone(&table);
            let worker = scope.spawn(move || clone_owner_on(&worker_table, source));
            started.wait();
            release_owner_on(&table, source).unwrap();
            assert_eq!(
                snapshot_owner_v2_on(&table, source).err(),
                Some(AnyOwnerError::StaleToken)
            );
            resume.wait();
            worker.join().unwrap().unwrap()
        });

        *V2_CLONE_GATE
            .get()
            .expect("clone gate installed")
            .lock()
            .unwrap() = None;
        let clone = snapshot_owner_v2_on(&table, cloned).unwrap();
        match clone.payload() {
            AnyOwnerPayload::ManagedV2(payload) => {
                assert_eq!(payload.identity(), identity);
                assert_eq!(unsafe { *payload.payload_ptr() }, 77);
            }
            AnyOwnerPayload::I32(_) | AnyOwnerPayload::OwnedString(_) => unreachable!(),
        }
        release_owner_on(&table, cloned).unwrap();
        assert_eq!(live_count_on(&table), Ok(0));
    }

    #[test]
    fn typed_reads_reject_mismatched_descriptors_and_pin_versions() {
        let table = local_table(8);
        let original = allocate_string_on(&table, "pinned v1").expect("string owner allocates");
        let integer = allocate_i32_on(&table, 41).expect("integer owner allocates");
        assert_eq!(
            read_i32_on(&table, original),
            Err(AnyOwnerError::DescriptorMismatch)
        );
        assert_eq!(
            copy_string_on(&table, integer),
            Err(AnyOwnerError::DescriptorMismatch)
        );

        let v2_drops = Arc::new(AtomicUsize::new(0));
        let drop_count = Arc::clone(&v2_drops);
        let v2 = custom_descriptor(
            AnyOwnerKind::OwnedString,
            2,
            Arc::new(clone_owned_string),
            Arc::new(move |payload| {
                drop_count.fetch_add(1, Ordering::SeqCst);
                drop(payload);
            }),
        );
        register_descriptor_version_on(&table, v2).expect("install immutable v2 descriptor");

        let fresh = allocate_string_on(&table, "fresh v2").expect("new value uses v2");
        let old_clone = clone_owner_on(&table, original).expect("old value clones with pinned v1");
        let fresh_clone = clone_owner_on(&table, fresh).expect("new value clones with v2");
        assert_eq!(
            descriptor_identity_on(&table, original).unwrap().version,
            DESCRIPTOR_V1
        );
        assert_eq!(
            descriptor_identity_on(&table, old_clone).unwrap().version,
            DESCRIPTOR_V1
        );
        assert_eq!(descriptor_identity_on(&table, fresh).unwrap().version, 2);
        assert_eq!(
            descriptor_identity_on(&table, fresh_clone).unwrap().version,
            2
        );
        assert_eq!(copy_string_on(&table, old_clone).unwrap(), "pinned v1");

        release_owner_on(&table, original).unwrap();
        release_owner_on(&table, integer).unwrap();
        release_owner_on(&table, fresh).unwrap();
        release_owner_on(&table, old_clone).unwrap();
        release_owner_on(&table, fresh_clone).unwrap();
        assert_eq!(v2_drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn exhausted_generation_retires_slot_without_wrapping() {
        let table = local_table(1);
        let first = allocate_i32_on(&table, 7).expect("owner allocates");
        {
            let mut registry = table.lock().unwrap();
            registry.slots[0].generation = u32::MAX;
        }
        let last_generation = AnyOwnerToken::from_parts(1, u32::MAX).unwrap();
        assert_eq!(read_i32_on(&table, first), Err(AnyOwnerError::StaleToken));
        release_owner_on(&table, last_generation).expect("last generation remains releasable");
        assert_eq!(
            validate_on(&table, last_generation),
            Err(AnyOwnerError::StaleToken)
        );
        assert_eq!(
            allocate_i32_on(&table, 8),
            Err(AnyOwnerError::Capacity),
            "a generation-exhausted slot is permanently retired"
        );
        assert!(table.lock().unwrap().slots[0].retired);
    }

    #[test]
    fn descriptor_drop_glue_can_reenter_after_release_unlocks() {
        let table = Arc::new(local_table(4));
        let table_weak_cell: Arc<OnceLock<Weak<Mutex<Registry>>>> = Arc::new(OnceLock::new());
        assert!(table_weak_cell.set(Arc::downgrade(&table)).is_ok());
        let unlocked = Arc::new(AtomicBool::new(false));
        let reentered = Arc::new(AtomicBool::new(false));
        let reentry_token = Arc::new(Mutex::new(None));
        let unlocked_in_glue = Arc::clone(&unlocked);
        let reentered_in_glue = Arc::clone(&reentered);
        let token_in_glue = Arc::clone(&reentry_token);
        let weak_in_glue = Arc::clone(&table_weak_cell);

        let descriptor = custom_descriptor(
            AnyOwnerKind::I32,
            2,
            Arc::new(clone_i32),
            Arc::new(move |payload| {
                drop(payload);
                let Some(registry) = weak_in_glue.get().and_then(Weak::upgrade) else {
                    return;
                };
                let lock_was_free = match registry.try_lock() {
                    Ok(guard) => {
                        drop(guard);
                        true
                    }
                    Err(_) => false,
                };
                unlocked_in_glue.store(lock_was_free, Ordering::SeqCst);
                if lock_was_free && !reentered_in_glue.swap(true, Ordering::SeqCst) {
                    if let Ok(token) = allocate_i32_on(&registry, 99) {
                        *token_in_glue.lock().unwrap() = Some(token);
                    }
                }
            }),
        );
        register_descriptor_version_on(&table, descriptor).expect("install reentrant test glue");

        let token = allocate_i32_on(&table, 12).expect("owner allocates");
        release_owner_on(&table, token).expect("release runs its drop glue");
        assert!(unlocked.load(Ordering::SeqCst));

        let nested = reentry_token.lock().unwrap().take();
        let nested = nested.expect("drop glue successfully reentered the table");
        assert_eq!(read_i32_on(&table, nested), Ok(99));
        release_owner_on(&table, nested).expect("release reentrant owner");
        assert_eq!(live_count_on(&table), Ok(0));
    }

    #[test]
    fn poisoned_registry_fails_closed() {
        let table = local_table(2);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = table.lock().unwrap();
            panic!("poison the local test registry");
        }));
        assert_eq!(allocate_i32_on(&table, 1), Err(AnyOwnerError::Poisoned));
    }

    #[test]
    fn retained_owners_keep_exact_descriptors_in_both_release_orders() {
        for source_first in [false, true] {
            let table = local_table(8);
            let integer = allocate_i32_on(&table, 0).unwrap();
            let text = allocate_string_on(&table, "值\0雪🧭").unwrap();
            for kind in [AnyOwnerKind::I32, AnyOwnerKind::OwnedString] {
                let clone: Arc<CloneGlue> = match kind {
                    AnyOwnerKind::I32 => Arc::new(clone_i32),
                    AnyOwnerKind::OwnedString => Arc::new(clone_owned_string),
                    AnyOwnerKind::ManagedV2 => unreachable!("v1 descriptor test"),
                };
                let v2 = custom_descriptor(kind, 2, clone, Arc::new(drop_payload));
                register_descriptor_version_on(&table, v2).unwrap();
            }
            let retained_integer = clone_owner_on(&table, integer).unwrap();
            let retained_text = clone_owner_on(&table, text).unwrap();
            let fresh_integer = allocate_i32_on(&table, 27).unwrap();
            let fresh_text = allocate_string_on(&table, "fresh v2").unwrap();
            assert_eq!(live_count_on(&table), Ok(6));
            for (source, retained, kind) in [
                (integer, retained_integer, AnyOwnerKind::I32),
                (text, retained_text, AnyOwnerKind::OwnedString),
            ] {
                assert_ne!(source, retained);
                assert_eq!(
                    descriptor_identity_on(&table, retained),
                    Ok(DescriptorIdentity {
                        kind,
                        version: DESCRIPTOR_V1
                    })
                );
                assert_eq!(
                    descriptor_identity_on(&table, source),
                    descriptor_identity_on(&table, retained)
                );
            }
            for fresh in [fresh_integer, fresh_text] {
                assert_eq!(descriptor_identity_on(&table, fresh).unwrap().version, 2);
            }
            let (first, remaining) = if source_first {
                ([integer, text], [retained_integer, retained_text])
            } else {
                ([retained_integer, retained_text], [integer, text])
            };
            for token in first {
                release_owner_on(&table, token).unwrap();
                assert_eq!(
                    descriptor_identity_on(&table, token),
                    Err(AnyOwnerError::StaleToken)
                );
            }
            assert_eq!(live_count_on(&table), Ok(4));
            assert_eq!(read_i32_on(&table, remaining[0]), Ok(0));
            assert_eq!(
                copy_string_on(&table, remaining[1]),
                Ok("值\0雪🧭".to_owned())
            );
            assert_eq!(
                read_i32_on(&table, remaining[1]),
                Err(AnyOwnerError::DescriptorMismatch)
            );
            assert_eq!(
                copy_string_on(&table, remaining[0]),
                Err(AnyOwnerError::DescriptorMismatch)
            );
            for token in remaining.into_iter().chain([fresh_integer, fresh_text]) {
                release_owner_on(&table, token).unwrap();
            }
            assert_eq!(live_count_on(&table), Ok(0));
        }
    }

    #[test]
    fn owner_clone_capacity_failure_restores_local_baseline() {
        for kind in [AnyOwnerKind::I32, AnyOwnerKind::OwnedString] {
            let table = local_table(1);
            let source = match kind {
                AnyOwnerKind::I32 => allocate_i32_on(&table, -17),
                AnyOwnerKind::OwnedString => allocate_string_on(&table, "值\0🌿"),
                AnyOwnerKind::ManagedV2 => unreachable!("v1 owner test"),
            }
            .unwrap();
            let descriptor = descriptor_identity_on(&table, source).unwrap();
            assert_eq!(clone_owner_on(&table, source), Err(AnyOwnerError::Capacity));
            assert_eq!(live_count_on(&table), Ok(1));
            assert_eq!(descriptor_identity_on(&table, source), Ok(descriptor));
            release_owner_on(&table, source).unwrap();
            assert_eq!(live_count_on(&table), Ok(0));
            assert_eq!(
                clone_owner_on(&table, source),
                Err(AnyOwnerError::StaleToken)
            );
        }
    }

    #[test]
    fn descriptor_mismatched_clone_never_publishes_and_drops_after_unlock() {
        let table = Arc::new(local_table(2));
        let weak_table = Arc::downgrade(&table);
        let drops = Arc::new(AtomicUsize::new(0));
        let unlocked = Arc::new(AtomicBool::new(true));
        let drops_in_glue = Arc::clone(&drops);
        let unlocked_in_glue = Arc::clone(&unlocked);
        let descriptor = custom_descriptor(
            AnyOwnerKind::I32,
            2,
            Arc::new(|_| Ok(AnyOwnerPayload::OwnedString("wrong shape".to_owned()))),
            Arc::new(move |payload| {
                drop(payload);
                drops_in_glue.fetch_add(1, Ordering::SeqCst);
                let registry = weak_table.upgrade().expect("local registry is live");
                if registry.try_lock().is_err() {
                    unlocked_in_glue.store(false, Ordering::SeqCst);
                }
            }),
        );
        register_descriptor_version_on(&table, descriptor).unwrap();
        let source = allocate_i32_on(&table, 41).unwrap();
        assert_eq!(
            clone_owner_on(&table, source),
            Err(AnyOwnerError::DescriptorMismatch)
        );
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "unpublished mismatched payload is released"
        );
        assert!(unlocked.load(Ordering::SeqCst));
        assert_eq!(live_count_on(&table), Ok(1));
        assert_eq!(read_i32_on(&table, source), Ok(41));
        assert_eq!(descriptor_identity_on(&table, source).unwrap().version, 2);
        release_owner_on(&table, source).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert!(unlocked.load(Ordering::SeqCst));
        assert_eq!(live_count_on(&table), Ok(0));
    }
}

#[cfg(test)]
mod receipt_v1_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn table(max_slots: u32) -> Mutex<Registry> {
        Mutex::new(Registry::with_slot_limit(max_slots).unwrap())
    }

    #[test]
    fn managed_v1_receipts_reject_wrong_kind_version_and_descriptor_without_consuming() {
        let table = table(4);
        let token = allocate_string_on(&table, "owned\0雪").unwrap();
        let descriptor = descriptor_identity_on(&table, token).unwrap();
        assert_eq!(
            read_i32_v1_on(&table, token, descriptor, AnyOwnerKind::I32, 1),
            Err(AnyOwnerReceiptError::KindMismatch)
        );
        assert_eq!(
            copy_string_v1_on(&table, token, descriptor, AnyOwnerKind::OwnedString, 2),
            Err(AnyOwnerReceiptError::VersionMismatch)
        );
        assert_eq!(
            release_owner_v1_on(&table, token, descriptor, 2),
            Err(AnyOwnerReceiptError::VersionMismatch)
        );
        let forged = DescriptorIdentity {
            kind: AnyOwnerKind::I32,
            version: 1,
        };
        assert_eq!(
            clone_owner_v1_on(&table, token, forged, 1),
            Err(AnyOwnerReceiptError::Owner(
                AnyOwnerError::DescriptorMismatch
            ))
        );
        assert_eq!(
            copy_string_v1_on(&table, token, descriptor, AnyOwnerKind::OwnedString, 1),
            Ok("owned\0雪".to_owned())
        );
        release_owner_v1_on(&table, token, descriptor, 1).unwrap();
        assert_eq!(
            release_owner_v1_on(&table, token, descriptor, 1),
            Err(AnyOwnerReceiptError::Owner(AnyOwnerError::StaleToken))
        );
        assert_eq!(live_count_on(&table), Ok(0));
    }

    #[test]
    fn managed_v1_clone_pins_old_descriptor_after_current_descriptor_update() {
        let table = table(4);
        let original = allocate_string_on(&table, "v1\0payload").unwrap();
        let descriptor = descriptor_identity_on(&table, original).unwrap();
        register_descriptor_version_on(
            &table,
            Arc::new(DescriptorVersion::new(
                AnyOwnerKind::OwnedString,
                2,
                Arc::new(clone_owned_string),
                Arc::new(drop_payload),
            )),
        )
        .unwrap();
        let clone = clone_owner_v1_on(&table, original, descriptor, 1).unwrap();
        let new_v2 = allocate_string_on(&table, "v2").unwrap();
        assert_eq!(descriptor_identity_on(&table, clone), Ok(descriptor));
        assert_eq!(descriptor_identity_on(&table, new_v2).unwrap().version, 2);
        let v2_identity = descriptor_identity_on(&table, new_v2).unwrap();
        assert_eq!(
            clone_owner_v1_on(&table, new_v2, v2_identity, 1),
            Err(AnyOwnerReceiptError::VersionMismatch)
        );
        release_owner_v1_on(&table, original, descriptor, 1).unwrap();
        assert_eq!(
            copy_string_v1_on(&table, clone, descriptor, AnyOwnerKind::OwnedString, 1),
            Ok("v1\0payload".to_owned())
        );
        release_owner_v1_on(&table, clone, descriptor, 1).unwrap();
        release_owner_on(&table, new_v2).unwrap();
        assert_eq!(live_count_on(&table), Ok(0));
    }

    #[test]
    fn managed_v1_capacity_rolls_back_and_receipt_validation_precedes_failpoint() {
        let table = table(1);
        let token = allocate_i32_on(&table, 0).unwrap();
        let descriptor = descriptor_identity_on(&table, token).unwrap();
        assert_eq!(
            clone_owner_v1_on(&table, token, descriptor, 1),
            Err(AnyOwnerReceiptError::Owner(AnyOwnerError::Capacity))
        );
        let called = std::cell::Cell::new(false);
        assert_eq!(
            clone_owner_v1_on_with_checkpoint(&table, token, descriptor, 2, || {
                called.set(true);
                Err(AnyOwnerError::Capacity)
            }),
            Err(AnyOwnerReceiptError::VersionMismatch)
        );
        assert!(!called.get());
        assert_eq!(
            read_i32_v1_on(&table, token, descriptor, AnyOwnerKind::I32, 1),
            Ok(0)
        );
        release_owner_v1_on(&table, token, descriptor, 1).unwrap();
        assert_eq!(live_count_on(&table), Ok(0));
    }

    #[test]
    fn managed_v1_copy_pins_payload_across_concurrent_release_and_runs_glue_without_lock() {
        let table = Arc::new(table(2));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let resume_rx = Mutex::new(resume_rx);
        let dropped = Arc::new(AtomicUsize::new(0));
        let dropped_in_glue = Arc::clone(&dropped);
        let weak = Arc::downgrade(&table);
        table.lock().unwrap().current_descriptors[1] = Arc::new(DescriptorVersion::new(
            AnyOwnerKind::OwnedString,
            1,
            Arc::new(move |payload| {
                let live = weak.upgrade().unwrap();
                assert!(
                    live.try_lock().is_ok(),
                    "clone glue executes after the receipt acquire unlocks"
                );
                entered_tx.send(()).unwrap();
                resume_rx.lock().unwrap().recv().unwrap();
                clone_owned_string(payload)
            }),
            Arc::new(move |payload| {
                dropped_in_glue.fetch_add(1, Ordering::SeqCst);
                drop(payload);
            }),
        ));
        let token = allocate_string_on(&table, "snapshot\0雪").unwrap();
        let descriptor = descriptor_identity_on(&table, token).unwrap();
        let worker_table = Arc::clone(&table);
        let worker = std::thread::spawn(move || {
            copy_string_v1_on(
                &worker_table,
                token,
                descriptor,
                AnyOwnerKind::OwnedString,
                1,
            )
        });
        entered_rx.recv().unwrap();
        release_owner_v1_on(&table, token, descriptor, 1).unwrap();
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "acquired payload is still pinned"
        );
        assert_eq!(live_count_on(&table), Ok(0));
        resume_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), Ok("snapshot\0雪".to_owned()));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(
            copy_string_v1_on(&table, token, descriptor, AnyOwnerKind::OwnedString, 1),
            Err(AnyOwnerReceiptError::Owner(AnyOwnerError::StaleToken))
        );
    }
}
