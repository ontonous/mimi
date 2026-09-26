//! Test-only model for a typed, owning Map/Any representation.
//!
//! This deliberately does not encode its identity into the existing `i64`
//! `ValueHandle` ABI and is not connected to runtime FFI or MIR routing. It
//! demonstrates the owner graph that a future typed ingress/egress contract
//! would need to preserve.

use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

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

    fn descriptor(&self) -> DescriptorId {
        match self {
            Self::I64(_) => DescriptorId::I64,
            Self::String(_) => DescriptorId::STRING,
            Self::Aggregate(_) => DescriptorId::AGGREGATE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DescriptorId(u32);

impl DescriptorId {
    const I64: Self = Self(1);
    const STRING: Self = Self(2);
    const AGGREGATE: Self = Self(3);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnyHandle {
    slot: u32,
    generation: u32,
    descriptor: DescriptorId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OwnerError {
    InvalidHandle,
    StaleHandle,
    UnknownDescriptor,
    DescriptorMismatch,
    Capacity,
    Poisoned,
    InjectedCommitFailure,
}

#[derive(Clone)]
struct TypedAny {
    descriptor: DescriptorId,
    value: AnyValue,
}

struct OwnerSlot {
    generation: u32,
    value: Option<TypedAny>,
    retired: bool,
}

struct OwnerState {
    descriptors: BTreeMap<DescriptorId, ValueShape>,
    slots: Vec<OwnerSlot>,
    free: Vec<u32>,
    max_slots: usize,
}

impl OwnerState {
    fn validate(&self, handle: AnyHandle) -> Result<&TypedAny, OwnerError> {
        let expected_shape = self
            .descriptors
            .get(&handle.descriptor)
            .ok_or(OwnerError::UnknownDescriptor)?;
        let slot = self
            .slots
            .get(handle.slot as usize)
            .ok_or(OwnerError::InvalidHandle)?;
        if slot.generation != handle.generation || slot.value.is_none() {
            return Err(OwnerError::StaleHandle);
        }
        let value = slot.value.as_ref().expect("checked live owner slot");
        if value.descriptor != handle.descriptor || value.value.shape() != *expected_shape {
            return Err(OwnerError::DescriptorMismatch);
        }
        Ok(value)
    }

    fn allocate_many(&mut self, values: &mut Vec<TypedAny>) -> Result<Vec<AnyHandle>, OwnerError> {
        for value in values.iter() {
            let expected_shape = self
                .descriptors
                .get(&value.descriptor)
                .ok_or(OwnerError::UnknownDescriptor)?;
            if value.value.shape() != *expected_shape {
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
                slot: slot_index,
                generation: slot.generation,
                descriptor: slot
                    .value
                    .as_ref()
                    .expect("new owner slot is populated")
                    .descriptor,
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
    state: std::sync::Mutex<OwnerState>,
}

impl AnyOwnerTable {
    fn with_capacity(max_slots: usize) -> Self {
        let descriptors = BTreeMap::from([
            (DescriptorId::I64, ValueShape::I64),
            (DescriptorId::STRING, ValueShape::String),
            (DescriptorId::AGGREGATE, ValueShape::Aggregate),
        ]);
        Self {
            state: std::sync::Mutex::new(OwnerState {
                descriptors,
                slots: Vec::new(),
                free: Vec::new(),
                max_slots: max_slots.min(u32::MAX as usize),
            }),
        }
    }

    fn allocate(&self, descriptor: DescriptorId, value: AnyValue) -> Result<AnyHandle, OwnerError> {
        self.allocate_many(vec![TypedAny { descriptor, value }])
            .map(|mut handles| handles.pop().expect("single value yields one owner token"))
    }

    fn allocate_many(&self, values: Vec<TypedAny>) -> Result<Vec<AnyHandle>, OwnerError> {
        let mut values = values;
        let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        let result = state.allocate_many(&mut values);
        drop(state);
        drop(values);
        result
    }

    fn clone_value(&self, handle: AnyHandle) -> Result<TypedAny, OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        Ok(state.validate(handle)?.clone())
    }

    fn read_i64(&self, handle: AnyHandle, expected: DescriptorId) -> Result<i64, OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        let value = state.validate(handle)?;
        if expected != DescriptorId::I64 || value.descriptor != expected {
            return Err(OwnerError::DescriptorMismatch);
        }
        value.value.as_i64().ok_or(OwnerError::DescriptorMismatch)
    }

    fn read_string(&self, handle: AnyHandle, expected: DescriptorId) -> Result<String, OwnerError> {
        let state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        let value = state.validate(handle)?;
        if expected != DescriptorId::STRING || value.descriptor != expected {
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
        let value = state.validate(handle)?.clone();
        let mut values = vec![value];
        let result = state.allocate_many(&mut values);
        drop(state);
        drop(values);
        result.map(|mut handles| handles.pop().expect("one retained value yields one token"))
    }

    fn retain_many(&self, handles: &[AnyHandle]) -> Result<Vec<AnyHandle>, OwnerError> {
        let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
        for handle in handles {
            state.validate(*handle)?;
        }
        let mut values = handles
            .iter()
            .map(|handle| {
                state
                    .validate(*handle)
                    .expect("prevalidated owner token")
                    .clone()
            })
            .collect();
        let result = state.allocate_many(&mut values);
        drop(state);
        drop(values);
        result
    }

    fn release(&self, handle: AnyHandle) -> Result<(), OwnerError> {
        let released = {
            let mut state = self.state.lock().map_err(|_| OwnerError::Poisoned)?;
            state.validate(handle)?;
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
        // Dropping a value may release nested owners; never run that work under
        // the table lock.
        drop(released);
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
        state.validate(handle)?;
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
    owners: Arc<AnyOwnerTable>,
    entries: BTreeMap<Arc<str>, AnyHandle>,
}

impl Drop for OwnedMapRoot {
    fn drop(&mut self) {
        for handle in self.entries.values() {
            let _ = self.owners.release(*handle);
        }
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
            root: Arc::new(OwnedMapRoot {
                owners,
                entries: BTreeMap::new(),
            }),
        }
    }

    /// Test fixture constructor that transfers the supplied owner tokens into
    /// the new root; callers must stop using those tokens after this call.
    fn from_owned_tokens(
        owners: Arc<AnyOwnerTable>,
        entries: BTreeMap<Arc<str>, AnyHandle>,
    ) -> Self {
        Self {
            root: Arc::new(OwnedMapRoot { owners, entries }),
        }
    }

    fn set_transactional(
        &self,
        key: &str,
        input: AnyHandle,
        expected: DescriptorId,
        failpoint: SetFailpoint,
    ) -> Result<Self, OwnerError> {
        let incoming = self.root.owners.clone_value(input)?;
        if input.descriptor != expected || incoming.descriptor != expected {
            return Err(OwnerError::DescriptorMismatch);
        }

        let mut staged = StagedOwnerTokens::new(self.root.owners.clone());
        let input_owner = self.root.owners.retain(input)?;
        staged.add(input_owner);
        if failpoint == SetFailpoint::AfterInputRetain {
            return Err(OwnerError::InjectedCommitFailure);
        }

        let mut entries = BTreeMap::new();
        for (old_key, old_handle) in &self.root.entries {
            if old_key.as_ref() == key {
                continue;
            }
            let retained = self.root.owners.retain(*old_handle)?;
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

        let result = Self {
            root: Arc::new(OwnedMapRoot {
                owners: self.root.owners.clone(),
                entries,
            }),
        };
        staged.commit();
        Ok(result)
    }

    fn get_owned(&self, key: &str) -> Result<Option<AnyHandle>, OwnerError> {
        self.root
            .entries
            .get(key)
            .map(|handle| self.root.owners.retain(*handle))
            .transpose()
    }

    fn values_owned(&self) -> Result<Vec<AnyHandle>, OwnerError> {
        self.root
            .owners
            .retain_many(&self.root.entries.values().copied().collect::<Vec<_>>())
    }

    fn remove_snapshot(&self, key: &str) -> Result<Self, OwnerError> {
        let retained = self
            .root
            .entries
            .iter()
            .filter(|(old_key, _)| old_key.as_ref() != key)
            .map(|(old_key, handle)| (old_key.clone(), *handle))
            .collect::<Vec<_>>();
        let new_handles = self.root.owners.retain_many(
            &retained
                .iter()
                .map(|(_, handle)| *handle)
                .collect::<Vec<_>>(),
        )?;
        let entries = retained
            .into_iter()
            .zip(new_handles)
            .map(|((key, _), handle)| (key, handle))
            .collect();
        Ok(Self {
            root: Arc::new(OwnedMapRoot {
                owners: self.root.owners.clone(),
                entries,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnyHandle, AnyOwnerTable, AnyStringOwner, AnyValue, DescriptorId, DropProbe,
        OwnedAggregate, OwnedPrototypeMap, OwnerError, PrototypeMap, SetFailpoint,
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
            descriptor: DescriptorId(99),
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
        );

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
