//! Exact identities for checker-selected static generic trait methods.
//!
//! A source method name is useful for diagnostics, but it is not an instance
//! key. Production dispatch carries the checked impl owner, callable, ordered
//! binders and type arguments, receiver type, and trait slot family together.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::core::{NodeId, ResolvedTypeId};

pub(crate) type CheckedCallSpan = (u32, usize, usize, usize, usize);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct CheckedTraitMethodInstanceKey {
    pub(crate) impl_owner: NodeId,
    pub(crate) trait_owner: NodeId,
    pub(crate) callable: NodeId,
    /// Ordered implementation binders followed by method-local binders.
    pub(crate) binder_ids: Vec<NodeId>,
    /// Checker-zonked arguments in exactly the same order as `binder_ids`.
    pub(crate) type_arguments: Vec<ResolvedTypeId>,
    pub(crate) receiver_type: ResolvedTypeId,
    /// Checked trait ABI slots occupied by this method.
    pub(crate) trait_slots: Vec<u32>,
}

impl CheckedTraitMethodInstanceKey {
    pub(crate) fn new(
        impl_owner: NodeId,
        trait_owner: NodeId,
        callable: NodeId,
        binder_ids: Vec<NodeId>,
        type_arguments: Vec<ResolvedTypeId>,
        receiver_type: ResolvedTypeId,
        trait_slots: Vec<u32>,
    ) -> Result<Self, String> {
        if impl_owner.0.trim().is_empty()
            || trait_owner.0.trim().is_empty()
            || callable.0.trim().is_empty()
            || receiver_type.as_str().trim().is_empty()
        {
            return Err("checked trait method identity contains an empty stable ID".into());
        }
        if binder_ids.len() != type_arguments.len() {
            return Err(format!(
                "checked trait method has {} binders but {} ordered type arguments",
                binder_ids.len(),
                type_arguments.len()
            ));
        }
        if binder_ids.iter().any(|binder| binder.0.trim().is_empty())
            || binder_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != binder_ids.len()
        {
            return Err("checked trait method binder identities are empty or repeated".into());
        }
        if trait_slots.is_empty() || trait_slots.windows(2).any(|slots| slots[0] >= slots[1]) {
            return Err("checked trait method slot family is empty or not strictly ordered".into());
        }
        Ok(Self {
            impl_owner,
            trait_owner,
            callable,
            binder_ids,
            type_arguments,
            receiver_type,
            trait_slots,
        })
    }

