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

#[cfg(test)]
mod tests {
    use super::{AnyStringOwner, AnyValue, DropProbe, OwnedAggregate, PrototypeMap};
    use std::collections::BTreeMap;
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
}
