//! Checker-owned native declaration facts. No surface type or declaration is
//! retained here. Layout consumers resolve the typed edges with their own ABI
//! policy; a payload recipe is not a receipt for executable drop glue.

use std::collections::{BTreeMap, BTreeSet};

use super::{ResolvedSignature, ResolvedType, ResolvedTypeId};
use crate::core::{
    CheckedProgram, NodeId, PrimitiveType, ResolvedTrait, ResolvedTypeKind, ResolvedTypeTable,
    ResolvedVariantShape,
};

pub const RESOLVED_NATIVE_DECLARATION_SCHEMA: &str = "mimi-native-declarations-v5";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeGenericKind {
    Free,
    Linear,
    LinearDrop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeGenericBound {
    pub name: String,
    /// A bound already naming a checked trait has its stable declaration ID.
    /// Other accepted bound vocabulary is retained explicitly, not erased.
    pub trait_owner: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeGenericBinder {
    pub owner: NodeId,
    pub binder: NodeId,
    pub name: String,
    pub kind: NativeGenericKind,
    pub bounds: Vec<NativeGenericBound>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeRepresentation {
    pub repr_c: bool,
    pub repr_transparent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFieldDeclaration {
    pub owner: NodeId,
    pub field: NodeId,
    pub name: String,
    pub ty: ResolvedTypeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeVariantDeclaration {
    pub owner: NodeId,
    pub variant: NodeId,
    pub name: String,
    /// Native ABI tags follow name order; this vector follows source order.
    pub native_tag: u64,
    pub shape: ResolvedVariantShape,
    pub members: Vec<NativeFieldDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeTypeDeclarationKind {
    Alias {
        target: ResolvedTypeId,
    },
    Newtype {
        target: ResolvedTypeId,
    },
    Record {
        fields: Vec<NativeFieldDeclaration>,
    },
    Enum {
        variants: Vec<NativeVariantDeclaration>,
    },
    Union {
        fields: Vec<NativeFieldDeclaration>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTypeDeclaration {
    pub owner: NodeId,
    pub name: String,
    pub public: bool,
    pub representation: NativeRepresentation,
    pub generics: Vec<NativeGenericBinder>,
    pub derives: Vec<String>,
    pub kind: NativeTypeDeclarationKind,
    /// Stable nominal dependencies reachable through canonical type edges.
    /// Forward references stay identities; consumers never substitute i64.
    pub referenced_declarations: BTreeSet<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeStateDeclaration {
    pub flow: NodeId,
    pub state: NodeId,
    pub name: String,
    pub fields: Vec<NativeFieldDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFlowDeclaration {
    pub owner: NodeId,
    pub name: String,
    pub generics: Vec<NativeGenericBinder>,
    pub states: Vec<NativeStateDeclaration>,
    /// Canonical transition order, identities, ABI signatures and dispatch
    /// flags. Bodies remain in the compatibility callable owner.
    pub transitions: Vec<NativeFlowTransitionDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFlowTransitionDeclaration {
    pub owner: NodeId,
    pub event: String,
    pub source_state: NodeId,
    pub source_name: String,
    pub target_states: Vec<NodeId>,
    pub target_names: Vec<String>,
    pub signature: ResolvedSignature,
    pub is_fallback: bool,
    pub is_ffi_pinned: bool,
    pub silent_transition: bool,
    /// Present only when the declaration carries an explicit `fails E`.
    /// The canonical signature result carries the full Result ABI.
    pub failure_type: Option<ResolvedTypeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeActorDeclaration {
    pub owner: NodeId,
    pub qualified_name: String,
    /// This is the validated checker-owned field/initializer/method ABI.
    /// Actor executable bodies remain under the tagged legacy owner.
    pub descriptor: super::ResolvedActorDescriptor,
}

/// One trait vtable slot. The vector order is the ABI slot order; the ordinal
/// is repeated so consumers can validate before materializing LLVM fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTraitSlotDeclaration {
    pub ordinal: u32,
    pub name: String,
    pub method_owner: NodeId,
    /// Canonical source-level declaration ABI. The physical erased vtable
    /// pointer shape remains a backend lowering decision.
    pub signature: ResolvedSignature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTraitDeclaration {
    pub owner: NodeId,
    pub qualified_name: String,
    pub slots: Vec<NativeTraitSlotDeclaration>,
}

/// A checked impl method attached to trait slots. `signature` is the exact
/// checker-finalized source-level signature; the legacy compiler still owns
/// the physical receiver convention. `symbol` is namespaced by checked impl,
/// callable, generic binders, method ordinal, and trait-slot family so distinct
/// generic impls cannot alias one LLVM body. An unbound slot remains null as in
/// the compatibility vtable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImplMethodDeclaration {
    pub name: String,
    /// Method ordinal within the checked implementation, independent of the
    /// trait slot ordinal(s) occupied by this declaration.
    pub ordinal: u32,
    pub callable: NodeId,
    pub signature: ResolvedSignature,
    /// Method-local binders. Impl binders live on the enclosing declaration;
    /// the signature preserves their concatenated canonical order.
    pub generics: Vec<NativeGenericBinder>,
    pub symbol: String,
    /// A repeated method name can occupy multiple legacy ABI slots.
    pub trait_slots: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImplDeclaration {
    pub owner: NodeId,
    pub qualified_name: String,
    pub trait_owner: NodeId,
    pub trait_name: String,
    pub type_name: String,
    pub generics: Vec<NativeGenericBinder>,
    /// Exact ordered vtable slot family for this checked implementation.
    pub vtable_slots: Vec<u32>,
    /// Unique global identity for the implementation's slot family.
    pub vtable_symbol: String,
    pub methods: Vec<NativeImplMethodDeclaration>,
}

/// The closed scalar C-export ABI admitted by the canonical native wrapper.
/// `Unit` is only valid as a result; parameters always carry a value.  Keep
/// this enum checker-owned so native, bytecode and verifier consumers share
/// one explicit boundary receipt instead of rediscovering surface `Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeExportScalarKind {
    I32,
    I64,
    F32,
    F64,
    Bool,
    Unit,
}

impl NativeExportScalarKind {
    pub fn is_unit(self) -> bool {
        matches!(self, Self::Unit)
    }

    pub fn canonical_text(self) -> &'static str {
        match self {
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::Unit => "unit",
        }
    }
}

/// Checker-owned declaration for one scalar `extern "C"` export.  The
/// internal body symbol is deliberately recorded alongside the public symbol
/// so wrappers cannot accidentally bind a legacy or user-defined symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExportDeclaration {
    pub owner: NodeId,
    pub public_symbol: String,
    pub body_symbol: String,
    pub abi: String,
    pub parameters: Vec<(ResolvedTypeId, NativeExportScalarKind)>,
    pub result: (ResolvedTypeId, NativeExportScalarKind),
}

/// The single-payload case must be classified after substitution and actual
/// ABI lowering. A canonical tuple/string/record can require a box even when
/// its declaration contains exactly one member. This enum records the honest
/// input to that decision rather than guessing from a display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeVariantPayloadRecipe {
    Empty,
    ClassifySingle(ResolvedTypeId),
    Packed(Vec<ResolvedTypeId>),
    ExternTag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeVariantRecipe {
    pub variant: NodeId,
    pub native_tag: u64,
    pub payload: NativeVariantPayloadRecipe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeConstructorRecipe {
    None,
    Identity(ResolvedTypeId),
    Fields(Vec<ResolvedTypeId>),
    Variants(Vec<NativeVariantRecipe>),
    UnionStorage(Vec<ResolvedTypeId>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeDropRecipe {
    Target(ResolvedTypeId),
    Fields(Vec<ResolvedTypeId>),
    /// Tag-conditional payload ownership. ClassifySingle still requires the
    /// concrete layout proof before an ordinal can be placed in box-drop glue.
    Variants(Vec<NativeVariantRecipe>),
    /// Existing union registration supplies storage but no active-field proof.
    UnionActiveFieldRequired,
}

impl NativeTypeDeclaration {
    pub fn type_edges(&self) -> Vec<&ResolvedTypeId> {
        match &self.kind {
            NativeTypeDeclarationKind::Alias { target }
            | NativeTypeDeclarationKind::Newtype { target } => vec![target],
            NativeTypeDeclarationKind::Record { fields }
            | NativeTypeDeclarationKind::Union { fields } => fields.iter().map(|f| &f.ty).collect(),
            NativeTypeDeclarationKind::Enum { variants } => variants
                .iter()
                .flat_map(|v| v.members.iter().map(|m| &m.ty))
                .collect(),
        }
    }

    fn variant_recipes(&self, variants: &[NativeVariantDeclaration]) -> Vec<NativeVariantRecipe> {
        variants
            .iter()
            .map(|variant| NativeVariantRecipe {
                variant: variant.variant.clone(),
                native_tag: variant.native_tag,
                payload: if self.representation.repr_c {
                    NativeVariantPayloadRecipe::ExternTag
                } else {
                    match (variant.shape, variant.members.as_slice()) {
                        (ResolvedVariantShape::Unit, _) => NativeVariantPayloadRecipe::Empty,
                        (ResolvedVariantShape::Tuple, [member]) => {
                            NativeVariantPayloadRecipe::ClassifySingle(member.ty.clone())
                        }
                        _ => NativeVariantPayloadRecipe::Packed(
                            variant.members.iter().map(|m| m.ty.clone()).collect(),
                        ),
                    }
                },
            })
            .collect()
    }

    pub fn constructor_recipe(&self) -> NativeConstructorRecipe {
        match &self.kind {
            NativeTypeDeclarationKind::Alias { .. } => NativeConstructorRecipe::None,
            NativeTypeDeclarationKind::Newtype { target } => {
                NativeConstructorRecipe::Identity(target.clone())
            }
            NativeTypeDeclarationKind::Record { fields } => {
                NativeConstructorRecipe::Fields(fields.iter().map(|f| f.ty.clone()).collect())
            }
            NativeTypeDeclarationKind::Enum { variants } => {
                NativeConstructorRecipe::Variants(self.variant_recipes(variants))
            }
            NativeTypeDeclarationKind::Union { fields } => {
                NativeConstructorRecipe::UnionStorage(fields.iter().map(|f| f.ty.clone()).collect())
            }
        }
    }

    pub fn drop_recipe(&self) -> NativeDropRecipe {
        match &self.kind {
            NativeTypeDeclarationKind::Alias { target }
            | NativeTypeDeclarationKind::Newtype { target } => {
                NativeDropRecipe::Target(target.clone())
            }
            NativeTypeDeclarationKind::Record { fields } => {
                NativeDropRecipe::Fields(fields.iter().map(|f| f.ty.clone()).collect())
            }
            NativeTypeDeclarationKind::Enum { variants } => {
                NativeDropRecipe::Variants(self.variant_recipes(variants))
            }
            NativeTypeDeclarationKind::Union { .. } => NativeDropRecipe::UnionActiveFieldRequired,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNativeDeclarations {
    pub schema: &'static str,
    pub actor_order: Vec<NodeId>,
    pub actors: BTreeMap<NodeId, NativeActorDeclaration>,
    pub trait_order: Vec<NodeId>,
    pub traits: BTreeMap<NodeId, NativeTraitDeclaration>,
    pub impl_order: Vec<NodeId>,
    pub impls: BTreeMap<NodeId, NativeImplDeclaration>,
    pub type_order: Vec<NodeId>,
    pub types: BTreeMap<NodeId, NativeTypeDeclaration>,
    pub flow_order: Vec<NodeId>,
    pub flows: BTreeMap<NodeId, NativeFlowDeclaration>,
    /// Closed scalar `extern "C"` exports admitted by the canonical wrapper.
    pub export_order: Vec<NodeId>,
    pub exports: BTreeMap<NodeId, NativeExportDeclaration>,
    /// Export bodies which remain outside the canonical ABI slice.  Keeping
    /// these reasons in the checker catalog makes the compatibility boundary
    /// explicit and lets `--mir` fail closed before any legacy emitter runs.
    pub unsupported_exports: BTreeMap<NodeId, String>,
}

impl Default for ResolvedNativeDeclarations {
    fn default() -> Self {
        Self {
            schema: RESOLVED_NATIVE_DECLARATION_SCHEMA,
            actor_order: Vec::new(),
            actors: BTreeMap::new(),
            trait_order: Vec::new(),
            traits: BTreeMap::new(),
            impl_order: Vec::new(),
            impls: BTreeMap::new(),
            type_order: Vec::new(),
            types: BTreeMap::new(),
            flow_order: Vec::new(),
            flows: BTreeMap::new(),
            export_order: Vec::new(),
            exports: BTreeMap::new(),
            unsupported_exports: BTreeMap::new(),
        }
    }
}

impl ResolvedNativeDeclarations {
    /// Stable semantic data only: BTreeMap keys and declaration-order vectors,
    /// never HashMap traversal, surface Debug text, or session SourceIds.
    pub fn canonical_text(&self) -> String {
        format!("{self:#?}\n")
    }

    /// Closed transparent aliases whose complete canonical target chain ends
    /// at a primitive. Generic binders, nominal/newtype edges, and every
    /// structural shape remain outside this scalar ABI receipt.
    pub fn closed_primitive_aliases(
        &self,
        types: &ResolvedTypeTable,
    ) -> BTreeMap<NodeId, PrimitiveType> {
        self.type_order
            .iter()
            .filter_map(|owner| {
                let declaration = self.types.get(owner)?;
                if !declaration.generics.is_empty()
                    || !matches!(&declaration.kind, NativeTypeDeclarationKind::Alias { .. })
                {
                    return None;
                }
                let NativeTypeDeclarationKind::Alias { target } = &declaration.kind else {
                    return None;
                };
                let primitive =
                    terminal_primitive_alias_target(target, self, types, &mut BTreeSet::new())?;
                Some((owner.clone(), primitive))
            })
            .collect()
    }

    pub fn validate_against(&self, program: &CheckedProgram) -> Result<(), String> {
        self.validate_structure(program)?;
        if program.native_declarations() != self {
            return Err(
                "native declaration facts differ from the immutable checker catalog".into(),
            );
        }
        Ok(())
    }

    fn validate_structure(&self, program: &CheckedProgram) -> Result<(), String> {
        if self.schema != RESOLVED_NATIVE_DECLARATION_SCHEMA {
            return Err("unknown native declaration schema".into());
        }
        let keys = |order: &[NodeId]| order.iter().cloned().collect::<BTreeSet<_>>();
        let checked_trait_method_owners = program
            .traits()
            .values()
            .flat_map(|trait_def| trait_def.method_owners.iter().cloned())
            .collect::<BTreeSet<_>>();
        if checked_trait_method_owners.len()
            != program
                .traits()
                .values()
                .map(|trait_def| trait_def.method_owners.len())
                .sum::<usize>()
            || checked_trait_method_owners
                != program
                    .resolved_trait_signatures()
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>()
        {
            return Err("canonical trait method ABI coverage or identity is invalid".into());
        }
        if keys(&self.actor_order).len() != self.actor_order.len()
            || keys(&self.actor_order) != self.actors.keys().cloned().collect()
            || self.actors.len() != program.actor_descriptors().len()
            || self.actors.len() != program.actors().len()
            || keys(&self.trait_order).len() != self.trait_order.len()
            || keys(&self.trait_order) != self.traits.keys().cloned().collect()
            || self.traits.len() != program.traits().len()
            || keys(&self.impl_order).len() != self.impl_order.len()
            || keys(&self.impl_order) != self.impls.keys().cloned().collect()
            || self.impls.len() != program.impls().len()
            || keys(&self.type_order).len() != self.type_order.len()
            || keys(&self.type_order) != self.types.keys().cloned().collect()
            || self.types.len() != program.type_defs().len()
            || keys(&self.flow_order).len() != self.flow_order.len()
            || keys(&self.flow_order) != self.flows.keys().cloned().collect()
            || self.flows.len() != program.flows().len()
        {
            return Err("native declaration identity coverage or order is invalid".into());
        }
        if keys(&self.export_order).len() != self.export_order.len()
            || keys(&self.export_order) != self.exports.keys().cloned().collect()
            || self
                .exports
                .keys()
                .any(|owner| self.unsupported_exports.contains_key(owner))
            || self
                .unsupported_exports
                .keys()
                .any(|owner| !program.functions().contains_key(owner))
        {
            return Err("native export identity coverage or order is invalid".into());
        }
        for (owner, declaration) in &self.exports {
            validate_export_declaration(program, owner, declaration)?;
        }
        for (owner, reason) in &self.unsupported_exports {
            if reason.trim().is_empty() {
                return Err(format!(
                    "native unsupported export '{}' has an empty compatibility reason",
                    owner.0
                ));
            }
            let function = program
                .functions()
                .get(owner)
                .ok_or("unsupported export owner is absent")?;
            if function.extern_abi.is_none() {
                return Err(format!(
                    "native unsupported export '{}' is not an extern-ABI function",
                    owner.0
                ));
            }
        }
        for (owner, declaration) in &self.actors {
            let source = program.actors().get(owner).ok_or("Actor owner is absent")?;
            let checked = program
                .actor_descriptors()
                .get(owner)
                .ok_or("checked Actor descriptor is absent")?;
            if declaration.owner != *owner
                || declaration.qualified_name != source.qualified_name
                || &declaration.descriptor != checked
                || declaration.descriptor.actor != *owner
            {
                return Err("native Actor owner, name or descriptor differs".into());
            }
            declaration
                .descriptor
                .validate_against(program)
                .map_err(|error| format!("native Actor descriptor is invalid: {error}"))?;
        }
        for (owner, declaration) in &self.traits {
            let source = program.traits().get(owner).ok_or("trait owner is absent")?;
            if declaration.owner != *owner
                || declaration.qualified_name != source.qualified_name
                || declaration.slots.len() != source.methods.len()
                || source.method_owners.len() != source.methods.len()
            {
                return Err("native trait owner, name or slot coverage differs".into());
            }
            for (ordinal, ((slot, name), method_owner)) in declaration
                .slots
                .iter()
                .zip(&source.methods)
                .zip(&source.method_owners)
                .enumerate()
            {
                let signature = program
                    .resolved_trait_signature(method_owner)
                    .ok_or("checked trait method canonical signature is absent")?;
                if slot.ordinal as usize != ordinal
                    || slot.name != *name
                    || slot.method_owner != *method_owner
                    || slot.signature.owner != *method_owner
                    || slot.signature != *signature
                {
                    return Err(
                        "native trait slot order, owner or canonical ABI differs from checked declaration"
                            .into(),
                    );
                }
                slot.signature
                    .validate(program.resolved_types())
                    .map_err(|error| format!("native trait method ABI is invalid: {error:?}"))?;
            }
        }
        for (owner, declaration) in &self.impls {
            let source = program.impls().get(owner).ok_or("impl owner is absent")?;
            let trait_source = unique_trait_declaration(program, &source.trait_name)?;
            if declaration.owner != *owner
                || declaration.qualified_name != source.qualified_name
                || declaration.trait_owner != trait_source.node_id
                || declaration.trait_name != source.trait_name
                || declaration.type_name != source.type_name
                || declaration.generics.len() != source.generic_parameters.len()
                || source.methods.len() != source.method_owners.len()
                || declaration.methods.len() != source.methods.len()
            {
                return Err("native impl owner, trait or method coverage differs".into());
            }
            let impl_binders = declaration
                .generics
                .iter()
                .map(|generic| (generic.name.clone(), generic.binder.clone()))
                .collect::<Vec<_>>();
            if impl_binders != source.generic_parameters {
                return Err("native impl generic binder identity or order differs".into());
            }
            validate_binders(&declaration.generics, owner, program)?;
            let trait_declaration = self
                .traits
                .get(&declaration.trait_owner)
                .ok_or("native impl trait declaration is absent")?;
            let expected_vtable_slots = trait_declaration
                .slots
                .iter()
                .map(|slot| slot.ordinal)
                .collect::<Vec<_>>();
            if declaration.vtable_slots != expected_vtable_slots
                || declaration.vtable_symbol
                    != native_impl_vtable_symbol(
                        owner,
                        &declaration.trait_owner,
                        &expected_vtable_slots,
                    )
            {
                return Err("native impl vtable identity or slot family differs".into());
            }
            for (index, method) in declaration.methods.iter().enumerate() {
                let method_name = &source.methods[index];
                let method_owner = &source.method_owners[index];
                let signature = program
                    .resolved_signature(method_owner)
                    .ok_or("impl method canonical signature is absent")?;
                let function = program
                    .functions()
                    .get(method_owner)
                    .ok_or("impl method checked function is absent")?;
                let method_binders = method
                    .generics
                    .iter()
                    .map(|generic| (generic.name.clone(), generic.binder.clone()))
                    .collect::<Vec<_>>();
                let expected_method_binders = function
                    .generic_binders
                    .get(impl_binders.len()..)
                    .ok_or("impl method generic binder prefix is absent")?
                    .to_vec();
                let expected_slots = trait_declaration
                    .slots
                    .iter()
                    .filter(|slot| slot.name == *method_name)
                    .map(|slot| slot.ordinal)
                    .collect::<Vec<_>>();
                let expected_ordinal = u32::try_from(index)
                    .map_err(|_| "impl method ordinal exceeds native ABI range")?;
                let generic_identities = declaration
                    .generics
                    .iter()
                    .chain(&method.generics)
                    .map(|generic| generic.binder.clone())
                    .collect::<Vec<_>>();
                let expected_symbol = native_impl_method_symbol(
                    owner,
                    method_owner,
                    expected_ordinal,
                    &generic_identities,
                    &expected_slots,
                );
                if method.name != *method_name
                    || method.ordinal != expected_ordinal
                    || method.callable != *method_owner
                    || method.signature != *signature
                    || method.signature.owner != *method_owner
                    || method_binders != expected_method_binders
                    || method.signature.generic_parameters
                        != function
                            .generic_binders
                            .iter()
                            .map(|(_, binder)| binder.clone())
                            .collect::<Vec<_>>()
                    || method
                        .signature
                        .parameters
                        .first()
                        .is_none_or(|parameter| parameter.name != "self")
                    || method.trait_slots != expected_slots
                    || method.symbol != expected_symbol
                {
                    return Err("native impl method binding or ABI signature differs".into());
                }
                validate_binders(&method.generics, method_owner, program)?;
            }
        }
        for (owner, declaration) in &self.types {
            let source = program
                .type_defs()
                .get(owner)
                .ok_or("type owner is absent")?;
            if declaration.owner != *owner || declaration.name != source.qualified_name {
                return Err("native type owner or name differs from checked identity".into());
            }
            let binders = declaration
                .generics
                .iter()
                .map(|g| (g.name.clone(), g.binder.clone()))
                .collect::<Vec<_>>();
            if binders != source.generic_parameters {
                return Err("native generic binder identity or order differs".into());
            }
            validate_binders(&declaration.generics, owner, program)?;
            match (&declaration.kind, source.kind) {
                (NativeTypeDeclarationKind::Alias { target }, ResolvedTypeKind::Alias)
                | (NativeTypeDeclarationKind::Newtype { target }, ResolvedTypeKind::Newtype) => {
                    if program.resolved_type_target(owner) != Some(target) {
                        return Err(
                            "native alias/newtype target differs from canonical target".into()
                        );
                    }
                }
                (NativeTypeDeclarationKind::Record { fields }, ResolvedTypeKind::Record)
                | (NativeTypeDeclarationKind::Union { fields }, ResolvedTypeKind::Union) => {
                    validate_fields(
                        fields,
                        owner,
                        &source
                            .fields
                            .iter()
                            .map(|f| f.0.as_str())
                            .collect::<Vec<_>>(),
                        &source.field_ids,
                        program,
                    )?;
                }
                (NativeTypeDeclarationKind::Enum { variants }, ResolvedTypeKind::Enum) => {
                    let mut names = source
                        .variants
                        .iter()
                        .map(|v| v.0.as_str())
                        .collect::<Vec<_>>();
                    names.sort();
                    if variants.len() != source.variants.len() {
                        return Err("native variant coverage differs".into());
                    }
                    for (variant, (source_name, _)) in variants.iter().zip(&source.variants) {
                        let expected = source
                            .variant_ids
                            .get(source_name)
                            .ok_or("variant identity absent")?;
                        let schema = program
                            .resolved_variant(expected)
                            .ok_or("canonical variant absent")?;
                        if variant.owner != *owner
                            || variant.variant != *expected
                            || variant.name != *source_name
                            || variant.shape != schema.shape
                            || names.get(variant.native_tag as usize).copied()
                                != Some(source_name.as_str())
                            || variant.members.len() != schema.members.len()
                        {
                            return Err("native variant owner, shape, order or tag differs".into());
                        }
                        for (member, checked) in variant.members.iter().zip(&schema.members) {
                            if member.owner != *expected
                                || member.field != checked.node_id
                                || member.name != checked.name
                                || member.ty != checked.ty
                            {
                                return Err(
                                    "native variant member identity, order or type differs".into(),
                                );
                            }
                        }
                    }
                }
                _ => return Err("native type kind differs from checked declaration".into()),
            }
            if declaration.referenced_declarations
                != referenced_declarations(declaration.type_edges(), program)?
            {
                return Err("native forward-reference dependency graph differs".into());
            }
        }
        for (owner, declaration) in &self.flows {
            let flow = program
                .flows()
                .values()
                .find(|flow| flow.node_id == *owner)
                .ok_or("flow owner absent")?;
            if declaration.owner != *owner
                || declaration.name != flow.id.0
                || declaration.states.len() != flow.states.len()
            {
                return Err("native Flow owner or state coverage differs".into());
            }
            let generic_binders = declaration
                .generics
                .iter()
                .map(|generic| (generic.name.clone(), generic.binder.clone()))
                .collect::<Vec<_>>();
            if generic_binders != flow.generic_parameters {
                return Err("native Flow generic binder identity or order differs".into());
            }
            validate_binders(&declaration.generics, owner, program)?;
            let mut states = BTreeSet::new();
            for state in &declaration.states {
                let source = flow
                    .states
                    .get(&state.name)
                    .ok_or("Flow state name absent")?;
                if state.flow != *owner
                    || state.state != source.node_id
                    || !states.insert(state.state.clone())
                {
                    return Err("native Flow state owner or identity differs".into());
                }
                validate_fields(
                    &state.fields,
                    &state.state,
                    &source
                        .payload
                        .iter()
                        .map(|f| f.0.as_str())
                        .collect::<Vec<_>>(),
                    &source.field_ids,
                    program,
                )?;
            }
            if declaration.transitions.len() != flow.transitions.len() {
                return Err("native Flow transition coverage differs".into());
            }
            for (native, transition_id) in declaration.transitions.iter().zip(&flow.transitions) {
                let source_transition = program
                    .transitions()
                    .get(transition_id)
                    .ok_or("Flow transition identity is absent")?;
                let signature = program
                    .resolved_signature(&source_transition.node_id)
                    .ok_or("Flow transition canonical signature is absent")?;
                let source = flow
                    .states
                    .get(&source_transition.id.source.name)
                    .ok_or("Flow transition source state is absent")?;
                let targets = source_transition
                    .targets
                    .iter()
                    .map(|target| {
                        flow.states
                            .get(&target.name)
                            .map(|state| (state.node_id.clone(), state.id.name.clone()))
                            .ok_or("Flow transition target state is absent")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let expected_failure =
                    canonical_transition_failure_type(&signature.result, program.resolved_types());
                if native.owner != source_transition.node_id
                    || native.event != source_transition.id.event
                    || native.source_state != source.node_id
                    || native.source_name != source.id.name
                    || native.target_states
                        != targets.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()
                    || native.target_names
                        != targets
                            .iter()
                            .map(|(_, name)| name.clone())
                            .collect::<Vec<_>>()
                    || &native.signature != signature
                    || signature.generic_parameters
                        != flow
                            .generic_parameters
                            .iter()
                            .map(|(_, binder)| binder.clone())
                            .collect::<Vec<_>>()
                    || native.is_fallback != source_transition.is_fallback
                    || native.is_ffi_pinned != source_transition.is_ffi_pinned
                    || native.silent_transition != source_transition.silent_transition
                    || native.failure_type != expected_failure
                    || native.failure_type.is_some() != source_transition.fails.is_some()
                {
                    return Err(
                        "native Flow transition identity, ABI signature or flags differ".into(),
                    );
                }
                if signature.parameters.len() != source_transition.params.len() + 1
                    || signature.parameters.first().is_none_or(|parameter| {
                        parameter.name != "self"
                            || parameter.id.0 != source_transition.source_parameter_id
                    })
                {
                    return Err("native Flow transition source parameter ABI differs".into());
                }
                for (index, ((source_name, _), parameter_id)) in source_transition
                    .params
                    .iter()
                    .zip(&source_transition.parameter_ids)
                    .enumerate()
                {
                    let Some(parameter) = signature.parameters.get(index + 1) else {
                        return Err("native Flow transition parameter ABI is incomplete".into());
                    };
                    if parameter.name != *source_name || parameter.id.0 != *parameter_id {
                        return Err(
                            "native Flow transition parameter identity or order differs".into()
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Materialize the string-keyed lookup shape used by the compatibility
    /// Flow emitter from canonical transition declarations. Its ABI checks
    /// consume the typed signature; the strings here are lookup/display views.
    pub fn build_transition_tables(
        &self,
        types: &ResolvedTypeTable,
    ) -> crate::core::TransitionTables {
        let mut resolved = std::collections::HashMap::new();
        let mut fallbacks = std::collections::HashSet::new();
        let mut pinned = std::collections::HashSet::new();
        let mut param_arity = std::collections::HashMap::new();
        let mut param_lists = std::collections::HashMap::new();
        let mut by_flow = std::collections::HashMap::new();
        let mut by_event = std::collections::HashMap::new();
        for owner in &self.flow_order {
            let Some(flow) = self.flows.get(owner) else {
                continue;
            };
            for transition in &flow.transitions {
                let key = (
                    flow.name.clone(),
                    transition.event.clone(),
                    transition.source_name.clone(),
                );
                let arity = transition.signature.parameters.len().saturating_sub(1);
                let targets = transition.target_names.clone();
                if transition.is_fallback {
                    fallbacks.insert(key.clone());
                }
                if transition.is_ffi_pinned {
                    pinned.insert(key.clone());
                }
                param_arity.insert(key.clone(), arity);
                param_lists.insert(
                    key.clone(),
                    transition
                        .signature
                        .parameters
                        .iter()
                        .skip(1)
                        .map(|parameter| {
                            (
                                parameter.name.clone(),
                                native_type_display_name(&parameter.ty, self, types),
                            )
                        })
                        .collect(),
                );
                resolved.insert(key.clone(), targets.clone());
                let targets_joined = targets.join("|");
                by_flow
                    .entry(flow.name.clone())
                    .or_insert_with(Vec::new)
                    .push((
                        transition.event.clone(),
                        transition.source_name.clone(),
                        targets_joined.clone(),
                        transition.is_fallback,
                        transition.is_ffi_pinned,
                        arity,
                    ));
                by_event
                    .entry(transition.event.clone())
                    .or_insert_with(Vec::new)
                    .push((
                        flow.name.clone(),
                        transition.source_name.clone(),
                        targets_joined,
                        transition.is_fallback,
                        transition.is_ffi_pinned,
                        arity,
                    ));
            }
        }
        for rows in by_flow.values_mut() {
            rows.sort();
        }
        for rows in by_event.values_mut() {
            rows.sort();
        }
        crate::core::TransitionTables {
            resolved,
            fallbacks,
            pinned,
            param_arity,
            param_lists,
            by_flow,
            by_event,
        }
    }
}

fn unique_trait_declaration<'a>(
    program: &'a CheckedProgram,
    name: &str,
) -> Result<&'a ResolvedTrait, String> {
    let mut matches = program
        .traits()
        .values()
        .filter(|trait_declaration| trait_declaration.qualified_name == name);
    let Some(declaration) = matches.next() else {
        return Err("impl trait owner is absent".into());
    };
    if matches.next().is_some() {
        return Err("impl trait name resolves to multiple checked owners".into());
    }
    Ok(declaration)
}

fn canonical_transition_failure_type(
    result: &ResolvedTypeId,
    types: &ResolvedTypeTable,
) -> Option<ResolvedTypeId> {
    let ResolvedType::Result { error, .. } = types.get(result)? else {
        return None;
    };
    let ResolvedType::Tuple(parts) = types.get(error)? else {
        return None;
    };
    (parts.len() == 2).then(|| parts[1].clone())
}

fn native_type_display_name(
    id: &ResolvedTypeId,
    declarations: &ResolvedNativeDeclarations,
    types: &ResolvedTypeTable,
) -> String {
    use super::{FunctionTypeAbi, OwnershipTypeKind, ResolvedType, TraitTypeKind};
    match types.get(id) {
        Some(ResolvedType::Primitive(primitive)) => primitive.language_name().to_owned(),
        Some(ResolvedType::GenericParameter(binder)) => declarations
            .flows
            .values()
            .flat_map(|flow| flow.generics.iter())
            .find(|generic| &generic.binder == binder)
            .map(|generic| generic.name.clone())
            .unwrap_or_else(|| "unknown".to_owned()),
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) => {
            let identity = item.as_str();
            let owner = NodeId(identity.to_owned());
            let name = declarations
                .types
                .get(&owner)
                .map(|declaration| declaration.name.clone())
                .unwrap_or_else(|| {
                    identity
                        .strip_prefix("builtin:type:")
                        .or_else(|| identity.strip_prefix("type:"))
                        .or_else(|| identity.strip_prefix("actor:"))
                        .or_else(|| identity.strip_prefix("state:"))
                        .unwrap_or(identity)
                        .rsplit("::")
                        .next()
                        .unwrap_or(identity)
                        .to_owned()
                });
            if arguments.is_empty() {
                name
            } else {
                format!(
                    "{}<{}>",
                    name,
                    arguments
                        .iter()
                        .map(|argument| native_type_display_name(argument, declarations, types))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        Some(ResolvedType::FlowStateSet { flow, states }) => format!(
            "{}<{}>",
            flow.as_str(),
            states
                .iter()
                .map(|state| state.as_str().to_owned())
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        Some(ResolvedType::Reference {
            lifetime,
            mutable,
            target,
        }) => {
            let prefix = if *mutable { "&mut" } else { "&" };
            match lifetime {
                Some(lifetime) => format!(
                    "{}'{} {}",
                    prefix,
                    lifetime,
                    native_type_display_name(target, declarations, types)
                ),
                None => format!(
                    "{}{}",
                    prefix,
                    native_type_display_name(target, declarations, types)
                ),
            }
        }
        Some(ResolvedType::Option(inner)) => format!(
            "Option<{}>",
            native_type_display_name(inner, declarations, types)
        ),
        Some(ResolvedType::Result { ok, error }) => format!(
            "Result<{}, {}>",
            native_type_display_name(ok, declarations, types),
            native_type_display_name(error, declarations, types)
        ),
        Some(ResolvedType::Tuple(elements)) => format!(
            "({})",
            elements
                .iter()
                .map(|element| native_type_display_name(element, declarations, types))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(ResolvedType::Function {
            abi,
            parameters,
            result,
        }) => format!(
            "{}fn({}) -> {}",
            if *abi == FunctionTypeAbi::C {
                "extern "
            } else {
                ""
            },
            parameters
                .iter()
                .map(|parameter| native_type_display_name(parameter, declarations, types))
                .collect::<Vec<_>>()
                .join(", "),
            native_type_display_name(result, declarations, types)
        ),
        Some(ResolvedType::CBuffer(inner)) => format!(
            "c_buffer<{}>",
            native_type_display_name(inner, declarations, types)
        ),
        Some(ResolvedType::Capability(capability)) => {
            format!("cap {}", capability.as_str())
        }
        Some(ResolvedType::Ownership { kind, target }) => format!(
            "{} {}",
            match kind {
                OwnershipTypeKind::Shared => "shared",
                OwnershipTypeKind::Weak => "weak",
            },
            native_type_display_name(target, declarations, types)
        ),
        Some(ResolvedType::Newtype { item, inner }) => {
            if matches!(types.get(inner), Some(ResolvedType::Newtype { .. })) {
                format!(
                    "newtype {} {}",
                    item.as_str(),
                    native_type_display_name(inner, declarations, types)
                )
            } else {
                native_type_display_name(inner, declarations, types)
            }
        }
        Some(ResolvedType::Array { element, length }) => format!(
            "[{}; {}]",
            native_type_display_name(element, declarations, types),
            length
        ),
        Some(ResolvedType::Slice(inner)) => {
            format!("[{}]", native_type_display_name(inner, declarations, types))
        }
        Some(ResolvedType::Trait { kind, traits }) => format!(
            "{} {}",
            match kind {
                TraitTypeKind::Opaque => "impl",
                TraitTypeKind::Dynamic => "dyn",
            },
            traits
                .iter()
                .map(|trait_id| trait_id.as_str())
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        Some(ResolvedType::RawPointer { mutable, target }) => format!(
            "*{}{}",
            if *mutable { "mut " } else { "" },
            native_type_display_name(target, declarations, types)
        ),
        Some(ResolvedType::DynamicAny { .. }) => "Any".to_owned(),
        None => "unknown".to_owned(),
    }
}

fn terminal_primitive_alias_target(
    target: &ResolvedTypeId,
    declarations: &ResolvedNativeDeclarations,
    types: &ResolvedTypeTable,
    visited: &mut BTreeSet<NodeId>,
) -> Option<PrimitiveType> {
    match types.get(target)? {
        ResolvedType::Primitive(primitive) => Some(*primitive),
        ResolvedType::Nominal {
            item, arguments, ..
        } if arguments.is_empty() => {
            let owner = NodeId(item.as_str().to_owned());
            if !visited.insert(owner.clone()) {
                return None;
            }
            let declaration = declarations.types.get(&owner)?;
            if !declaration.generics.is_empty() {
                return None;
            }
            match &declaration.kind {
                NativeTypeDeclarationKind::Alias { target } => {
                    terminal_primitive_alias_target(target, declarations, types, visited)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn validate_binders(
    binders: &[NativeGenericBinder],
    owner: &NodeId,
    program: &CheckedProgram,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for binder in binders {
        if binder.owner != *owner
            || !seen.insert(binder.binder.clone())
            || !program.node_meta().contains_key(&binder.binder)
        {
            return Err("native generic binder owner, identity or coverage is invalid".into());
        }
        for bound in &binder.bounds {
            if let Some(trait_owner) = &bound.trait_owner {
                if !program.traits().contains_key(trait_owner) {
                    return Err("native generic bound trait owner absent".into());
                }
            }
        }
    }
    Ok(())
}

fn validate_fields(
    fields: &[NativeFieldDeclaration],
    owner: &NodeId,
    names: &[&str],
    ids: &BTreeMap<String, NodeId>,
    program: &CheckedProgram,
) -> Result<(), String> {
    if fields.len() != names.len() {
        return Err("native field coverage differs".into());
    }
    let mut seen = BTreeSet::new();
    for (field, name) in fields.iter().zip(names) {
        if field.owner != *owner
            || field.name != *name
            || ids.get(*name) != Some(&field.field)
            || !seen.insert(field.field.clone())
            || !program.node_meta().contains_key(&field.field)
            || program.resolved_field_type(&field.field) != Some(&field.ty)
        {
            return Err("native field owner, identity, order or type differs".into());
        }
    }
    Ok(())
}

fn referenced_declarations(
    roots: Vec<&ResolvedTypeId>,
    program: &CheckedProgram,
) -> Result<BTreeSet<NodeId>, String> {
    let mut seen = BTreeSet::new();
    let mut owners = BTreeSet::new();
    let mut pending = roots;
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let ty = program
            .resolved_types()
            .get(id)
            .ok_or("native type edge absent from canonical table")?;
        match ty {
            ResolvedType::Nominal {
                item, arguments, ..
            } => {
                owners.insert(NodeId(item.as_str().into()));
                pending.extend(arguments);
            }
            ResolvedType::Newtype { item, inner } => {
                owners.insert(NodeId(item.as_str().into()));
                pending.push(inner);
            }
            ResolvedType::Capability(item) => {
                owners.insert(NodeId(item.as_str().into()));
            }
            ResolvedType::FlowStateSet { flow, states } => {
                owners.insert(NodeId(flow.as_str().into()));
                owners.extend(states.iter().map(|s| NodeId(s.as_str().into())));
            }
            ResolvedType::Trait { traits, .. } => {
                owners.extend(traits.iter().map(|t| NodeId(t.as_str().into())));
            }
            ResolvedType::Reference { target, .. }
            | ResolvedType::Ownership { target, .. }
            | ResolvedType::RawPointer { target, .. }
            | ResolvedType::Option(target)
            | ResolvedType::CBuffer(target)
            | ResolvedType::Slice(target) => pending.push(target),
            ResolvedType::Result { ok, error } => {
                pending.push(ok);
                pending.push(error);
            }
            ResolvedType::Tuple(members) => pending.extend(members),
            ResolvedType::Function {
                parameters, result, ..
            } => {
                pending.extend(parameters);
                pending.push(result);
            }
            ResolvedType::Array { element, .. } => pending.push(element),
            ResolvedType::Primitive(_)
            | ResolvedType::GenericParameter(_)
            | ResolvedType::DynamicAny { .. } => {}
        }
    }
    Ok(owners)
}

fn export_primitive(
    ty: &ResolvedTypeId,
    program: &CheckedProgram,
    seen: &mut BTreeSet<ResolvedTypeId>,
) -> Option<PrimitiveType> {
    if !seen.insert(ty.clone()) {
        return None;
    }
    match program.resolved_types().get(ty)? {
        ResolvedType::Primitive(primitive) => Some(*primitive),
        ResolvedType::Tuple(elements) if elements.is_empty() => Some(PrimitiveType::Unit),
        ResolvedType::Nominal {
            item, arguments, ..
        } if arguments.is_empty() => program
            .resolved_type_target(&NodeId(item.as_str().to_owned()))
            .and_then(|target| export_primitive(target, program, seen)),
        _ => None,
    }
}

fn export_scalar_kind(
    ty: &ResolvedTypeId,
    program: &CheckedProgram,
    result: bool,
) -> Option<NativeExportScalarKind> {
    let primitive = export_primitive(ty, program, &mut BTreeSet::new());
    match primitive {
        Some(PrimitiveType::I32) => Some(NativeExportScalarKind::I32),
        Some(PrimitiveType::I64) => Some(NativeExportScalarKind::I64),
        Some(PrimitiveType::F32) => Some(NativeExportScalarKind::F32),
        Some(PrimitiveType::F64) => Some(NativeExportScalarKind::F64),
        Some(PrimitiveType::Bool) => Some(NativeExportScalarKind::Bool),
        Some(PrimitiveType::Unit) if result => Some(NativeExportScalarKind::Unit),
        _ => None,
    }
}

fn validate_export_declaration(
    program: &CheckedProgram,
    owner: &NodeId,
    declaration: &NativeExportDeclaration,
) -> Result<(), String> {
    let function = program
        .functions()
        .get(owner)
        .ok_or("native export owner is absent")?;
    let signature = program
        .resolved_signature(owner)
        .ok_or("native export signature is absent")?;
    if declaration.owner != *owner
        || declaration.abi != "C"
        || declaration.public_symbol != function.qualified_name
        || declaration.body_symbol != format!("$mimi.export.body.{}", function.qualified_name)
        || function.extern_abi.as_deref() != Some("C")
        || !function.generics.is_empty()
        || function.is_async
    {
        return Err(format!(
            "native scalar export '{}' disagrees with its checked declaration",
            owner.0
        ));
    }
    if declaration.parameters.len() != signature.parameters.len()
        || declaration
            .parameters
            .iter()
            .zip(&signature.parameters)
            .any(|((ty, kind), parameter)| {
                ty != &parameter.ty
                    || kind.is_unit()
                    || export_scalar_kind(&parameter.ty, program, false) != Some(*kind)
            })
        || declaration.result.0 != signature.result
        || export_scalar_kind(&signature.result, program, true) != Some(declaration.result.1)
    {
        return Err(format!(
            "native scalar export '{}' has a non-canonical parameter/result ABI",
            owner.0
        ));
    }
    Ok(())
}

/// Frontend producer: syntax is consulted only for header facts not yet held
/// by older checked catalogs (attributes, binder kinds/bounds and order).
/// All type/payload facts come from canonical checker IDs. In particular this
/// does not read ResolvedTypeDef.declaration or create any TypeDef/Type value.
pub(crate) fn build_checked_native_declarations(
    file: &crate::ast::File,
    program: &CheckedProgram,
) -> Result<ResolvedNativeDeclarations, Vec<crate::diagnostic::Diagnostic>> {
    use crate::ast::Item;
    let mut catalog = ResolvedNativeDeclarations::default();
    let build = (|| -> Result<(), String> {
        for item in &file.items {
            match item {
                Item::Func(function) if function.extern_abi.is_some() => {
                    let owner = NodeId(format!("function:{}", function.name));
                    let checked = program
                        .functions()
                        .get(&owner)
                        .or_else(|| program.function(&function.name))
                        .ok_or("checked extern export owner absent")?;
                    let signature = program
                        .resolved_signature(&checked.node_id)
                        .ok_or("checked extern export signature absent")?;
                    let mut unsupported = None;
                    if checked.extern_abi.as_deref() != Some("C") {
                        unsupported = Some("ABI is not C".to_string());
                    } else if !checked.generics.is_empty() {
                        unsupported =
                            Some("generic export body is outside scalar wrapper ABI".into());
                    } else if checked.is_async {
                        unsupported =
                            Some("async export body is outside scalar wrapper ABI".into());
                    } else if program
                        .callable(&checked.node_id)
                        .is_some_and(|callable| !callable.contracts.is_empty())
                    {
                        unsupported = Some(
                            "export contracts (requires/ensures) are outside scalar wrapper ABI"
                                .into(),
                        );
                    } else if signature
                        .parameters
                        .iter()
                        .any(|parameter| parameter.permission.is_some())
                    {
                        unsupported =
                            Some("export parameter mode is outside scalar wrapper ABI".into());
                    } else if signature.parameters.iter().any(|parameter| {
                        export_scalar_kind(&parameter.ty, program, false).is_none()
                    }) {
                        unsupported =
                            Some("aggregate export parameter is outside scalar wrapper ABI".into());
                    } else if export_scalar_kind(&signature.result, program, true).is_none() {
                        unsupported =
                            Some("aggregate export result is outside scalar wrapper ABI".into());
                    }
                    if let Some(reason) = unsupported {
                        catalog
                            .unsupported_exports
                            .insert(checked.node_id.clone(), reason);
                    } else {
                        let parameters = signature
                            .parameters
                            .iter()
                            .map(|parameter| {
                                let kind = export_scalar_kind(&parameter.ty, program, false)
                                    .ok_or("scalar export parameter classification disappeared")?;
                                Ok((parameter.ty.clone(), kind))
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        let result = (
                            signature.result.clone(),
                            export_scalar_kind(&signature.result, program, true)
                                .ok_or("scalar export result classification disappeared")?,
                        );
                        let declaration = NativeExportDeclaration {
                            owner: checked.node_id.clone(),
                            public_symbol: checked.qualified_name.clone(),
                            body_symbol: format!("$mimi.export.body.{}", checked.qualified_name),
                            abi: "C".into(),
                            parameters,
                            result,
                        };
                        catalog.export_order.push(checked.node_id.clone());
                        catalog.exports.insert(checked.node_id.clone(), declaration);
                    }
                }
                Item::Type(header) => {
                    let definition = program
                        .type_def(&header.name)
                        .ok_or("checked type header absent")?;
                    let owner = &definition.node_id;
                    let generics = build_binders(
                        &header.generics,
                        &definition.generic_parameters,
                        owner,
                        program,
                    )?;
                    let fields = || {
                        build_fields(
                            owner,
                            definition.fields.iter().map(|f| f.0.as_str()),
                            &definition.field_ids,
                            program,
                        )
                    };
                    let kind = match definition.kind {
                        ResolvedTypeKind::Alias => NativeTypeDeclarationKind::Alias {
                            target: program
                                .resolved_type_target(owner)
                                .ok_or("canonical alias target absent")?
                                .clone(),
                        },
                        ResolvedTypeKind::Newtype => NativeTypeDeclarationKind::Newtype {
                            target: program
                                .resolved_type_target(owner)
                                .ok_or("canonical newtype target absent")?
                                .clone(),
                        },
                        ResolvedTypeKind::Record => {
                            NativeTypeDeclarationKind::Record { fields: fields()? }
                        }
                        ResolvedTypeKind::Union => {
                            NativeTypeDeclarationKind::Union { fields: fields()? }
                        }
                        ResolvedTypeKind::Enum => {
                            let mut tag_names = definition
                                .variants
                                .iter()
                                .map(|v| v.0.as_str())
                                .collect::<Vec<_>>();
                            tag_names.sort();
                            let mut variants = Vec::new();
                            for (name, _) in &definition.variants {
                                let variant = definition
                                    .variant_ids
                                    .get(name)
                                    .ok_or("checked variant identity absent")?;
                                let schema = program
                                    .resolved_variant(variant)
                                    .ok_or("canonical variant schema absent")?;
                                variants.push(NativeVariantDeclaration {
                                    owner: owner.clone(),
                                    variant: variant.clone(),
                                    name: name.clone(),
                                    native_tag: tag_names
                                        .iter()
                                        .position(|n| *n == name)
                                        .ok_or("native tag absent")?
                                        as u64,
                                    shape: schema.shape,
                                    members: schema
                                        .members
                                        .iter()
                                        .map(|m| NativeFieldDeclaration {
                                            owner: variant.clone(),
                                            field: m.node_id.clone(),
                                            name: m.name.clone(),
                                            ty: m.ty.clone(),
                                        })
                                        .collect(),
                                });
                            }
                            NativeTypeDeclarationKind::Enum { variants }
                        }
                    };
                    let mut declaration = NativeTypeDeclaration {
                        owner: owner.clone(),
                        name: definition.qualified_name.clone(),
                        public: header.pub_,
                        representation: NativeRepresentation {
                            repr_c: header
                                .attributes
                                .contains(&crate::ast::TypeAttribute::ReprC),
                            repr_transparent: header
                                .attributes
                                .contains(&crate::ast::TypeAttribute::ReprTransparent),
                        },
                        generics,
                        derives: header.derives.clone(),
                        kind,
                        referenced_declarations: BTreeSet::new(),
                    };
                    declaration.referenced_declarations =
                        referenced_declarations(declaration.type_edges(), program)?;
                    catalog.type_order.push(owner.clone());
                    catalog.types.insert(owner.clone(), declaration);
                }
                Item::Flow(header) => {
                    let flow = program
                        .flows()
                        .values()
                        .find(|f| f.id.0 == header.name)
                        .ok_or("checked Flow header absent")?;
                    let generic_ids = flow.generic_parameters.clone();
                    let generics =
                        build_binders(&header.generics, &generic_ids, &flow.node_id, program)?;
                    let mut states = Vec::new();
                    for state_header in &header.states {
                        let state = flow
                            .states
                            .get(&state_header.name)
                            .ok_or("checked Flow state absent")?;
                        states.push(NativeStateDeclaration {
                            flow: flow.node_id.clone(),
                            state: state.node_id.clone(),
                            name: state.id.name.clone(),
                            fields: build_fields(
                                &state.node_id,
                                state.payload.iter().map(|f| f.0.as_str()),
                                &state.field_ids,
                                program,
                            )?,
                        });
                    }
                    let mut transitions = Vec::new();
                    for transition_id in &flow.transitions {
                        let transition = program
                            .transitions()
                            .get(transition_id)
                            .ok_or("checked Flow transition absent")?;
                        let signature = program
                            .resolved_signature(&transition.node_id)
                            .ok_or("Flow transition canonical signature absent")?
                            .clone();
                        let source = flow
                            .states
                            .get(&transition.id.source.name)
                            .ok_or("Flow transition source state absent")?;
                        let mut target_states = Vec::with_capacity(transition.targets.len());
                        let mut target_names = Vec::with_capacity(transition.targets.len());
                        for target in &transition.targets {
                            let state = flow
                                .states
                                .get(&target.name)
                                .ok_or("Flow transition target state absent")?;
                            target_states.push(state.node_id.clone());
                            target_names.push(state.id.name.clone());
                        }
                        let failure_type = if transition.fails.is_some() {
                            Some(
                                canonical_transition_failure_type(
                                    &signature.result,
                                    program.resolved_types(),
                                )
                                .ok_or("Flow fails signature has no canonical error payload")?,
                            )
                        } else {
                            None
                        };
                        transitions.push(NativeFlowTransitionDeclaration {
                            owner: transition.node_id.clone(),
                            event: transition.id.event.clone(),
                            source_state: source.node_id.clone(),
                            source_name: source.id.name.clone(),
                            target_states,
                            target_names,
                            signature,
                            is_fallback: transition.is_fallback,
                            is_ffi_pinned: transition.is_ffi_pinned,
                            silent_transition: transition.silent_transition,
                            failure_type,
                        });
                    }
                    catalog.flow_order.push(flow.node_id.clone());
                    catalog.flows.insert(
                        flow.node_id.clone(),
                        NativeFlowDeclaration {
                            owner: flow.node_id.clone(),
                            name: flow.id.0.clone(),
                            generics,
                            states,
                            transitions,
                        },
                    );
                }
                _ => {}
            }
        }
        catalog.actor_order = program.actor_descriptors().keys().cloned().collect();
        for owner in &catalog.actor_order {
            let descriptor = program
                .actor_descriptors()
                .get(owner)
                .ok_or("checked Actor descriptor disappeared")?;
            let actor = program
                .actors()
                .get(owner)
                .ok_or("checked Actor declaration disappeared")?;
            catalog.actors.insert(
                owner.clone(),
                NativeActorDeclaration {
                    owner: owner.clone(),
                    qualified_name: actor.qualified_name.clone(),
                    descriptor: descriptor.clone(),
                },
            );
        }
        catalog.trait_order = program.traits().keys().cloned().collect();
        catalog.trait_order.sort();
        for owner in &catalog.trait_order {
            let source = program
                .traits()
                .get(owner)
                .ok_or("checked trait declaration disappeared")?;
            if source.methods.len() != source.method_owners.len() {
                return Err("checked trait method owner coverage differs".into());
            }
            let slots = source
                .methods
                .iter()
                .zip(&source.method_owners)
                .enumerate()
                .map(|(ordinal, (name, method_owner))| {
                    Ok(NativeTraitSlotDeclaration {
                        ordinal: u32::try_from(ordinal)
                            .map_err(|_| "trait method slot exceeds native ABI range")?,
                        name: name.clone(),
                        method_owner: method_owner.clone(),
                        signature: program
                            .resolved_trait_signature(method_owner)
                            .ok_or("checked trait method canonical signature absent")?
                            .clone(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            catalog.traits.insert(
                owner.clone(),
                NativeTraitDeclaration {
                    owner: owner.clone(),
                    qualified_name: source.qualified_name.clone(),
                    slots,
                },
            );
        }
        // Preserve merged source order here because the legacy owner uses
        // last-write-wins for multiple generic trait instantiations sharing a
        // (type, trait) vtable symbol. The exact impl identities remain in the
        // map; consumers that emit that compatibility symbol must collapse in
        // this recorded order.
        for item in &file.items {
            let Item::Impl(header) = item else {
                continue;
            };
            let qualified = crate::core::resolved::impl_qualified_name(
                &header.trait_name,
                &header.trait_args,
                &header.type_name,
            );
            let owner = program
                .impls()
                .values()
                .find(|implementation| implementation.qualified_name == qualified)
                .map(|implementation| implementation.node_id.clone())
                .ok_or("checked impl source order identity disappeared")?;
            if catalog.impl_order.contains(&owner) {
                return Err("duplicate checked impl identity in source order".into());
            }
            catalog.impl_order.push(owner);
        }
        if catalog.impl_order.len() != program.impls().len() {
            return Err("checked impl source order coverage differs".into());
        }
        for owner in &catalog.impl_order {
            let source = program
                .impls()
                .get(owner)
                .ok_or("checked impl declaration disappeared")?;
            let trait_declaration = unique_trait_declaration(program, &source.trait_name)?;
            if source.methods.len() != source.method_owners.len() {
                return Err("checked impl method owner coverage differs".into());
            }
            let generic_binders =
                build_binders(&source.generics, &source.generic_parameters, owner, program)?;
            validate_binders(&generic_binders, owner, program)?;
            let vtable_slots = trait_declaration
                .methods
                .iter()
                .enumerate()
                .map(|(ordinal, _)| {
                    u32::try_from(ordinal).map_err(|_| "trait method slot exceeds native ABI range")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let vtable_symbol =
                native_impl_vtable_symbol(owner, &trait_declaration.node_id, &vtable_slots);
            let mut methods = Vec::with_capacity(source.methods.len());
            for (ordinal, (name, callable)) in
                source.methods.iter().zip(&source.method_owners).enumerate()
            {
                let signature = program
                    .resolved_signature(callable)
                    .ok_or("checked impl method signature disappeared")?
                    .clone();
                let function = program
                    .functions()
                    .get(callable)
                    .ok_or("checked impl method function disappeared")?;
                if function.generic_binders.len() != generic_binders.len() + function.generics.len()
                    || function.generic_binders[..generic_binders.len()]
                        != source.generic_parameters
                {
                    return Err("checked impl method generic binder inheritance differs".into());
                }
                let method_binders = build_binders(
                    &function.generics,
                    &function.generic_binders[generic_binders.len()..],
                    callable,
                    program,
                )?;
                validate_binders(&method_binders, callable, program)?;
                if signature.generic_parameters
                    != function
                        .generic_binders
                        .iter()
                        .map(|(_, binder)| binder.clone())
                        .collect::<Vec<_>>()
                {
                    return Err("checked impl signature generic binder order differs".into());
                }
                let trait_slots = trait_declaration
                    .methods
                    .iter()
                    .enumerate()
                    .filter(|(_, trait_method)| *trait_method == name)
                    .map(|(ordinal, _)| {
                        u32::try_from(ordinal)
                            .map_err(|_| "trait method slot exceeds native ABI range")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let ordinal = u32::try_from(ordinal)
                    .map_err(|_| "impl method ordinal exceeds native ABI range")?;
                let generic_identities = generic_binders
                    .iter()
                    .chain(&method_binders)
                    .map(|generic| generic.binder.clone())
                    .collect::<Vec<_>>();
                let symbol = native_impl_method_symbol(
                    owner,
                    callable,
                    ordinal,
                    &generic_identities,
                    &trait_slots,
                );
                methods.push(NativeImplMethodDeclaration {
                    name: name.clone(),
                    ordinal,
                    callable: callable.clone(),
                    signature,
                    generics: method_binders,
                    symbol,
                    trait_slots,
                });
            }
            catalog.impls.insert(
                owner.clone(),
                NativeImplDeclaration {
                    owner: owner.clone(),
                    qualified_name: source.qualified_name.clone(),
                    trait_owner: trait_declaration.node_id.clone(),
                    trait_name: source.trait_name.clone(),
                    type_name: source.type_name.clone(),
                    generics: generic_binders,
                    vtable_slots,
                    vtable_symbol,
                    methods,
                },
            );
        }
        catalog.validate_structure(program)
    })();
    if let Err(error) = build {
        return Err(vec![crate::diagnostic::Diagnostic::error(
            format!("TOOL-RESOLUTION-001: {error}"),
            crate::span::Span::UNKNOWN,
        )]);
    }
    Ok(catalog)
}

fn node_id_symbol_fragment(id: &NodeId) -> String {
    id.0.as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn native_impl_method_symbol(
    implementation: &NodeId,
    callable: &NodeId,
    ordinal: u32,
    generic_binders: &[NodeId],
    trait_slots: &[u32],
) -> String {
    let generic_ids = generic_binders
        .iter()
        .map(node_id_symbol_fragment)
        .collect::<Vec<_>>()
        .join("_");
    let slots = trait_slots
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("_");
    format!(
        "__mimi_impl_method__impl_{}__callable_{}__ordinal_{}__generics_{}_{}__slots_{}_{}",
        node_id_symbol_fragment(implementation),
        node_id_symbol_fragment(callable),
        ordinal,
        generic_binders.len(),
        generic_ids,
        trait_slots.len(),
        slots,
    )
}

fn native_impl_vtable_symbol(
    implementation: &NodeId,
    trait_owner: &NodeId,
    vtable_slots: &[u32],
) -> String {
    let slots = vtable_slots
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("_");
    format!(
        "__mimi_vtable__impl_{}__trait_{}__slots_{}_{}",
        node_id_symbol_fragment(implementation),
        node_id_symbol_fragment(trait_owner),
        vtable_slots.len(),
        slots,
    )
}

fn build_fields<'a>(
    owner: &NodeId,
    names: impl Iterator<Item = &'a str>,
    ids: &BTreeMap<String, NodeId>,
    program: &CheckedProgram,
) -> Result<Vec<NativeFieldDeclaration>, String> {
    names
        .map(|name| {
            let field = ids.get(name).ok_or("canonical field identity absent")?;
            let ty = program
                .resolved_field_type(field)
                .ok_or("canonical field type absent")?;
            Ok(NativeFieldDeclaration {
                owner: owner.clone(),
                field: field.clone(),
                name: name.into(),
                ty: ty.clone(),
            })
        })
        .collect()
}

fn build_binders(
    headers: &[crate::ast::GenericParam],
    ids: &[(String, NodeId)],
    owner: &NodeId,
    program: &CheckedProgram,
) -> Result<Vec<NativeGenericBinder>, String> {
    if headers.len() != ids.len() {
        return Err("generic header coverage differs".into());
    }
    headers
        .iter()
        .zip(ids)
        .map(|(header, (name, id))| {
            if header.name != *name {
                return Err("generic header order differs".into());
            }
            Ok(NativeGenericBinder {
                owner: owner.clone(),
                binder: id.clone(),
                name: name.clone(),
                kind: match header.kind {
                    crate::ast::GenericKind::Free => NativeGenericKind::Free,
                    crate::ast::GenericKind::Linear => NativeGenericKind::Linear,
                    crate::ast::GenericKind::LinearDrop => NativeGenericKind::LinearDrop,
                },
                bounds: header
                    .bounds
                    .iter()
                    .map(|name| NativeGenericBound {
                        name: name.clone(),
                        trait_owner: program
                            .traits()
                            .values()
                            .find(|t| t.qualified_name == *name)
                            .map(|t| t.node_id.clone()),
                    })
                    .collect(),
            })
        })
        .collect()
}