    /// True when any ordered argument still contains a checker binder. Such a
    /// key is retained as a receipt but cannot be treated as a concrete body
    /// specialization without an independent unused-binder proof.
    pub(crate) fn has_parametric_type_arguments(
        &self,
        types: &crate::core::ResolvedTypeTable,
    ) -> Result<bool, String> {
        let mut visited = BTreeSet::new();
        for argument in &self.type_arguments {
            if contains_generic_parameter(argument, types, &mut visited)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn contains_generic_parameter(
    id: &ResolvedTypeId,
    types: &crate::core::ResolvedTypeTable,
    visited: &mut BTreeSet<ResolvedTypeId>,
) -> Result<bool, String> {
    use crate::core::ir::ResolvedType;

    if !visited.insert(id.clone()) {
        return Ok(false);
    }
    let ty = types
        .get(id)
        .ok_or_else(|| format!("checked type argument '{}' is absent", id.as_str()))?;
    let children: Vec<&ResolvedTypeId> = match ty {
        ResolvedType::GenericParameter(_) => return Ok(true),
        ResolvedType::Nominal { arguments, .. } | ResolvedType::Tuple(arguments) => {
            arguments.iter().collect()
        }
        ResolvedType::Reference { target, .. }
        | ResolvedType::Option(target)
        | ResolvedType::CBuffer(target)
        | ResolvedType::Ownership { target, .. }
        | ResolvedType::Newtype { inner: target, .. }
        | ResolvedType::Slice(target)
        | ResolvedType::RawPointer { target, .. } => vec![target],
        ResolvedType::Result { ok, error } => vec![ok, error],
        ResolvedType::Function {
            parameters, result, ..
        } => parameters.iter().chain(std::iter::once(result)).collect(),
        ResolvedType::Array { element, .. } => vec![element],
        ResolvedType::Primitive(_)
        | ResolvedType::FlowStateSet { .. }
        | ResolvedType::Capability(_)
        | ResolvedType::Trait { .. }
        | ResolvedType::DynamicAny { .. } => Vec::new(),
    };
    for child in children {
        if contains_generic_parameter(child, types, visited)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckedTraitMethodTarget {
    /// Exact checker-selected dispatch facts. This is intentionally metadata
    /// only: no canonical MIR instance owner is claimed until specialization
    /// consumes the full disposition-aware materializer key.
    pub(crate) key: CheckedTraitMethodInstanceKey,
    /// Stable checked native declaration symbol retained only while the
    /// compatibility body emitter remains an active consumer.
    pub(crate) compatibility_symbol: String,
}

impl CheckedTraitMethodTarget {
    pub(crate) fn new(
        key: CheckedTraitMethodInstanceKey,
        compatibility_symbol: String,
    ) -> Result<Self, String> {
        if compatibility_symbol.trim().is_empty() {
            return Err("checked trait method compatibility symbol is empty".into());
        }
        Ok(Self {
            key,
            compatibility_symbol,
        })
    }
}

/// Storage for typed callsite targets and their full static generic dispatch
/// keys. This receipt table does not materialize executable instance IDs.
#[derive(Debug, Clone, Default)]
pub(crate) struct CheckedTraitMethodDispatchIndex {
    pub(crate) call_targets: HashMap<CheckedCallSpan, Vec<CheckedTraitMethodTarget>>,
    pub(crate) call_callees: HashMap<CheckedCallSpan, crate::core::ir::ResolvedCallee>,
    compatibility_symbols: BTreeMap<CheckedTraitMethodInstanceKey, String>,
}

impl CheckedTraitMethodDispatchIndex {
    pub(crate) fn clear(&mut self) {
        self.call_targets.clear();
        self.call_callees.clear();
        self.compatibility_symbols.clear();
    }

    pub(crate) fn register_callee(
        &mut self,
        span: CheckedCallSpan,
        callee: crate::core::ir::ResolvedCallee,
    ) -> Result<(), String> {
        if let Some(previous) = self.call_callees.get(&span) {
            if previous != &callee {
                return Err("checked method source span has conflicting typed callees".into());
            }
        } else {
            self.call_callees.insert(span, callee);
        }
        Ok(())
    }

    pub(crate) fn callee_for_span(
        &self,
        span: &CheckedCallSpan,
    ) -> Option<&crate::core::ir::ResolvedCallee> {
        self.call_callees.get(span)
    }

    pub(crate) fn register(
        &mut self,
        span: CheckedCallSpan,
        target: CheckedTraitMethodTarget,
    ) -> Result<(), String> {
        if let Some(previous) = self.compatibility_symbols.get(&target.key) {
            if previous != &target.compatibility_symbol {
                return Err(format!(
                    "checked trait method instance key for '{}' maps to conflicting compatibility symbols",
                    target.key.callable.0
                ));
            }
        } else {
            self.compatibility_symbols
                .insert(target.key.clone(), target.compatibility_symbol.clone());
        }
        let targets = self.call_targets.entry(span).or_default();
        if !targets.iter().any(|existing| existing == &target) {
            targets.push(target);
            targets.sort_by(|left, right| left.key.cmp(&right.key));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ir::{PrimitiveType, ResolvedType, ResolvedTypeTable};

    fn ty(table: &mut ResolvedTypeTable, primitive: PrimitiveType) -> ResolvedTypeId {
        table
            .intern_resolved(ResolvedType::Primitive(primitive))
            .expect("canonical primitive type")
    }

    fn key(
        impl_owner: &str,
        binder_ids: Vec<NodeId>,
        type_arguments: Vec<ResolvedTypeId>,
        receiver_type: ResolvedTypeId,
        trait_slots: Vec<u32>,
    ) -> CheckedTraitMethodInstanceKey {
        CheckedTraitMethodInstanceKey::new(
            NodeId(impl_owner.into()),
            NodeId("trait:Marker".into()),
            NodeId("function:Marker:for:List::read:root".into()),
            binder_ids,
            type_arguments,
            receiver_type,
            trait_slots,
        )
        .expect("well-formed checked target key")
    }

    #[test]
    fn checked_generic_trait_instance_identity_keeps_every_dispatch_dimension() {
        let mut types = ResolvedTypeTable::new();
        let i32_ty = ty(&mut types, PrimitiveType::I32);
        let string_ty = ty(&mut types, PrimitiveType::String);
        let receiver = ty(&mut types, PrimitiveType::I64);
        let other_receiver = ty(&mut types, PrimitiveType::Bool);
        let binder = NodeId("binder:Marker:T".into());
        let base = key(
            "impl:Marker:i32",
            vec![binder.clone()],
            vec![i32_ty.clone()],
            receiver.clone(),
            vec![0],
        );
        let other_impl = key(
            "impl:Marker:string",
            vec![binder.clone()],
            vec![i32_ty.clone()],
            receiver.clone(),
            vec![0],
        );
        assert_ne!(
            base,
            other_impl.clone(),
            "impl owner is part of the instance key even when all other fields coincide"
        );
        assert_ne!(
            base,
            key(
                "impl:Marker:i32",
                vec![binder.clone()],
                vec![string_ty.clone()],
                receiver.clone(),
                vec![0],
            ),
            "ordered checker type arguments are part of the instance key"
        );
        assert_ne!(
            base,
            key(
                "impl:Marker:i32",
                vec![binder],
                vec![i32_ty],
                other_receiver.clone(),
                vec![0],
            ),
            "canonical receiver type is part of the instance key"
        );
        let mut different_trait = base.clone();
        different_trait.trait_owner = NodeId("trait:Other".into());
        assert_ne!(base, different_trait, "trait owner is part of the key");
        let mut different_callable = base.clone();
        different_callable.callable = NodeId("function:Other:read:root".into());
        assert_ne!(
            base, different_callable,
            "callable owner is part of the key"
        );
        let mut different_binder_order = base.clone();
        different_binder_order.binder_ids = vec![NodeId("binder:Marker:Other".into())];
        assert_ne!(
            base, different_binder_order,
            "ordered generic binder identities are part of the key"
        );
        let mut different_slots = base.clone();
        different_slots.trait_slots = vec![0, 2];
        assert_ne!(
            base, different_slots,
            "trait slot family is part of the instance key"
        );
        let mut different_arguments = base.clone();
        different_arguments.type_arguments = vec![string_ty];
        let mut different_receiver_key = base.clone();
        different_receiver_key.receiver_type = other_receiver;
        for (variant, dimension) in [
            (different_trait, "trait owner"),
            (different_callable, "callable owner"),
            (different_binder_order, "ordered binders"),
            (different_slots, "trait slots"),
            (different_arguments, "ordered type arguments"),
            (different_receiver_key, "receiver type"),
        ] {
            assert_ne!(base, variant, "the exact dispatch key includes {dimension}");
        }
    }

    #[test]
    fn ambiguous_span_receipts_are_retained_for_fail_closed_consumers() {
        let mut types = ResolvedTypeTable::new();
        let i32_ty = ty(&mut types, PrimitiveType::I32);
        let receiver = ty(&mut types, PrimitiveType::I64);
        let first = key(
            "impl:Marker:i32",
            vec![NodeId("binder:Marker:T".into())],
            vec![i32_ty.clone()],
            receiver.clone(),
            vec![0],
        );
        let second = key(
            "impl:Marker:string",
            vec![NodeId("binder:Marker:T".into())],
            vec![i32_ty],
            receiver,
            vec![0],
        );
        let mut index = CheckedTraitMethodDispatchIndex::default();
        index
            .register(
                (1, 2, 3, 4, 5),
                CheckedTraitMethodTarget::new(first, "first-symbol".into())
                    .expect("first exact target"),
            )
            .expect("first receipt");
        index
            .register(
                (1, 2, 3, 4, 5),
                CheckedTraitMethodTarget::new(second, "second-symbol".into())
                    .expect("second exact target"),
            )
            .expect("second receipt");
        assert_eq!(
            index.call_targets[&(1, 2, 3, 4, 5)].len(),
            2,
            "the consumer must reject multiple exact owners instead of picking the first"
        );
    }

    #[test]
    fn malformed_generic_trait_instance_receipts_fail_closed() {
        let mut types = ResolvedTypeTable::new();
        let i32_ty = ty(&mut types, PrimitiveType::I32);
        let receiver = ty(&mut types, PrimitiveType::I64);
        assert!(CheckedTraitMethodInstanceKey::new(
            NodeId("impl:Marker:i32".into()),
            NodeId("trait:Marker".into()),
            NodeId("function:Marker:for:List::read:root".into()),
            vec![NodeId("binder:T".into())],
            vec![],
            receiver.clone(),
            vec![0],
        )
        .is_err());
        assert!(CheckedTraitMethodInstanceKey::new(
            NodeId("impl:Marker:i32".into()),
            NodeId("trait:Marker".into()),
            NodeId("function:Marker:for:List::read:root".into()),
            vec![NodeId("binder:T".into()), NodeId("binder:T".into())],
            vec![i32_ty.clone(), i32_ty],
            receiver.clone(),
            vec![0],
        )
        .is_err());
        assert!(CheckedTraitMethodInstanceKey::new(
            NodeId("impl:Marker:i32".into()),
            NodeId("trait:Marker".into()),
            NodeId("function:Marker:for:List::read:root".into()),
            vec![],
            vec![],
            receiver,
            vec![1, 0],
        )
        .is_err());
    }

    #[test]
    fn checked_target_remains_metadata_only_until_full_specialization_key_is_available() {
        let mut types = ResolvedTypeTable::new();
        let i32_ty = ty(&mut types, PrimitiveType::I32);
        let receiver = ty(&mut types, PrimitiveType::I64);
        let key = key(
            "impl:Marker:i32",
            vec![NodeId("binder:Marker:T".into())],
            vec![i32_ty],
            receiver,
            vec![0],
        );
        let target = CheckedTraitMethodTarget::new(key.clone(), "checked-symbol".into())
            .expect("checked metadata target");
        let mut index = CheckedTraitMethodDispatchIndex::default();
        index
            .register((1, 2, 3, 4, 5), target.clone())
            .expect("receipt registration");
        assert_eq!(target.key, key);
        assert_eq!(target.compatibility_symbol, "checked-symbol");
        assert_eq!(index.call_targets[&(1, 2, 3, 4, 5)], vec![target]);
    }

    #[test]
    fn parametric_checker_binder_is_retained_and_not_a_concrete_instance_argument() {
        let mut types = ResolvedTypeTable::new();
        let binder = NodeId("binder:Marker:T".into());
        let parametric_ty = types
            .intern_resolved(ResolvedType::GenericParameter(binder.clone()))
            .expect("canonical generic parameter type");
        let string_ty = ty(&mut types, PrimitiveType::String);
        let receiver = ty(&mut types, PrimitiveType::I64);
        let key = key(
            "impl:Marker:Holder",
            vec![binder],
            vec![parametric_ty.clone()],
            receiver,
            vec![0],
        );
        assert_eq!(key.type_arguments, vec![parametric_ty]);
        assert!(key
            .has_parametric_type_arguments(&types)
            .expect("checker binder is present in the type table"));

        let generic_key = key.clone();
        let concrete_key = CheckedTraitMethodInstanceKey {
            type_arguments: vec![string_ty],
            ..key
        };
        assert_ne!(
            generic_key, concrete_key,
            "the exact unresolved binder remains distinct from a concrete substitution"
        );
        assert!(!concrete_key
            .has_parametric_type_arguments(&types)
            .expect("concrete argument is present in the type table"));
    }

    #[test]
    fn index_rejects_conflicting_compatibility_symbols_for_one_exact_key() {
        let mut types = ResolvedTypeTable::new();
        let i32_ty = ty(&mut types, PrimitiveType::I32);
        let receiver = ty(&mut types, PrimitiveType::I64);
        let key = key(
            "impl:Marker:i32",
            vec![NodeId("binder:Marker:T".into())],
            vec![i32_ty],
            receiver,
            vec![0],
        );
        let mut index = CheckedTraitMethodDispatchIndex::default();
        index
            .register(
                (1, 2, 3, 4, 5),
                CheckedTraitMethodTarget::new(key.clone(), "canonical-symbol".into())
                    .expect("checked target"),
            )
            .expect("first receipt");
        assert!(index
            .register(
                (1, 2, 3, 4, 6),
                CheckedTraitMethodTarget::new(key, "conflicting-symbol".into())
                    .expect("checked target"),
            )
            .is_err());
    }
}
