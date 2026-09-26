//! Test-only model for a typed, owning Map/Any representation.
//!
//! This deliberately does not encode its identity into the existing `i64`
//! `ValueHandle` ABI and is not connected to runtime FFI or MIR routing. It
//! demonstrates the owner graph that a future typed ingress/egress contract
//! would need to preserve.

use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, Weak,
};

static NEXT_ANY_OWNER_TABLE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
struct DropProbe(Arc<AtomicUsize>);

impl DropProbe {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    fn record_drop(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct AnyStringPayload {
    text: Box<str>,
    drop_probe: DropProbe,
}

impl Drop for AnyStringPayload {
    fn drop(&mut self) {
        self.drop_probe.record_drop();
    }
}

#[derive(Clone)]
struct AnyStringOwner(Arc<AnyStringPayload>);

impl AnyStringOwner {
    fn new(text: &str, drop_probe: DropProbe) -> Self {
        Self(Arc::new(AnyStringPayload {
            text: text.into(),
            drop_probe,
        }))
    }

    fn as_str(&self) -> &str {
        &self.0.text
    }
}

struct OwnedAggregate {
    fields: Vec<AnyValue>,
    drop_probe: DropProbe,
}

impl Drop for OwnedAggregate {
    fn drop(&mut self) {
        self.drop_probe.record_drop();
    }
}

#[derive(Clone)]
enum AnyValue {
    I64(i64),
    String(AnyStringOwner),
    Aggregate(Arc<OwnedAggregate>),
}

impl AnyValue {
    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::I64(value) => Some(*value),
            Self::String(_) | Self::Aggregate(_) => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value.as_str()),
            Self::I64(_) | Self::Aggregate(_) => None,
        }
    }

    fn aggregate_str_field(&self, index: usize) -> Option<&str> {
        match self {
            Self::Aggregate(value) => value.fields.get(index)?.as_str(),
            Self::I64(_) | Self::String(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DescriptorId {
    slot: u32,
    generation: u32,
}

impl DescriptorId {
    const I64: Self = Self::new(1, 1);
    const STRING: Self = Self::new(2, 1);
    const AGGREGATE: Self = Self::new(3, 1);

    const fn new(slot: u32, generation: u32) -> Self {
        Self { slot, generation }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueShape {
    I64,
    String,
    Aggregate,
}

impl AnyValue {
    fn shape(&self) -> ValueShape {
        match self {
            Self::I64(_) => ValueShape::I64,
            Self::String(_) => ValueShape::String,
            Self::Aggregate(_) => ValueShape::Aggregate,
        }
    }
}

struct DescriptorVersion {
    identity: DescriptorId,
    shape: ValueShape,
    drop_glue: Arc<dyn Fn(AnyValue) + Send + Sync>,
    drop_value_probe: DropProbe,
    drop_probe: DropProbe,
}

impl Drop for DescriptorVersion {
    fn drop(&mut self) {
        self.drop_probe.record_drop();
    }
}

fn descriptor_version(identity: DescriptorId, shape: ValueShape) -> Arc<DescriptorVersion> {
    let drop_value_probe = DropProbe::default();
    let callback_probe = drop_value_probe.clone();
    Arc::new(DescriptorVersion {
        identity,
        shape,
        drop_glue: Arc::new(move |value| {
            debug_assert_eq!(value.shape(), shape);
            callback_probe.record_drop();
            drop(value);
        }),
        drop_value_probe,
        drop_probe: DropProbe::default(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnyHandle {
    table_id: u64,
    slot: u32,
    generation: u32,
    descriptor: DescriptorId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MapRootId {
    table_id: u64,
    slot: u32,
    generation: u32,
}

type MapRootRetireObserver = Arc<dyn Fn(MapRootId) + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum OwnerError {
    InvalidHandle,
    InvalidRoot,
    WrongTable,
    StaleHandle,
    StaleRoot,
    UnknownDescriptor,
    StaleDescriptor,
    DescriptorMismatch,
    DuplicateOwnerToken,
    Capacity,
    Poisoned,
    InjectedCommitFailure,
}

#[derive(Clone)]
struct TypedAny {
    descriptor: Arc<DescriptorVersion>,
    value: AnyValue,
}

struct OwnerSlot {
    generation: u32,
    value: Option<TypedAny>,
    retired: bool,
}

struct OwnerState {
    known_descriptor_slots: std::collections::BTreeSet<u32>,
    current_descriptors: BTreeMap<u32, Arc<DescriptorVersion>>,
    slots: Vec<OwnerSlot>,
    free: Vec<u32>,
    max_slots: usize,
}

struct MapRootSlot {
    generation: u32,
    root: Option<Weak<OwnedMapRoot>>,
    retired: bool,
}

struct MapRootState {
    slots: Vec<MapRootSlot>,
    free: Vec<u32>,
    max_slots: usize,
}

impl Drop for OwnerState {
    fn drop(&mut self) {
        for slot in &mut self.slots {
            if let Some(TypedAny { descriptor, value }) = slot.value.take() {
                (descriptor.drop_glue)(value);
            }
        }
    }
}

impl OwnerState {
    fn validate(&self, table_id: u64, handle: AnyHandle) -> Result<&TypedAny, OwnerError> {
        if handle.table_id != table_id {
            return Err(OwnerError::WrongTable);
        }
        let slot = self
            .slots
            .get(handle.slot as usize)
            .ok_or(OwnerError::InvalidHandle)?;
        if slot.generation != handle.generation || slot.value.is_none() {
            return Err(OwnerError::StaleHandle);
        }
        let value = slot.value.as_ref().expect("checked live owner slot");
        if value.descriptor.identity != handle.descriptor {
            return if self
                .known_descriptor_slots
                .contains(&handle.descriptor.slot)
            {
                Err(OwnerError::DescriptorMismatch)
            } else {
                Err(OwnerError::UnknownDescriptor)
            };
        }
        if value.value.shape() != value.descriptor.shape {
            return Err(OwnerError::DescriptorMismatch);
        }
        Ok(value)
    }

    fn current_descriptor(
        &self,
        identity: DescriptorId,
    ) -> Result<Arc<DescriptorVersion>, OwnerError> {
        let Some(current) = self.current_descriptors.get(&identity.slot) else {
            return Err(OwnerError::UnknownDescriptor);
        };
        if current.identity != identity {
            return if identity.generation < current.identity.generation {
                Err(OwnerError::StaleDescriptor)
            } else {
                Err(OwnerError::UnknownDescriptor)
            };
        }
        Ok(current.clone())
    }

    fn replace_descriptor(
        &mut self,
        current: DescriptorId,
        shape: ValueShape,
    ) -> Result<(DescriptorId, Arc<DescriptorVersion>), OwnerError> {
        let Some(current_version) = self.current_descriptors.get(&current.slot) else {
            return Err(OwnerError::UnknownDescriptor);
        };
        if current_version.identity != current {
            return if current.generation < current_version.identity.generation {
                Err(OwnerError::StaleDescriptor)
            } else {
                Err(OwnerError::UnknownDescriptor)
            };
        }
        let generation = current
            .generation
            .checked_add(1)
            .ok_or(OwnerError::Capacity)?;
        let identity = DescriptorId::new(current.slot, generation);
        let version = descriptor_version(identity, shape);
        self.known_descriptor_slots.insert(current.slot);
        let previous = self
            .current_descriptors
            .insert(current.slot, version)
            .expect("current descriptor was validated");
        Ok((identity, previous))
    }

    fn allocate_many(
        &mut self,
        table_id: u64,
        values: &mut Vec<TypedAny>,
    ) -> Result<Vec<AnyHandle>, OwnerError> {
        for value in values.iter() {
            if value.value.shape() != value.descriptor.shape {
                return Err(OwnerError::DescriptorMismatch);
            }
        }

        let append_capacity = self.max_slots.saturating_sub(self.slots.len());
        let available = self
            .free
            .len()
            .checked_add(append_capacity)
            .ok_or(OwnerError::Capacity)?;
        if values.len() > available {
            return Err(OwnerError::Capacity);
        }

        let append_count = values.len().saturating_sub(self.free.len());
        let final_slot_count = self
            .slots
            .len()
            .checked_add(append_count)
            .ok_or(OwnerError::Capacity)?;
        if final_slot_count > self.max_slots || final_slot_count > u32::MAX as usize {
            return Err(OwnerError::Capacity);
        }

        let reused = values.len().min(self.free.len());
        let mut seen_free_slots = std::collections::BTreeSet::new();
        for index in self.free.iter().rev().take(reused) {
            if !seen_free_slots.insert(*index) {
                return Err(OwnerError::Capacity);
            }
            let slot = self
                .slots
                .get(*index as usize)
                .ok_or(OwnerError::Capacity)?;
            if slot.retired || slot.value.is_some() {
                return Err(OwnerError::Capacity);
            }
        }

        let mut handles = Vec::with_capacity(values.len());
        for value in std::mem::take(values) {
            let slot_index = if let Some(index) = self.free.pop() {
                index
            } else {
                let index = u32::try_from(self.slots.len()).expect("slot count was preflighted");
                self.slots.push(OwnerSlot {
                    generation: 1,
                    value: None,
                    retired: false,
                });
                index
            };
            let slot = &mut self.slots[slot_index as usize];
            slot.value = Some(value);
            handles.push(AnyHandle {
                table_id,
                slot: slot_index,
                generation: slot.generation,
                descriptor: slot
                    .value
                    .as_ref()
                    .expect("new owner slot is populated")
                    .descriptor
                    .identity,
            });
        }
        Ok(handles)
    }
}

/// Generation-checked, typed owner tokens for the isolated test model.
///
/// Every allocated token is a distinct owner. Releasing a token invalidates
/// its generation, while retaining a value allocates a new token. This is not
/// the production `ValueHandle` registry or a C ABI implementation.
struct AnyOwnerTable {
    table_id: u64,
    state: std::sync::Mutex<OwnerState>,
    map_roots: std::sync::Mutex<MapRootState>,
}

impl AnyOwnerTable {
    fn with_capacity(max_slots: usize) -> Self {
        let table_id = NEXT_ANY_OWNER_TABLE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .expect("Any owner table identity space exhausted");
        let definitions = [
            (DescriptorId::I64, ValueShape::I64),
            (DescriptorId::STRING, ValueShape::String),
            (DescriptorId::AGGREGATE, ValueShape::Aggregate),
        ];
        let current_descriptors = definitions
            .iter()
            .map(|(identity, shape)| (identity.slot, descriptor_version(*identity, *shape)))
            .collect();
        let known_descriptor_slots = definitions
            .iter()
            .map(|(identity, _)| identity.slot)
            .collect();
        Self {
            table_id,
            state: std::sync::Mutex::new(OwnerState {
                known_descriptor_slots,
                current_descriptors,
                slots: Vec::new(),
                free: Vec::new(),
                max_slots: max_slots.min(u32::MAX as usize),
            }),
            map_roots: std::sync::Mutex::new(MapRootState {
                slots: Vec::new(),
                free: Vec::new(),
                max_slots: 1024,
            }),
        }
    }

    fn create_map_root(
        self: &Arc<Self>,
        entries: BTreeMap<Arc<str>, AnyHandle>,
    ) -> Result<Arc<OwnedMapRoot>, OwnerError> {
        self.create_map_root_inner(entries, None)
    }

    #[cfg(test)]
    fn create_map_root_with_retire_observer_for_test(
        self: &Arc<Self>,
        entries: BTreeMap<Arc<str>, AnyHandle>,
        observer: MapRootRetireObserver,
    ) -> Result<Arc<OwnedMapRoot>, OwnerError> {
        self.create_map_root_inner(entries, Some(observer))
    }

    fn create_map_root_inner(
        self: &Arc<Self>,
        entries: BTreeMap<Arc<str>, AnyHandle>,
        retire_observer: Option<MapRootRetireObserver>,
    ) -> Result<Arc<OwnedMapRoot>, OwnerError> {
        for handle in entries.values() {
            self.validate_handle(*handle)?;
        }
        let mut roots = self.map_roots.lock().map_err(|_| OwnerError::Poisoned)?;
        let index = if let Some(index) = roots.free.pop() {
            let index = index as usize;
            if index >= roots.slots.len() {
                return Err(OwnerError::Capacity);
            }
            index
        } else {
            if roots.slots.len() >= roots.max_slots {
                return Err(OwnerError::Capacity);
            }
            let index = roots.slots.len();
            roots.slots.push(MapRootSlot {
                generation: 1,
                root: None,
                retired: false,
            });
            index
        };
        let (generation, unavailable) = {
            let slot = &roots.slots[index];
            (slot.generation, slot.retired || slot.root.is_some())
        };
        if unavailable {
            roots.slots[index].retired = true;
            roots
                .free
                .retain(|free_index| *free_index as usize != index);
            return Err(OwnerError::Capacity);
        }
        let identity = MapRootId {
            table_id: self.table_id,
            slot: index as u32,
            generation,
        };
        let root = Arc::new(OwnedMapRoot {
            identity,
            owners: self.clone(),
            entries,
            retire_observer,
        });
        roots.slots[index].root = Some(Arc::downgrade(&root));
        Ok(root)
    }

    fn resolve_map_root(&self, identity: MapRootId) -> Result<Arc<OwnedMapRoot>, OwnerError> {
        if identity.table_id != self.table_id {
            return Err(OwnerError::WrongTable);
        }
        let roots = self.map_roots.lock().map_err(|_| OwnerError::Poisoned)?;
        let slot = roots
            .slots
            .get(identity.slot as usize)
            .ok_or(OwnerError::InvalidRoot)?;
        if slot.generation != identity.generation {
            return Err(OwnerError::StaleRoot);
        }
        let root = slot
            .root
            .as_ref()
            .ok_or(OwnerError::StaleRoot)?
            .upgrade()
            .ok_or(OwnerError::StaleRoot)?;
        if root.identity != identity {
            return Err(OwnerError::StaleRoot);
        }
        Ok(root)
    }

    fn live_map_root_count(&self) -> Result<usize, OwnerError> {
        let roots = self.map_roots.lock().map_err(|_| OwnerError::Poisoned)?;
        Ok(roots
            .slots
            .iter()
            .filter(|slot| {
                slot.root
                    .as_ref()
                    .is_some_and(|root| root.strong_count() > 0)
            })
            .count())
    }

    fn retire_map_root(&self, identity: MapRootId) {
        if identity.table_id != self.table_id {
            return;
        }
        let retired_weak = {
            let mut roots = self
                .map_roots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(slot) = roots.slots.get_mut(identity.slot as usize) else {
                return;
            };
            if slot.generation != identity.generation || slot.root.is_none() {
                return;
            }
            let weak = slot.root.take();
            let reusable = !slot.retired;
            if let Some(next_generation) = slot.generation.checked_add(1) {
                slot.generation = next_generation;
                if reusable {
                    roots.free.push(identity.slot);
                }
            } else {
                slot.retired = true;
            }
            weak
        };
        drop(retired_weak);
    }

    fn allocate(&self, descriptor: DescriptorId, value: AnyValue) -> Result<AnyHandle, OwnerError> {
        let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        let version = state.current_descriptor(descriptor)?;
        let mut values = vec![TypedAny {
            descriptor: version,
            value,
        }];
        let result = state.allocate_many(self.table_id, &mut values);
        drop(state);
        drop(values);
        result.map(|mut handles| handles.pop().expect("single value yields one owner token"))
    }

    fn allocate_many(&self, values: Vec<TypedAny>) -> Result<Vec<AnyHandle>, OwnerError> {
        let mut values = values;
        let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        let result = state.allocate_many(self.table_id, &mut values);
        drop(state);
        drop(values);
        result
    }

    fn replace_descriptor_for_test_only(
        &self,
        current: DescriptorId,
        shape: ValueShape,
    ) -> Result<DescriptorId, OwnerError> {
        let (identity, previous) = {
            let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
            state.replace_descriptor(current, shape)?
        };
        drop(previous);
        Ok(identity)
    }

    fn descriptor_drop_probe_for_test_only(
        &self,
        handle: AnyHandle,
    ) -> Result<DropProbe, OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        Ok(state
            .validate(self.table_id, handle)?
            .descriptor
            .drop_probe
            .clone())
    }

    #[cfg(test)]
    fn descriptor_value_drop_probe_for_test_only(
        &self,
        handle: AnyHandle,
    ) -> Result<DropProbe, OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        Ok(state
            .validate(self.table_id, handle)?
            .descriptor
            .drop_value_probe
            .clone())
    }

    fn validate_handle(&self, handle: AnyHandle) -> Result<(), OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        state.validate(self.table_id, handle).map(|_| ())
    }

    fn clone_value(&self, handle: AnyHandle) -> Result<TypedAny, OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        Ok(state.validate(self.table_id, handle)?.clone())
    }

    fn read_i64(&self, handle: AnyHandle, expected: DescriptorId) -> Result<i64, OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        let value = state.validate(self.table_id, handle)?;
        if value.descriptor.identity != expected || value.descriptor.shape != ValueShape::I64 {
            return Err(OwnerError::DescriptorMismatch);
        }
        value.value.as_i64().ok_or(OwnerError::DescriptorMismatch)
    }

    fn read_string(&self, handle: AnyHandle, expected: DescriptorId) -> Result<String, OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        let value = state.validate(self.table_id, handle)?;
        if value.descriptor.identity != expected || value.descriptor.shape != ValueShape::String {
            return Err(OwnerError::DescriptorMismatch);
        }
        value
            .value
            .as_str()
            .map(str::to_owned)
            .ok_or(OwnerError::DescriptorMismatch)
    }

    fn retain(&self, handle: AnyHandle) -> Result<AnyHandle, OwnerError> {
        let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        let value = state.validate(self.table_id, handle)?.clone();
        let mut values = vec![value];
        let result = state.allocate_many(self.table_id, &mut values);
        drop(state);
        drop(values);
        result.map(|mut handles| handles.pop().expect("one retained value yields one token"))
    }

    fn retain_many(&self, handles: &[AnyHandle]) -> Result<Vec<AnyHandle>, OwnerError> {
        let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        for handle in handles {
            state.validate(self.table_id, *handle)?;
        }
        let mut values = handles
            .iter()
            .map(|handle| {
                state
                    .validate(self.table_id, *handle)
                    .expect("prevalidated owner token")
                    .clone()
            })
            .collect();
        let result = state.allocate_many(self.table_id, &mut values);
        drop(state);
        drop(values);
        result
    }

    fn release(&self, handle: AnyHandle) -> Result<(), OwnerError> {
        let released = {
            let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
            state.validate(self.table_id, handle)?;
            let index = handle.slot as usize;
            let released = state.slots[index]
                .value
                .take()
                .expect("validated owner slot is populated");
            let slot = &mut state.slots[index];
            if let Some(next_generation) = slot.generation.checked_add(1) {
                slot.generation = next_generation;
                state.free.push(handle.slot);
            } else {
                // Retire instead of wrapping and making an old token live again.
                slot.retired = true;
            }
            released
        };
        // Descriptor glue may release nested owners; never run it under the
        // table lock, and always dispatch through the pinned version.
        let TypedAny { descriptor, value } = released;
        (descriptor.drop_glue)(value);
        drop(descriptor);
        Ok(())
    }

    fn live_count(&self) -> Result<usize, OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        Ok(state
            .slots
            .iter()
            .filter(|slot| slot.value.is_some())
            .count())
    }

    #[cfg(test)]
    fn force_generation_for_test(
        &self,
        handle: AnyHandle,
        generation: u32,
    ) -> Result<AnyHandle, OwnerError> {
        let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        state.validate(self.table_id, handle)?;
        state.slots[handle.slot as usize].generation = generation;
        Ok(AnyHandle {
            generation,
            ..handle
        })
    }

    #[cfg(test)]
    fn poison_for_test(&self) {
        let _guard = self.state.lock().expect("fresh owner table");
        panic!("test-only owner table poison");
    }
}

struct MapRoot {
    entries: BTreeMap<Arc<str>, AnyValue>,
    drop_probe: Option<DropProbe>,
}

impl Drop for MapRoot {
    fn drop(&mut self) {
        if let Some(probe) = &self.drop_probe {
            probe.record_drop();
        }
    }
}

#[derive(Clone)]
struct PrototypeMap {
    root: Arc<MapRoot>,
}

impl PrototypeMap {
    fn empty() -> Self {
        Self::from_entries(BTreeMap::new(), None)
    }

    fn from_entries(entries: BTreeMap<Arc<str>, AnyValue>, drop_probe: Option<DropProbe>) -> Self {
        Self {
            root: Arc::new(MapRoot {
                entries,
                drop_probe,
            }),
        }
    }

    fn insert(&self, key: &str, value: AnyValue) -> Self {
        let mut entries = self.root.entries.clone();
        entries.insert(Arc::from(key), value);
        Self::from_entries(entries, None)
    }

    fn remove(&self, key: &str) -> Self {
        let mut entries = self.root.entries.clone();
        entries.remove(key);
        Self::from_entries(entries, None)
    }

    fn get(&self, key: &str) -> Option<AnyValue> {
        self.root.entries.get(key).cloned()
    }

    fn values_snapshot(&self) -> Vec<AnyValue> {
        self.root.entries.values().cloned().collect()
    }

    fn keys(&self) -> Vec<String> {
        self.root
            .entries
            .keys()
            .map(|key| key.to_string())
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SetFailpoint {
    None,
    AfterInputRetain,
    AfterEntryRetains,
    BeforePublish,
}

struct StagedOwnerTokens {
    owners: Arc<AnyOwnerTable>,
    handles: Vec<AnyHandle>,
    committed: bool,
}

impl StagedOwnerTokens {
    fn new(owners: Arc<AnyOwnerTable>) -> Self {
        Self {
            owners,
            handles: Vec::new(),
            committed: false,
        }
    }

    fn add(&mut self, handle: AnyHandle) {
        self.handles.push(handle);
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for StagedOwnerTokens {
    fn drop(&mut self) {
        if !self.committed {
            for handle in self.handles.drain(..).rev() {
                let _ = self.owners.release(handle);
            }
        }
    }
}

struct OwnedMapRoot {
    identity: MapRootId,
    owners: Arc<AnyOwnerTable>,
    entries: BTreeMap<Arc<str>, AnyHandle>,
    retire_observer: Option<MapRootRetireObserver>,
}

impl Drop for OwnedMapRoot {
    fn drop(&mut self) {
        self.owners.retire_map_root(self.identity);
        if let Some(observer) = &self.retire_observer {
            observer(self.identity);
        }
        for handle in self.entries.values() {
            let _ = self.owners.release(*handle);
        }
    }
}

#[derive(Clone)]
struct OwnedMapKeysSnapshot {
    root: Arc<OwnedMapRoot>,
    keys: Vec<Arc<str>>,
}

impl OwnedMapKeysSnapshot {
    fn identity(&self) -> MapRootId {
        self.root.identity
    }

    fn keys(&self) -> Vec<String> {
        self.keys.iter().map(|key| key.to_string()).collect()
    }

    fn get_owned(&self, key: &str) -> Result<Option<AnyHandle>, OwnerError> {
        let root = self.root.owners.resolve_map_root(self.root.identity)?;
        if !Arc::ptr_eq(&root, &self.root) {
            return Err(OwnerError::StaleRoot);
        }
        root.entries
            .get(key)
            .map(|handle| root.owners.retain(*handle))
            .transpose()
    }
}

/// Persistent Map model whose root owns one independent typed token per entry.
/// Cloning a map shares the root; publishing a changed root retains new tokens
/// so the old root and its snapshots stay independently valid.
#[derive(Clone)]
struct OwnedPrototypeMap {
    root: Arc<OwnedMapRoot>,
}

impl OwnedPrototypeMap {
    fn empty(owners: Arc<AnyOwnerTable>) -> Self {
        Self {
            root: owners
                .create_map_root(BTreeMap::new())
                .expect("fresh owner table can register its empty Map root"),
        }
    }

    /// Test fixture constructor that transfers the supplied owner tokens into
    /// the new root. It validates the whole set before transfer, so failure
    /// leaves every token with the caller.
    fn from_owned_tokens(
        owners: Arc<AnyOwnerTable>,
        entries: BTreeMap<Arc<str>, AnyHandle>,
    ) -> Result<Self, OwnerError> {
        let mut transferred = std::collections::BTreeSet::new();
        for handle in entries.values() {
            if !transferred.insert((handle.table_id, handle.slot, handle.generation)) {
                return Err(OwnerError::DuplicateOwnerToken);
            }
            owners.validate_handle(*handle)?;
        }
        let root = owners.create_map_root(entries)?;
        Ok(Self { root })
    }

    fn root_identity(&self) -> MapRootId {
        self.root.identity
    }

    fn resolve_root(&self, identity: MapRootId) -> Result<Arc<OwnedMapRoot>, OwnerError> {
        let root = self.root.owners.resolve_map_root(identity)?;
        if !Arc::ptr_eq(&root, &self.root) {
            return Err(OwnerError::StaleRoot);
        }
        Ok(root)
    }

    fn keys_snapshot(&self) -> Result<OwnedMapKeysSnapshot, OwnerError> {
        let root = self.resolve_root(self.root_identity())?;
        Ok(OwnedMapKeysSnapshot {
            keys: root.entries.keys().cloned().collect(),
            root,
        })
    }

    fn set_transactional(
        &self,
        key: &str,
        input: AnyHandle,
        expected: DescriptorId,
        failpoint: SetFailpoint,
    ) -> Result<Self, OwnerError> {
        let root = self.resolve_root(self.root_identity())?;
        let incoming = root.owners.clone_value(input)?;
        if input.descriptor != expected || incoming.descriptor.identity != expected {
            return Err(OwnerError::DescriptorMismatch);
        }

        let mut staged = StagedOwnerTokens::new(root.owners.clone());
        let input_owner = root.owners.retain(input)?;
        staged.add(input_owner);
        if failpoint == SetFailpoint::AfterInputRetain {
            return Err(OwnerError::InjectedCommitFailure);
        }

        let mut entries = BTreeMap::new();
        for (old_key, old_handle) in &root.entries {
            if old_key.as_ref() == key {
                continue;
            }
            let retained = root.owners.retain(*old_handle)?;
            staged.add(retained);
            entries.insert(old_key.clone(), retained);
        }
        if failpoint == SetFailpoint::AfterEntryRetains {
            return Err(OwnerError::InjectedCommitFailure);
        }

        entries.insert(Arc::from(key), input_owner);
        if failpoint == SetFailpoint::BeforePublish {
            return Err(OwnerError::InjectedCommitFailure);
        }

        let next_root = root.owners.create_map_root(entries)?;
        staged.commit();
        Ok(Self { root: next_root })
    }

    fn get_owned(&self, key: &str) -> Result<Option<AnyHandle>, OwnerError> {
        let root = self.resolve_root(self.root_identity())?;
        root.entries
            .get(key)
            .map(|handle| root.owners.retain(*handle))
            .transpose()
    }

    fn values_owned(&self) -> Result<Vec<AnyHandle>, OwnerError> {
        let root = self.resolve_root(self.root_identity())?;
        root.owners
            .retain_many(&root.entries.values().copied().collect::<Vec<_>>())
    }

    fn remove_snapshot(&self, key: &str) -> Result<Self, OwnerError> {
        let root = self.resolve_root(self.root_identity())?;
        let retained = root
            .entries
            .iter()
            .filter(|(old_key, _)| old_key.as_ref() != key)
            .map(|(old_key, handle)| (old_key.clone(), *handle))
            .collect::<Vec<_>>();
        let new_handles = root.owners.retain_many(
            &retained
                .iter()
                .map(|(_, handle)| *handle)
                .collect::<Vec<_>>(),
        )?;
        let entries: BTreeMap<Arc<str>, AnyHandle> = retained
            .into_iter()
            .zip(new_handles)
            .map(|((key, _), handle)| (key, handle))
            .collect();
        let mut staged = StagedOwnerTokens::new(root.owners.clone());
        for handle in entries.values() {
            staged.add(*handle);
        }
        let next_root = root.owners.create_map_root(entries)?;
        staged.commit();
        Ok(Self { root: next_root })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnyHandle, AnyOwnerTable, AnyStringOwner, AnyValue, DescriptorId, DropProbe, MapRootId,
        OwnedAggregate, OwnedPrototypeMap, OwnerError, PrototypeMap, SetFailpoint, ValueShape,
    };
    use std::collections::BTreeMap;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Arc;

    #[test]
    fn dynamic_kind_is_not_inferred_from_scalar_bits() {
        let raw_bits = 0x7fff_ffff_ffff_ff01_u64 as i64;
        let scalar = AnyValue::I64(raw_bits);
        let string = AnyValue::String(AnyStringOwner::new("owned", DropProbe::default()));

        assert_eq!(scalar.as_i64(), Some(raw_bits));
        assert_eq!(scalar.as_str(), None);
        assert_eq!(string.as_str(), Some("owned"));
        assert_eq!(string.as_i64(), None);
    }

    #[test]
    fn map_root_and_shared_payload_wait_for_last_snapshot_owner() {
        let string_drops = DropProbe::default();
        let root_drops = DropProbe::default();
        let mut entries = BTreeMap::new();
        entries.insert(
            Arc::from("entry"),
            AnyValue::String(AnyStringOwner::new("original", string_drops.clone())),
        );
        let base = PrototypeMap::from_entries(entries, Some(root_drops.clone()));
        let sibling = base.clone();
        let overwritten = base.insert("entry", AnyValue::I64(42));
        let removed = base.remove("entry");

        assert_eq!(
            base.get("entry").as_ref().and_then(AnyValue::as_str),
            Some("original")
        );
        assert_eq!(
            sibling.get("entry").as_ref().and_then(AnyValue::as_str),
            Some("original")
        );
        assert_eq!(
            overwritten.get("entry").and_then(|value| value.as_i64()),
            Some(42)
        );
        assert!(removed.get("entry").is_none());

        drop(base);
        drop(overwritten);
        drop(removed);
        assert_eq!(root_drops.count(), 0);
        assert_eq!(string_drops.count(), 0);
        assert_eq!(
            sibling.get("entry").as_ref().and_then(AnyValue::as_str),
            Some("original")
        );

        drop(sibling);
        assert_eq!(root_drops.count(), 1);
        assert_eq!(string_drops.count(), 1);
    }

    #[test]
    fn get_and_values_snapshot_outlive_the_source_map() {
        let string_drops = DropProbe::default();
        let map = PrototypeMap::empty().insert(
            "entry",
            AnyValue::String(AnyStringOwner::new("snapshot", string_drops.clone())),
        );
        let got = map.get("entry").expect("entry exists");
        let values = map.values_snapshot();

        drop(map);
        assert_eq!(got.as_str(), Some("snapshot"));
        assert_eq!(values[0].as_str(), Some("snapshot"));
        drop(got);
        assert_eq!(string_drops.count(), 0);
        drop(values);
        assert_eq!(string_drops.count(), 1);
    }

    #[test]
    fn aggregate_snapshot_recursively_owns_string_payload() {
        let string_drops = DropProbe::default();
        let aggregate_drops = DropProbe::default();
        let aggregate = AnyValue::Aggregate(Arc::new(OwnedAggregate {
            fields: vec![AnyValue::String(AnyStringOwner::new(
                "nested",
                string_drops.clone(),
            ))],
            drop_probe: aggregate_drops.clone(),
        }));
        let map = PrototypeMap::empty().insert("record", aggregate);
        let values = map.values_snapshot();

        drop(map);
        assert_eq!(values[0].aggregate_str_field(0), Some("nested"));
        assert_eq!(aggregate_drops.count(), 0);
        assert_eq!(string_drops.count(), 0);

        drop(values);
        assert_eq!(aggregate_drops.count(), 1);
        assert_eq!(string_drops.count(), 1);
    }

    #[test]
    fn snapshot_key_order_is_independent_of_insertion_order() {
        let forward = PrototypeMap::empty()
            .insert("b", AnyValue::I64(2))
            .insert("a", AnyValue::I64(1));
        let reverse = PrototypeMap::empty()
            .insert("a", AnyValue::I64(1))
            .insert("b", AnyValue::I64(2));

        assert_eq!(forward.keys(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(forward.keys(), reverse.keys());
    }

    #[test]
    fn root_probe_fires_after_the_last_clone() {
        let root_drops = DropProbe::default();
        let map = PrototypeMap::from_entries(BTreeMap::new(), Some(root_drops.clone()));
        let clone = map.clone();

        drop(map);
        assert_eq!(root_drops.count(), 0);
        drop(clone);
        assert_eq!(root_drops.count(), 1);
    }

    #[test]
    fn owner_tokens_reject_invalid_stale_and_repeated_release_handles() {
        let owners = AnyOwnerTable::with_capacity(1);
        let first = owners
            .allocate(DescriptorId::I64, AnyValue::I64(7))
            .expect("first token fits");
        let invalid = AnyHandle {
            table_id: owners.table_id,
            slot: u32::MAX,
            generation: 1,
            descriptor: DescriptorId::I64,
        };
        assert_eq!(
            owners.retain_many(&[first, invalid]),
            Err(OwnerError::InvalidHandle)
        );
        assert_eq!(owners.live_count(), Ok(1));
        assert!(matches!(
            owners.clone_value(invalid),
            Err(OwnerError::InvalidHandle)
        ));
        assert_eq!(owners.release(invalid), Err(OwnerError::InvalidHandle));

        owners.release(first).expect("first release succeeds");
        assert_eq!(owners.release(first), Err(OwnerError::StaleHandle));
        assert_eq!(
            owners.read_i64(first, DescriptorId::I64),
            Err(OwnerError::StaleHandle)
        );

        let reused = owners
            .allocate(DescriptorId::I64, AnyValue::I64(9))
            .expect("released slot is reused with a new generation");
        assert_eq!(reused.slot, first.slot);
        assert_eq!(reused.generation, first.generation + 1);
        assert_eq!(
            owners.read_i64(first, DescriptorId::I64),
            Err(OwnerError::StaleHandle)
        );
        assert_eq!(owners.release(first), Err(OwnerError::StaleHandle));
        assert_eq!(owners.read_i64(reused, DescriptorId::I64), Ok(9));
    }

    #[test]
    fn owner_tokens_are_scoped_to_their_table_namespace() {
        let first_table = AnyOwnerTable::with_capacity(2);
        let second_table = AnyOwnerTable::with_capacity(2);
        let first = first_table
            .allocate(DescriptorId::I64, AnyValue::I64(7))
            .expect("first table token fits");
        let second = second_table
            .allocate(DescriptorId::I64, AnyValue::I64(9))
            .expect("second table token fits at the same slot coordinates");

        assert_eq!(first.slot, second.slot);
        assert_eq!(first.generation, second.generation);
        assert_ne!(first.table_id, second.table_id);
        assert_eq!(
            second_table.read_i64(first, DescriptorId::I64),
            Err(OwnerError::WrongTable)
        );
        assert_eq!(second_table.release(first), Err(OwnerError::WrongTable));
        assert_eq!(first_table.read_i64(first, DescriptorId::I64), Ok(7));
        assert_eq!(second_table.read_i64(second, DescriptorId::I64), Ok(9));

        first_table
            .release(first)
            .expect("release first table token");
        second_table
            .release(second)
            .expect("release second table token");
        assert_eq!(first_table.live_count(), Ok(0));
        assert_eq!(second_table.live_count(), Ok(0));
    }

    #[test]
    fn map_root_identity_is_versioned_scoped_and_key_snapshot_bound() {
        let string_probe = DropProbe::default();
        let owners = Arc::new(AnyOwnerTable::with_capacity(12));
        let other_owners = Arc::new(AnyOwnerTable::with_capacity(2));
        let empty = OwnedPrototypeMap::empty(owners.clone());
        let empty_id = empty.root_identity();
        let other_empty = OwnedPrototypeMap::empty(other_owners.clone());
        let other_id = other_empty.root_identity();
        assert_eq!(empty_id.slot, other_id.slot);
        assert_eq!(empty_id.generation, other_id.generation);
        assert_ne!(empty_id.table_id, other_id.table_id);
        assert_eq!(
            owners.resolve_map_root(other_id).map(|_| ()),
            Err(OwnerError::WrongTable)
        );

        let zero_seed = owners
            .allocate(DescriptorId::I64, AnyValue::I64(0))
            .expect("zero seed fits");
        let map_a = empty
            .set_transactional("k", zero_seed, DescriptorId::I64, SetFailpoint::None)
            .expect("Map A contains typed zero");
        owners.release(zero_seed).expect("release zero seed");
        let id_a = map_a.root_identity();
        assert_ne!(id_a, empty_id);
        let clone_a = map_a.clone();
        assert_eq!(clone_a.root_identity(), id_a);
        let keys_a = map_a.keys_snapshot().expect("snapshot captures root A");
        assert_eq!(keys_a.identity(), id_a);
        assert_eq!(keys_a.keys(), vec!["k".to_string()]);
        let get_a = keys_a
            .get_owned("k")
            .expect("snapshot lookup uses root A")
            .expect("root A contains k");
        let values_a = map_a.values_owned().expect("retain root A values");
        assert_eq!(values_a.len(), 1);

        let string_seed = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("new", string_probe.clone())),
            )
            .expect("replacement seed fits");
        let map_b = map_a
            .set_transactional("k", string_seed, DescriptorId::STRING, SetFailpoint::None)
            .expect("overwrite publishes root B");
        owners.release(string_seed).expect("release string seed");
        let id_b = map_b.root_identity();
        assert_eq!(
            map_a.resolve_root(id_b).map(|_| ()),
            Err(OwnerError::StaleRoot),
            "a valid root from the same owner table cannot be substituted for this Map"
        );
        let keys_b = map_b.keys_snapshot().expect("snapshot captures root B");
        let get_b = keys_b
            .get_owned("k")
            .expect("snapshot lookup uses root B")
            .expect("root B contains k");
        let map_c = map_b.remove_snapshot("k").expect("remove publishes root C");
        let id_c = map_c.root_identity();
        assert_ne!(id_a, id_b);
        assert_ne!(id_b, id_c);
        assert_eq!(
            owners.read_i64(get_a, DescriptorId::I64),
            Ok(0),
            "root A key snapshot remains tied to its old entry"
        );
        assert_eq!(
            owners.read_string(get_b, DescriptorId::STRING),
            Ok("new".into())
        );
        assert_eq!(map_c.get_owned("k"), Ok(None));
        assert!(owners.resolve_map_root(id_a).is_ok());
        assert!(owners.resolve_map_root(id_b).is_ok());
        assert!(owners.resolve_map_root(id_c).is_ok());
        assert_eq!(
            owners
                .resolve_map_root(MapRootId {
                    generation: id_a.generation + 1,
                    ..id_a
                })
                .map(|_| ()),
            Err(OwnerError::StaleRoot)
        );
        assert_eq!(
            owners
                .resolve_map_root(MapRootId {
                    slot: u32::MAX,
                    ..id_a
                })
                .map(|_| ()),
            Err(OwnerError::InvalidRoot)
        );

        drop(empty);
        drop(map_a);
        drop(clone_a);
        assert!(owners.resolve_map_root(id_a).is_ok());
        assert_eq!(
            owners.read_i64(values_a[0], DescriptorId::I64),
            Ok(0),
            "independent value tokens outlive root A"
        );
        drop(keys_a);
        assert_eq!(
            owners.resolve_map_root(id_a).map(|_| ()),
            Err(OwnerError::StaleRoot)
        );
        let map_d = OwnedPrototypeMap::empty(owners.clone());
        let id_d = map_d.root_identity();
        assert_eq!(id_d.slot, id_a.slot);
        assert_eq!(id_d.generation, id_a.generation + 1);
        assert_eq!(
            owners.resolve_map_root(id_a).map(|_| ()),
            Err(OwnerError::StaleRoot),
            "slot reuse cannot revive the old root id"
        );
        assert_eq!(owners.live_map_root_count(), Ok(3));

        owners.release(get_a).expect("release root A get snapshot");
        owners
            .release(values_a[0])
            .expect("release root A values snapshot");
        drop(map_b);
        drop(keys_b);
        drop(map_c);
        drop(map_d);
        drop(other_empty);
        owners.release(get_b).expect("release root B get snapshot");
        assert_eq!(string_probe.count(), 1);
        assert_eq!(owners.live_count(), Ok(0));
        assert_eq!(owners.live_map_root_count(), Ok(0));
    }

    #[test]
    fn map_root_generation_exhaustion_retires_without_wraparound() {
        let owners = Arc::new(AnyOwnerTable::with_capacity(1));
        {
            let mut roots = owners.map_roots.lock().expect("root registry is healthy");
            roots.max_slots = 1;
            roots.slots.push(super::MapRootSlot {
                generation: u32::MAX,
                root: None,
                retired: false,
            });
            roots.free.push(0);
        }
        let root = owners
            .create_map_root(BTreeMap::new())
            .expect("maximum-generation root registers");
        let identity = root.identity;
        assert!(owners.resolve_map_root(identity).is_ok());
        drop(root);
        assert_eq!(
            owners.resolve_map_root(identity).map(|_| ()),
            Err(OwnerError::StaleRoot)
        );
        assert_eq!(
            owners.create_map_root(BTreeMap::new()).map(|_| ()),
            Err(OwnerError::Capacity)
        );
    }

    #[test]
    fn map_root_retirement_invalidates_identity_before_releasing_entries() {
        let owners = Arc::new(AnyOwnerTable::with_capacity(2));
        let probe = DropProbe::default();
        let entry = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("held by root", probe.clone())),
            )
            .expect("entry token fits");
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observer_owners = owners.clone();
        let observer_results = observed.clone();
        let root = owners
            .create_map_root_with_retire_observer_for_test(
                BTreeMap::from([(Arc::from("key"), entry)]),
                Arc::new(move |identity| {
                    let stale = observer_owners.resolve_map_root(identity).map(|_| ());
                    let live_entries = observer_owners.live_count();
                    observer_results
                        .lock()
                        .expect("observer results mutex is healthy")
                        .push((stale, live_entries));
                }),
            )
            .expect("root registers with test observer");
        let identity = root.identity;
        assert_eq!(owners.live_count(), Ok(1));
        drop(root);

        assert_eq!(
            *observed.lock().expect("observer results mutex is healthy"),
            vec![(Err(OwnerError::StaleRoot), Ok(1))],
            "root identity is invalid before its entry owner is released"
        );
        assert_eq!(
            owners.resolve_map_root(identity).map(|_| ()),
            Err(OwnerError::StaleRoot)
        );
        assert_eq!(owners.live_count(), Ok(0));
        assert_eq!(probe.count(), 1);
    }

    #[test]
    fn map_root_corrupt_free_list_entries_fail_closed_without_reusing_live_roots() {
        let owners = Arc::new(AnyOwnerTable::with_capacity(1));
        owners
            .map_roots
            .lock()
            .expect("root registry is healthy")
            .max_slots = 1;
        let root = OwnedPrototypeMap::empty(owners.clone());
        let identity = root.root_identity();

        {
            let mut roots = owners.map_roots.lock().expect("root registry is healthy");
            roots.free.push(identity.slot);
        }
        assert_eq!(
            owners.create_map_root(BTreeMap::new()).map(|_| ()),
            Err(OwnerError::Capacity),
            "an occupied slot in the free list must fail closed"
        );
        assert!(owners.resolve_map_root(identity).is_ok());
        {
            let roots = owners.map_roots.lock().expect("root registry is healthy");
            assert!(roots.slots[identity.slot as usize].retired);
            assert!(roots.free.is_empty());
        }
        drop(root);
        assert_eq!(
            owners.resolve_map_root(identity).map(|_| ()),
            Err(OwnerError::StaleRoot)
        );
        assert_eq!(
            owners.create_map_root(BTreeMap::new()).map(|_| ()),
            Err(OwnerError::Capacity),
            "a corrupt slot stays retired after the original root dies"
        );

        let other = Arc::new(AnyOwnerTable::with_capacity(2));
        other
            .map_roots
            .lock()
            .expect("second root registry is healthy")
            .free
            .push(u32::MAX);
        assert_eq!(
            other.create_map_root(BTreeMap::new()).map(|_| ()),
            Err(OwnerError::Capacity),
            "an out-of-range free-list index must fail closed rather than panic"
        );
        assert_eq!(
            other
                .create_map_root(BTreeMap::new())
                .map(|root| root.identity.slot),
            Ok(0)
        );
    }

    #[test]
    fn map_root_capacity_failures_roll_back_set_and_remove_staging() {
        let owners = Arc::new(AnyOwnerTable::with_capacity(8));
        owners
            .map_roots
            .lock()
            .expect("root registry is healthy")
            .max_slots = 2;
        let empty = OwnedPrototypeMap::empty(owners.clone());
        let first_seed = owners
            .allocate(DescriptorId::I64, AnyValue::I64(1))
            .expect("first value fits");
        let first = empty
            .set_transactional("a", first_seed, DescriptorId::I64, SetFailpoint::None)
            .expect("first root fits");
        drop(empty);
        owners.release(first_seed).expect("release first seed");

        let second_seed = owners
            .allocate(DescriptorId::I64, AnyValue::I64(2))
            .expect("second value fits");
        let base = first
            .set_transactional("b", second_seed, DescriptorId::I64, SetFailpoint::None)
            .expect("second root reuses retired empty-root slot");
        owners.release(second_seed).expect("release second seed");
        assert_eq!(owners.live_map_root_count(), Ok(2));

        let input = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("staged", DropProbe::default())),
            )
            .expect("replacement value fits");
        let identity = base.root_identity();
        let owner_count = owners.live_count().expect("owner table is healthy");
        assert_eq!(
            base.set_transactional("a", input, DescriptorId::STRING, SetFailpoint::None)
                .map(|_| ()),
            Err(OwnerError::Capacity)
        );
        assert_eq!(
            base.remove_snapshot("a").map(|_| ()),
            Err(OwnerError::Capacity)
        );
        assert_eq!(owners.live_count(), Ok(owner_count));
        assert_eq!(owners.live_map_root_count(), Ok(2));
        assert!(owners.resolve_map_root(identity).is_ok());
        assert_eq!(
            owners.read_i64(base.root.entries["a"], DescriptorId::I64),
            Ok(1)
        );
        assert_eq!(
            owners.read_i64(base.root.entries["b"], DescriptorId::I64),
            Ok(2)
        );

        owners
            .release(input)
            .expect("release external replacement token");
        drop(base);
        drop(first);
        assert_eq!(owners.live_count(), Ok(0));
        assert_eq!(owners.live_map_root_count(), Ok(0));
    }

    #[test]
    fn owned_map_rejects_foreign_table_tokens_without_consuming_them() {
        let source_owners = Arc::new(AnyOwnerTable::with_capacity(2));
        let destination_owners = Arc::new(AnyOwnerTable::with_capacity(2));
        let source_token = source_owners
            .allocate(DescriptorId::I64, AnyValue::I64(17))
            .expect("source token fits");

        assert_eq!(
            OwnedPrototypeMap::from_owned_tokens(
                destination_owners.clone(),
                BTreeMap::from([(Arc::from("foreign"), source_token)]),
            )
            .map(|_| ()),
            Err(OwnerError::WrongTable)
        );
        assert_eq!(
            OwnedPrototypeMap::from_owned_tokens(
                source_owners.clone(),
                BTreeMap::from([
                    (Arc::from("first"), source_token),
                    (Arc::from("duplicate"), source_token),
                ]),
            )
            .map(|_| ()),
            Err(OwnerError::DuplicateOwnerToken)
        );
        assert_eq!(source_owners.live_count(), Ok(1));
        assert_eq!(destination_owners.live_count(), Ok(0));
        assert_eq!(
            source_owners.read_i64(source_token, DescriptorId::I64),
            Ok(17)
        );
        source_owners
            .release(source_token)
            .expect("failed construction leaves source token with caller");
    }

    #[test]
    fn descriptor_replacement_keeps_old_owners_pinned_and_rejects_stale_ingress() {
        let string_probe = DropProbe::default();
        let aggregate_probe = DropProbe::default();
        let owners = AnyOwnerTable::with_capacity(4);
        let old_descriptor = DescriptorId::STRING;
        let old_owner = owners
            .allocate(
                old_descriptor,
                AnyValue::String(AnyStringOwner::new("old layout", string_probe.clone())),
            )
            .expect("old descriptor accepts a String owner");
        let old_descriptor_drop = owners
            .descriptor_drop_probe_for_test_only(old_owner)
            .expect("live owner exposes its pinned descriptor lifetime probe");
        let old_drop_glue_probe = owners
            .descriptor_value_drop_probe_for_test_only(old_owner)
            .expect("live owner exposes its pinned value drop glue probe");

        let new_descriptor = owners
            .replace_descriptor_for_test_only(old_descriptor, ValueShape::Aggregate)
            .expect("descriptor update publishes a new immutable generation");
        assert_eq!(new_descriptor.slot, old_descriptor.slot);
        assert_eq!(new_descriptor.generation, old_descriptor.generation + 1);
        assert_eq!(old_descriptor_drop.count(), 0);
        assert_eq!(
            owners.read_string(old_owner, old_descriptor),
            Ok("old layout".into())
        );
        let retained_old_owner = owners
            .retain(old_owner)
            .expect("retaining an extant retired version pins its descriptor");

        let rejected_probe = DropProbe::default();
        assert_eq!(
            owners.allocate(
                old_descriptor,
                AnyValue::String(AnyStringOwner::new("stale ingress", rejected_probe.clone())),
            ),
            Err(OwnerError::StaleDescriptor)
        );
        assert_eq!(rejected_probe.count(), 1);

        let forged_new_version = AnyHandle {
            descriptor: new_descriptor,
            ..old_owner
        };
        assert_eq!(
            owners.clone_value(forged_new_version).map(|_| ()),
            Err(OwnerError::DescriptorMismatch)
        );
        assert_eq!(
            owners.release(forged_new_version),
            Err(OwnerError::DescriptorMismatch)
        );
        assert_eq!(
            owners.read_string(old_owner, old_descriptor),
            Ok("old layout".into())
        );

        let aggregate = owners
            .allocate(
                new_descriptor,
                AnyValue::Aggregate(Arc::new(OwnedAggregate {
                    fields: vec![AnyValue::String(AnyStringOwner::new(
                        "new layout",
                        aggregate_probe.clone(),
                    ))],
                    drop_probe: aggregate_probe.clone(),
                })),
            )
            .expect("new descriptor accepts only its new aggregate shape");
        let new_descriptor_drop = owners
            .descriptor_drop_probe_for_test_only(aggregate)
            .expect("new owner exposes its descriptor lifetime probe");
        let new_drop_glue_probe = owners
            .descriptor_value_drop_probe_for_test_only(aggregate)
            .expect("new owner exposes its descriptor-specific drop glue probe");
        let cloned_aggregate = owners
            .clone_value(aggregate)
            .expect("new aggregate owner validates");
        assert_eq!(cloned_aggregate.descriptor.identity, new_descriptor);
        assert_eq!(cloned_aggregate.descriptor.shape, ValueShape::Aggregate);
        drop(cloned_aggregate);

        owners
            .release(old_owner)
            .expect("retired descriptor still releases its old layout");
        assert_eq!(string_probe.count(), 0);
        assert_eq!(old_descriptor_drop.count(), 0);
        assert_eq!(old_drop_glue_probe.count(), 1);
        owners
            .release(retained_old_owner)
            .expect("last old-version owner runs its pinned destructor");
        assert_eq!(string_probe.count(), 1);
        assert_eq!(old_descriptor_drop.count(), 1);
        assert_eq!(old_drop_glue_probe.count(), 2);
        owners
            .release(aggregate)
            .expect("release new descriptor owner");
        assert_eq!(aggregate_probe.count(), 2);
        assert_eq!(new_drop_glue_probe.count(), 1);
        assert_eq!(owners.live_count(), Ok(0));
        assert_eq!(new_descriptor_drop.count(), 0);
        drop(owners);
        assert_eq!(new_descriptor_drop.count(), 1);
    }

    #[test]
    fn descriptor_generation_exhaustion_keeps_current_version_usable() {
        let owners = AnyOwnerTable::with_capacity(2);
        let exhausted = DescriptorId::new(DescriptorId::STRING.slot, u32::MAX);
        let retired = {
            let mut state = owners.state.lock().expect("owner table lock is healthy");
            let current = state
                .current_descriptors
                .get_mut(&DescriptorId::STRING.slot)
                .expect("String descriptor is registered");
            std::mem::replace(
                current,
                super::descriptor_version(exhausted, ValueShape::String),
            )
        };
        drop(retired);

        assert_eq!(
            owners.replace_descriptor_for_test_only(exhausted, ValueShape::Aggregate),
            Err(OwnerError::Capacity)
        );
        let current = owners
            .allocate(
                exhausted,
                AnyValue::String(AnyStringOwner::new("still current", DropProbe::default())),
            )
            .expect("failed replacement leaves the maximum generation current");
        assert_eq!(
            owners.read_string(current, exhausted),
            Ok("still current".into())
        );
        owners
            .release(current)
            .expect("release maximum generation owner");
        assert_eq!(owners.live_count(), Ok(0));
        assert_eq!(
            owners.allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("stale", DropProbe::default())),
            ),
            Err(OwnerError::StaleDescriptor)
        );
    }

    #[test]
    fn retained_owner_token_is_distinct_and_duplicate_drop_is_rejected() {
        let probe = DropProbe::default();
        let owners = AnyOwnerTable::with_capacity(2);
        let first = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("shared", probe.clone())),
            )
            .expect("first owner token fits");
        let second = owners.retain(first).expect("retain creates a new token");

        assert_ne!(first, second);
        owners.release(first).expect("release first token");
        assert_eq!(owners.release(first), Err(OwnerError::StaleHandle));
        assert_eq!(
            owners.read_string(first, DescriptorId::STRING),
            Err(OwnerError::StaleHandle)
        );
        assert_eq!(
            owners.read_string(second, DescriptorId::STRING),
            Ok("shared".into())
        );
        assert_eq!(probe.count(), 0);

        owners.release(second).expect("release last token");
        assert_eq!(probe.count(), 1);
        assert_eq!(owners.live_count(), Ok(0));
    }

    #[test]
    fn forged_or_mismatched_descriptors_fail_without_consuming_the_real_token() {
        let probe = DropProbe::default();
        let owners = AnyOwnerTable::with_capacity(1);
        let string = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("typed", probe.clone())),
            )
            .expect("string owner token fits");
        let forged_known_descriptor = AnyHandle {
            descriptor: DescriptorId::I64,
            ..string
        };
        let forged_unknown_descriptor = AnyHandle {
            descriptor: DescriptorId::new(99, 1),
            ..string
        };

        assert_eq!(
            owners.read_i64(forged_known_descriptor, DescriptorId::I64),
            Err(OwnerError::DescriptorMismatch)
        );
        assert_eq!(
            owners.release(forged_known_descriptor),
            Err(OwnerError::DescriptorMismatch)
        );
        assert!(matches!(
            owners.clone_value(forged_unknown_descriptor),
            Err(OwnerError::UnknownDescriptor)
        ));
        assert_eq!(
            owners.read_string(string, DescriptorId::I64),
            Err(OwnerError::DescriptorMismatch)
        );
        assert_eq!(
            owners.read_string(string, DescriptorId::STRING),
            Ok("typed".into())
        );
        assert_eq!(owners.live_count(), Ok(1));
        assert_eq!(probe.count(), 0);

        owners
            .release(string)
            .expect("original token remains valid");
        assert_eq!(probe.count(), 1);
    }

    #[test]
    fn owner_generation_exhaustion_retires_slot_without_wraparound() {
        let owners = AnyOwnerTable::with_capacity(1);
        let original = owners
            .allocate(DescriptorId::I64, AnyValue::I64(23))
            .expect("owner token fits");
        let last_generation = owners
            .force_generation_for_test(original, u32::MAX)
            .expect("test can place token at final generation");

        assert_eq!(
            owners.read_i64(original, DescriptorId::I64),
            Err(OwnerError::StaleHandle)
        );
        assert_eq!(owners.read_i64(last_generation, DescriptorId::I64), Ok(23));
        owners
            .release(last_generation)
            .expect("final generation can be released");
        assert_eq!(
            owners.read_i64(last_generation, DescriptorId::I64),
            Err(OwnerError::StaleHandle)
        );
        assert_eq!(
            owners.allocate(DescriptorId::I64, AnyValue::I64(24)),
            Err(OwnerError::Capacity)
        );
    }

    #[test]
    fn owner_table_poison_fails_closed_and_table_drop_reclaims_payload() {
        let probe = DropProbe::default();
        let owners = AnyOwnerTable::with_capacity(1);
        let handle = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("poisoned", probe.clone())),
            )
            .expect("owner token fits");
        let drop_glue_probe = owners
            .descriptor_value_drop_probe_for_test_only(handle)
            .expect("live owner exposes descriptor drop glue probe");
        let descriptor_lifetime_probe = owners
            .descriptor_drop_probe_for_test_only(handle)
            .expect("live owner exposes descriptor lifetime probe");

        assert!(catch_unwind(AssertUnwindSafe(|| owners.poison_for_test())).is_err());
        assert!(matches!(
            owners.clone_value(handle),
            Err(OwnerError::Poisoned)
        ));
        assert_eq!(owners.retain(handle), Err(OwnerError::Poisoned));
        assert_eq!(owners.release(handle), Err(OwnerError::Poisoned));
        assert_eq!(owners.live_count(), Err(OwnerError::Poisoned));
        assert_eq!(probe.count(), 0);

        drop(owners);
        assert_eq!(probe.count(), 1);
        assert_eq!(drop_glue_probe.count(), 1);
        assert_eq!(descriptor_lifetime_probe.count(), 1);
    }

    #[test]
    fn owned_map_clones_share_root_and_get_values_own_independent_tokens() {
        let probe = DropProbe::default();
        let owners = Arc::new(AnyOwnerTable::with_capacity(4));
        let seed = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("owned", probe.clone())),
            )
            .expect("seed token fits");
        let map = OwnedPrototypeMap::empty(owners.clone())
            .set_transactional("k", seed, DescriptorId::STRING, SetFailpoint::None)
            .expect("map root retains seed value");
        owners.release(seed).expect("caller releases seed token");
        let sibling = map.clone();
        let got = map.get_owned("k").expect("get works").expect("key exists");
        let values = map.values_owned().expect("values snapshot owns each entry");
        assert_eq!(values.len(), 1);
        assert_eq!(owners.live_count(), Ok(3));

        drop(map);
        assert_eq!(owners.live_count(), Ok(3));
        drop(sibling);
        assert_eq!(owners.live_count(), Ok(2));
        assert_eq!(
            owners.read_string(got, DescriptorId::STRING),
            Ok("owned".into())
        );
        assert_eq!(
            owners.read_string(values[0], DescriptorId::STRING),
            Ok("owned".into())
        );
        owners.release(got).expect("release get result");
        assert_eq!(probe.count(), 0);
        owners.release(values[0]).expect("release values result");
        assert_eq!(probe.count(), 1);
        assert_eq!(owners.live_count(), Ok(0));
    }

    #[test]
    fn owned_map_miss_is_absence_and_present_zero_stays_distinguishable() {
        let owners = Arc::new(AnyOwnerTable::with_capacity(2));
        let seed = owners
            .allocate(DescriptorId::I64, AnyValue::I64(0))
            .expect("zero-valued owner fits");
        let empty = OwnedPrototypeMap::empty(owners.clone());
        let map = empty
            .set_transactional("zero", seed, DescriptorId::I64, SetFailpoint::None)
            .expect("insert zero-valued owner");
        drop(empty);
        owners.release(seed).expect("caller releases seed token");

        assert_eq!(owners.live_count(), Ok(1));
        assert_eq!(map.get_owned("missing"), Ok(None));
        assert_eq!(owners.live_count(), Ok(1));

        let present = map
            .get_owned("zero")
            .expect("present lookup succeeds")
            .expect("zero-valued entry is present");
        assert_eq!(owners.read_i64(present, DescriptorId::I64), Ok(0));
        owners.release(present).expect("release retained lookup");
        drop(map);
        assert_eq!(owners.live_count(), Ok(0));
    }

    #[test]
    fn owned_map_update_remove_state_sequence_preserves_typed_snapshots_and_releases_once() {
        let probe = DropProbe::default();
        let owners = Arc::new(AnyOwnerTable::with_capacity(16));
        let empty = OwnedPrototypeMap::empty(owners.clone());
        assert_eq!(empty.get_owned("key"), Ok(None));
        assert_eq!(owners.live_count(), Ok(0));

        let zero_seed = owners
            .allocate(DescriptorId::I64, AnyValue::I64(0))
            .expect("zero-valued seed fits");
        let zero_map = empty
            .set_transactional("key", zero_seed, DescriptorId::I64, SetFailpoint::None)
            .expect("absent key accepts a typed zero value");
        drop(empty);
        owners
            .release(zero_seed)
            .expect("caller releases zero seed");

        let old_root = zero_map.clone();
        let zero_get = zero_map
            .get_owned("key")
            .expect("zero lookup succeeds")
            .expect("key is present");
        let zero_values = zero_map.values_owned().expect("zero snapshot succeeds");
        assert_eq!(zero_values.len(), 1);
        assert_eq!(owners.read_i64(zero_get, DescriptorId::I64), Ok(0));
        assert_eq!(owners.read_i64(zero_values[0], DescriptorId::I64), Ok(0));
        assert_eq!(
            owners.read_string(zero_get, DescriptorId::STRING),
            Err(OwnerError::DescriptorMismatch)
        );

        let string_seed = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("replacement", probe.clone())),
            )
            .expect("replacement string fits");
        let string_map = zero_map
            .set_transactional("key", string_seed, DescriptorId::STRING, SetFailpoint::None)
            .expect("overwrite publishes a sibling root");
        owners
            .release(string_seed)
            .expect("caller releases replacement seed");

        assert_eq!(zero_map.get_owned("missing"), Ok(None));
        assert_eq!(
            owners.read_i64(zero_map.root.entries["key"], DescriptorId::I64),
            Ok(0)
        );
        assert_eq!(
            owners.read_i64(old_root.root.entries["key"], DescriptorId::I64),
            Ok(0)
        );
        assert_eq!(
            owners.read_string(string_map.root.entries["key"], DescriptorId::STRING),
            Ok("replacement".into())
        );
        assert_eq!(probe.count(), 0);

        let string_get = string_map
            .get_owned("key")
            .expect("replacement lookup succeeds")
            .expect("replacement key is present");
        let string_values = string_map
            .values_owned()
            .expect("replacement values snapshot succeeds");
        assert_eq!(string_values.len(), 1);
        let removed = string_map
            .remove_snapshot("key")
            .expect("remove publishes an empty sibling root");
        assert_eq!(removed.get_owned("key"), Ok(None));
        assert_eq!(
            owners.read_string(string_get, DescriptorId::STRING),
            Ok("replacement".into())
        );
        assert_eq!(
            owners.read_string(string_values[0], DescriptorId::STRING),
            Ok("replacement".into())
        );
        assert_eq!(
            owners.read_i64(string_get, DescriptorId::I64),
            Err(OwnerError::DescriptorMismatch)
        );

        owners.release(zero_get).expect("release zero get token");
        assert_eq!(
            owners.read_i64(zero_get, DescriptorId::I64),
            Err(OwnerError::StaleHandle)
        );
        owners
            .release(string_get)
            .expect("release string get token");
        assert_eq!(
            owners.read_string(string_get, DescriptorId::STRING),
            Err(OwnerError::StaleHandle)
        );

        drop(zero_map);
        drop(old_root);
        assert_eq!(owners.read_i64(zero_values[0], DescriptorId::I64), Ok(0));
        drop(string_map);
        drop(removed);
        assert_eq!(
            owners.read_string(string_values[0], DescriptorId::STRING),
            Ok("replacement".into())
        );
        assert_eq!(probe.count(), 0);

        owners
            .release(zero_values[0])
            .expect("release zero values snapshot");
        owners
            .release(string_values[0])
            .expect("release replacement values snapshot");
        assert_eq!(owners.live_count(), Ok(0));
        assert_eq!(probe.count(), 1);
    }

    #[test]
    fn owned_map_duplicate_payload_tokens_survive_remove_snapshots_and_drop_once() {
        let probe = DropProbe::default();
        let owners = Arc::new(AnyOwnerTable::with_capacity(8));
        let seed = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("shared", probe.clone())),
            )
            .expect("string owner fits");
        let empty = OwnedPrototypeMap::empty(owners.clone());
        let base = empty
            .set_transactional("a", seed, DescriptorId::STRING, SetFailpoint::None)
            .expect("first entry owns a token");
        drop(empty);
        owners.release(seed).expect("caller releases seed token");

        let alias = base
            .get_owned("a")
            .expect("get succeeds")
            .expect("first entry exists");
        let updated = base
            .set_transactional("b", alias, DescriptorId::STRING, SetFailpoint::None)
            .expect("second entry retains the same payload independently");
        owners.release(alias).expect("caller releases get snapshot");
        assert_eq!(owners.live_count(), Ok(3));

        let removed = updated
            .remove_snapshot("a")
            .expect("remove publishes a sibling root retaining b");
        assert!(!removed.root.entries.contains_key("a"));
        assert_eq!(
            owners.read_string(removed.root.entries["b"], DescriptorId::STRING),
            Ok("shared".into())
        );
        assert_eq!(probe.count(), 0);

        drop(base);
        drop(updated);
        assert_eq!(owners.live_count(), Ok(1));
        assert_eq!(probe.count(), 0);
        assert_eq!(
            owners.read_string(removed.root.entries["b"], DescriptorId::STRING),
            Ok("shared".into())
        );

        drop(removed);
        assert_eq!(owners.live_count(), Ok(0));
        assert_eq!(probe.count(), 1);
    }

    #[test]
    fn owned_map_concurrent_snapshot_retain_read_release_and_drop_balance() {
        let probe = DropProbe::default();
        let owners = Arc::new(AnyOwnerTable::with_capacity(16));
        let seed = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("parallel", probe.clone())),
            )
            .expect("string owner fits");
        let empty = OwnedPrototypeMap::empty(owners.clone());
        let base = empty
            .set_transactional("key", seed, DescriptorId::STRING, SetFailpoint::None)
            .expect("map root retains seed");
        drop(empty);
        owners.release(seed).expect("caller releases seed token");
        let shared = Arc::new(base);

        let workers = (0..8)
            .map(|_| {
                let shared = shared.clone();
                let owners = owners.clone();
                std::thread::spawn(move || {
                    for _ in 0..64 {
                        let sibling = shared.as_ref().clone();
                        let got = sibling
                            .get_owned("key")
                            .expect("concurrent get succeeds")
                            .expect("key remains present");
                        assert_eq!(
                            owners.read_string(got, DescriptorId::STRING),
                            Ok("parallel".into())
                        );
                        owners.release(got).expect("release get snapshot");

                        let values = sibling.values_owned().expect("values snapshot succeeds");
                        assert_eq!(values.len(), 1);
                        assert_eq!(
                            owners.read_string(values[0], DescriptorId::STRING),
                            Ok("parallel".into())
                        );
                        owners.release(values[0]).expect("release values snapshot");

                        let removed = sibling
                            .remove_snapshot("absent")
                            .expect("unrelated remove creates a valid snapshot");
                        assert_eq!(
                            owners.read_string(removed.root.entries["key"], DescriptorId::STRING),
                            Ok("parallel".into())
                        );
                        drop(removed);
                        drop(sibling);
                    }
                })
            })
            .collect::<Vec<_>>();

        for worker in workers {
            worker.join().expect("concurrent owner operations succeed");
        }
        assert_eq!(owners.live_count(), Ok(1));
        drop(shared);
        assert_eq!(owners.live_count(), Ok(0));
        assert_eq!(probe.count(), 1);
    }

    #[test]
    fn owned_map_set_failpoints_rollback_tokens_and_preserve_old_root() {
        let probe = DropProbe::default();
        let owners = Arc::new(AnyOwnerTable::with_capacity(5));
        let key_seed = owners
            .allocate(DescriptorId::I64, AnyValue::I64(7))
            .expect("key seed fits");
        let empty = OwnedPrototypeMap::empty(owners.clone());
        let with_key = empty
            .set_transactional("key", key_seed, DescriptorId::I64, SetFailpoint::None)
            .expect("first entry succeeds");
        drop(empty);
        owners.release(key_seed).expect("release key seed");

        let keep_seed = owners
            .allocate(DescriptorId::I64, AnyValue::I64(9))
            .expect("keep seed fits");
        let base = with_key
            .set_transactional("keep", keep_seed, DescriptorId::I64, SetFailpoint::None)
            .expect("second entry succeeds");
        drop(with_key);
        owners.release(keep_seed).expect("release keep seed");

        let incoming = owners
            .allocate(
                DescriptorId::STRING,
                AnyValue::String(AnyStringOwner::new("replacement", probe.clone())),
            )
            .expect("incoming value fits");
        let initial_owners = owners.live_count().expect("table is healthy");
        let base_identity = base.root_identity();
        let initial_roots = owners.live_map_root_count().expect("root table is healthy");
        for failpoint in [
            SetFailpoint::AfterInputRetain,
            SetFailpoint::AfterEntryRetains,
            SetFailpoint::BeforePublish,
        ] {
            assert_eq!(
                base.set_transactional("key", incoming, DescriptorId::STRING, failpoint)
                    .map(|_| ()),
                Err(OwnerError::InjectedCommitFailure)
            );
            assert_eq!(owners.live_count(), Ok(initial_owners));
            assert_eq!(owners.live_map_root_count(), Ok(initial_roots));
            assert!(owners.resolve_map_root(base_identity).is_ok());
            assert_eq!(
                owners.read_i64(base.root.entries["key"], DescriptorId::I64),
                Ok(7)
            );
            assert_eq!(
                owners.read_i64(base.root.entries["keep"], DescriptorId::I64),
                Ok(9)
            );
            assert_eq!(probe.count(), 0);
        }

        // Leave room for retaining the input but not for cloning every old
        // entry into the replacement root. The second retain must fail and
        // the staged input token must be rolled back after the table unlocks.
        let blocker = owners
            .allocate(DescriptorId::I64, AnyValue::I64(11))
            .expect("capacity leaves room for a blocker");
        let before_capacity_failure = owners.live_count().expect("table is healthy");
        assert_eq!(
            base.set_transactional("key", incoming, DescriptorId::STRING, SetFailpoint::None)
                .map(|_| ()),
            Err(OwnerError::Capacity)
        );
        assert_eq!(owners.live_count(), Ok(before_capacity_failure));
        assert_eq!(
            owners.read_i64(base.root.entries["key"], DescriptorId::I64),
            Ok(7)
        );
        assert_eq!(
            owners.read_i64(base.root.entries["keep"], DescriptorId::I64),
            Ok(9)
        );
        assert_eq!(
            owners.read_string(incoming, DescriptorId::STRING),
            Ok("replacement".into())
        );
        assert_eq!(probe.count(), 0);
        owners.release(blocker).expect("release capacity blocker");

        assert_eq!(
            base.set_transactional("key", incoming, DescriptorId::I64, SetFailpoint::None)
                .map(|_| ()),
            Err(OwnerError::DescriptorMismatch)
        );
        assert_eq!(owners.live_count(), Ok(initial_owners));

        let updated = base
            .set_transactional("key", incoming, DescriptorId::STRING, SetFailpoint::None)
            .expect("valid update creates a new owning root");
        assert_eq!(
            owners.read_i64(base.root.entries["key"], DescriptorId::I64),
            Ok(7)
        );
        assert_eq!(
            owners.read_string(updated.root.entries["key"], DescriptorId::STRING),
            Ok("replacement".into())
        );
        assert_eq!(owners.live_count(), Ok(initial_owners + 2));

        drop(base);
        assert_eq!(owners.live_count(), Ok(initial_owners));
        owners
            .release(incoming)
            .expect("caller releases incoming token");
        drop(updated);
        assert_eq!(probe.count(), 1);
        assert_eq!(owners.live_count(), Ok(0));
    }

    #[test]
    fn owned_map_values_capacity_failure_has_no_partial_token_or_root_change() {
        let owners = Arc::new(AnyOwnerTable::with_capacity(3));
        let first = owners
            .allocate(DescriptorId::I64, AnyValue::I64(1))
            .expect("first entry token fits");
        let second = owners
            .allocate(DescriptorId::I64, AnyValue::I64(2))
            .expect("second entry token fits");
        let map = OwnedPrototypeMap::from_owned_tokens(
            owners.clone(),
            BTreeMap::from([(Arc::from("a"), first), (Arc::from("b"), second)]),
        )
        .expect("all supplied owners belong to this table");

        assert_eq!(map.values_owned(), Err(OwnerError::Capacity));
        assert_eq!(owners.live_count(), Ok(2));
        assert_eq!(owners.read_i64(first, DescriptorId::I64), Ok(1));
        assert_eq!(owners.read_i64(second, DescriptorId::I64), Ok(2));

        let removed = map
            .remove_snapshot("a")
            .expect("remove creates a sibling root");
        assert_eq!(owners.live_count(), Ok(3));
        assert!(!removed.root.entries.contains_key("a"));
        assert_eq!(
            owners.read_i64(map.root.entries["a"], DescriptorId::I64),
            Ok(1)
        );
        assert_eq!(
            owners.read_i64(removed.root.entries["b"], DescriptorId::I64),
            Ok(2)
        );

        // The original root still owns both entries, and the table is full.
        // A remove snapshot needs to retain the surviving entry, so capacity
        // failure must not partially publish a root or invalidate either old
        // token.
        assert_eq!(
            map.remove_snapshot("b").map(|_| ()),
            Err(OwnerError::Capacity)
        );
        assert_eq!(owners.live_count(), Ok(3));
        assert_eq!(owners.read_i64(first, DescriptorId::I64), Ok(1));
        assert_eq!(owners.read_i64(second, DescriptorId::I64), Ok(2));

        drop(removed);
        assert_eq!(owners.live_count(), Ok(2));
    }
}
