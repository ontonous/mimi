//! Backend-independent type descriptors consumed by canonical MIR passes.
//!
//! `ResolvedTypeId` remains the stable identity. `MirTypeDesc` is the first
//! materialized semantic view of that identity: it records ownership and an
//! abstract ABI class without importing LLVM, runtime handles, or bytecode
//! opcodes. Backends may map the ABI class to a physical layout later, but may
//! not re-derive ownership from that layout.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{
    BuiltinId, FunctionTypeAbi, OwnershipTypeKind, PrimitiveType, ResolvedProjection, ResolvedType,
    ResolvedTypeId, ResolvedTypeTable,
};
use crate::core::mir::MirSetOperation;
use crate::core::{CheckedProgram, NodeId, NominalTypeId, ResolvedTypeKind};

pub const MIR_TYPE_DESC_SCHEMA_VERSION: &str = "mimi-mir-type-desc-12";

/// Maximum size of a canonical trap identity/message carried by a MIR
/// terminator.  Trap text is semantic diagnostic data, not an unchecked
/// backend format string; keeping it bounded and control-character-free makes
/// the reference and bytecode representations deterministic.
pub const MIR_TRAP_CODE_MAX_LEN: usize = 128;

/// Validate the backend-independent contract for a canonical `Trap`.
///
/// A trap has no value operand, layout, ABI, ownership transfer, or drop
/// obligation. Its only semantic payload is a stable non-empty diagnostic
/// identity. The bytecode/native representation may format it differently,
/// but it may not accept a malformed identity or invent a value contract.
pub fn validate_trap_code(code: &str) -> Result<(), String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("trap code is empty".into());
    }
    if code.len() > MIR_TRAP_CODE_MAX_LEN {
        return Err(format!("trap code exceeds {} bytes", MIR_TRAP_CODE_MAX_LEN));
    }
    if code.chars().any(char::is_control) {
        return Err("trap code contains a control character".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirOwnership {
    Copy,
    Move,
    Linear,
    SharedBorrow,
    WeakBorrow,
}

impl MirOwnership {
    pub fn needs_drop(self) -> bool {
        matches!(self, Self::Move | Self::Linear)
    }

    pub fn needs_clone(self) -> bool {
        !matches!(self, Self::Copy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirAbiClass {
    Unit,
    Integer {
        bits: u16,
        signed: bool,
    },
    Float {
        bits: u16,
    },
    Bool,
    Char,
    StringHandle,
    /// A move-owned Set runtime handle.  The element contract remains in the
    /// accompanying `MirLayout::Set`; this ABI class is intentionally
    /// distinct from Map and other opaque handles.
    SetHandle,
    OpaqueHandle,
    Pointer,
    Aggregate,
    FunctionPointer,
}

/// Semantic builtin operations that have a first-class canonical MIR node.
/// The enum is intentionally closed: a surface builtin remains a legacy
/// `Call` until its ABI, effect, trap, and ownership contract is materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirBuiltinKind {
    /// Signed i64 / f64 absolute value. i64::MIN traps with E0802.
    Abs,
    /// Signed i64 minimum. Narrower widths and floating-point finiteness are
    /// deliberately outside this first comparison contract.
    Min,
    /// Signed i64 maximum. Narrower widths and floating-point finiteness are
    /// deliberately outside this first comparison contract.
    Max,
    /// Write one Copy boolean followed by a newline. The output effect is
    /// part of the MIR contract so reference, bytecode, and native consumers
    /// cannot silently treat `println` as an ordinary legacy call.
    PrintlnBool,
    /// Write one Copy signed integer followed by a newline. The accepted
    /// width is checker-owned by the argument TypeDesc: signed i32 and i64
    /// are the only concrete integer ABIs in this contract.
    PrintlnInt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirBuiltinEffect {
    Pure,
    StdoutLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirBuiltinContract {
    pub kind: MirBuiltinKind,
    pub name: &'static str,
    pub arity: usize,
    pub input_abi: MirAbiClass,
    pub preserves_type: bool,
    pub requires_copy: bool,
    pub requires_same_input_type: bool,
    pub overflow_trap: Option<&'static str>,
    pub result_must_be_unit: bool,
    pub effect: MirBuiltinEffect,
}

impl MirBuiltinContract {
    pub fn for_kind(kind: MirBuiltinKind) -> Self {
        match kind {
            MirBuiltinKind::Abs => Self {
                kind,
                name: "abs",
                arity: 1,
                input_abi: MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                },
                preserves_type: true,
                requires_copy: true,
                requires_same_input_type: false,
                overflow_trap: Some("E0802"),
                result_must_be_unit: false,
                effect: MirBuiltinEffect::Pure,
            },
            MirBuiltinKind::Min => Self {
                kind,
                name: "min",
                arity: 2,
                input_abi: MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                },
                preserves_type: true,
                requires_copy: true,
                requires_same_input_type: true,
                overflow_trap: None,
                result_must_be_unit: false,
                effect: MirBuiltinEffect::Pure,
            },
            MirBuiltinKind::Max => Self {
                kind,
                name: "max",
                arity: 2,
                input_abi: MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                },
                preserves_type: true,
                requires_copy: true,
                requires_same_input_type: true,
                overflow_trap: None,
                result_must_be_unit: false,
                effect: MirBuiltinEffect::Pure,
            },
            MirBuiltinKind::PrintlnBool => Self {
                kind,
                name: "println",
                arity: 1,
                input_abi: MirAbiClass::Bool,
                preserves_type: false,
                requires_copy: true,
                requires_same_input_type: false,
                overflow_trap: None,
                result_must_be_unit: true,
                effect: MirBuiltinEffect::StdoutLine,
            },
            MirBuiltinKind::PrintlnInt => Self {
                kind,
                name: "println",
                arity: 1,
                input_abi: MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                },
                preserves_type: false,
                requires_copy: true,
                requires_same_input_type: false,
                overflow_trap: None,
                result_must_be_unit: true,
                effect: MirBuiltinEffect::StdoutLine,
            },
        }
    }

    /// Resolve only the surface builtin identities already admitted to the
    /// canonical MIR schema. Unknown identities deliberately return `None` so
    /// the caller can fail closed instead of inventing a backend contract.
    pub fn from_builtin(id: &BuiltinId) -> Option<Self> {
        match id.as_str() {
            "abs" => Some(Self::for_kind(MirBuiltinKind::Abs)),
            "min" => Some(Self::for_kind(MirBuiltinKind::Min)),
            "max" => Some(Self::for_kind(MirBuiltinKind::Max)),
            _ => None,
        }
    }

    /// Resolve a type-directed surface builtin. `println` is deliberately
    /// not resolved by name alone: its canonical node must record the
    /// concrete scalar ABI selected by checker-owned TypeDesc facts.
    pub fn from_builtin_with_abi(id: &BuiltinId, abi: MirAbiClass) -> Option<Self> {
        if id.as_str() == "println" {
            let kind = match abi {
                MirAbiClass::Bool => MirBuiltinKind::PrintlnBool,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                } => MirBuiltinKind::PrintlnInt,
                _ => return None,
            };
            return Some(Self::for_kind(kind));
        }
        Self::from_builtin(id)
    }

    /// The contract admits a second ABI shape for the same polymorphic
    /// operation. Keeping this rule here makes TypeDesc the source of truth
    /// rather than duplicating the accepted widths in each consumer.
    pub fn accepts_abi(self, abi: MirAbiClass) -> bool {
        match self.kind {
            MirBuiltinKind::Abs => matches!(
                abi,
                MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                } | MirAbiClass::Float { bits: 64 }
            ),
            MirBuiltinKind::Min | MirBuiltinKind::Max => matches!(
                abi,
                MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                }
            ),
            MirBuiltinKind::PrintlnBool => abi == MirAbiClass::Bool,
            MirBuiltinKind::PrintlnInt => matches!(
                abi,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                }
            ),
        }
    }

    pub fn accepts_layout(self, layout: &MirLayout) -> bool {
        matches!(layout, MirLayout::Scalar)
            && matches!(
                self.kind,
                MirBuiltinKind::Abs
                    | MirBuiltinKind::Min
                    | MirBuiltinKind::Max
                    | MirBuiltinKind::PrintlnBool
                    | MirBuiltinKind::PrintlnInt
            )
    }

    pub fn accepted_abi_description(self) -> &'static str {
        match self.kind {
            MirBuiltinKind::Abs => "signed i64 or f64",
            MirBuiltinKind::Min | MirBuiltinKind::Max => "signed i64",
            MirBuiltinKind::PrintlnBool => "bool",
            MirBuiltinKind::PrintlnInt => "signed i32 or i64",
        }
    }
}

/// The closed set of conversion shapes currently materialized in canonical
/// MIR.  A surface cast remains a `Convert` node, but it cannot reach a
/// backend until its source/target TypeDesc pair resolves to one of these
/// contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirConversionKind {
    /// Explicitly spell a no-op cast for a Copy scalar without changing its
    /// canonical type identity.
    ScalarIdentity,
    /// Exact signed integer widening.  Runtime scalar values are already
    /// carried in the canonical integer slot, so this has no trap or loss.
    SignedI32ToI64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirConversionContract {
    pub kind: MirConversionKind,
    pub name: &'static str,
    pub requires_scalar: bool,
    pub requires_copy: bool,
}

impl MirConversionContract {
    pub fn for_kind(kind: MirConversionKind) -> Self {
        match kind {
            MirConversionKind::ScalarIdentity => Self {
                kind,
                name: "scalar identity",
                requires_scalar: true,
                requires_copy: true,
            },
            MirConversionKind::SignedI32ToI64 => Self {
                kind,
                name: "signed i32 to signed i64 widening",
                requires_scalar: true,
                requires_copy: true,
            },
        }
    }

    /// Resolve a pair of checker-owned TypeDesc values to the closed
    /// conversion family.  No backend may add another ABI pair locally.
    pub fn for_descriptors(source: &MirTypeDesc, target: &MirTypeDesc) -> Option<Self> {
        [
            MirConversionKind::ScalarIdentity,
            MirConversionKind::SignedI32ToI64,
        ]
        .into_iter()
        .map(Self::for_kind)
        .find(|contract| contract.accepts(source, target))
    }

    pub fn accepts(self, source: &MirTypeDesc, target: &MirTypeDesc) -> bool {
        let layout_ok = !self.requires_scalar
            || (matches!(&source.layout, MirLayout::Scalar)
                && matches!(&target.layout, MirLayout::Scalar));
        let ownership_ok = !self.requires_copy
            || (source.ownership == MirOwnership::Copy && target.ownership == MirOwnership::Copy);
        if !layout_ok || !ownership_ok {
            return false;
        }
        match self.kind {
            MirConversionKind::ScalarIdentity => source.id == target.id && source.abi == target.abi,
            MirConversionKind::SignedI32ToI64 => {
                matches!(
                    source.abi,
                    MirAbiClass::Integer {
                        bits: 32,
                        signed: true
                    }
                ) && matches!(
                    target.abi,
                    MirAbiClass::Integer {
                        bits: 64,
                        signed: true
                    }
                )
            }
        }
    }

    pub fn accepted_description() -> &'static str {
        "same Copy scalar type or signed i32 to signed i64"
    }
}

/// Backend-independent implementation selected for one ownership boundary.
///
/// `OwnedString` and `List` are semantic contracts, not VM/LLVM
/// representations: every consumer must implement the same retain/release/
/// transfer behavior for the corresponding owned Mimi value. `Aggregate` is
/// reserved for a recursively materialized product contract. `Unsupported`
/// is deliberately explicit so a backend cannot turn an unmodelled aggregate
/// into an accidental shallow copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirGlueKind {
    Noop,
    OwnedString,
    List,
    Set,
    Aggregate,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirGlueOperation {
    MoveOut,
    Clone,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirGlueContract {
    pub move_out: MirGlueKind,
    pub clone: MirGlueKind,
    pub drop: MirGlueKind,
}

/// Canonical field-level drop schedule for an aggregate product.
///
/// The schedule is stored in destruction order (reverse declaration order),
/// while `index` remains the declaration slot used by the semantic layout.
/// Nested aggregate fields refer back to their child `MirTypeDesc`, so the
/// complete recursive schedule is carried by the TypeDesc graph rather than
/// reconstructed by a backend from a physical tuple representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirDropGluePlan {
    pub fields: Vec<MirDropGlueField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirDropGlueField {
    pub index: usize,
    pub ty: ResolvedTypeId,
    pub glue: MirGlueKind,
}

/// Canonical drop schedule for one Option/Result variant payload.  Unlike a
/// product drop plan, a variant has a runtime-selected payload shape, so the
/// active variant identity is part of the schedule key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirVariantDropGluePlan {
    pub variant: NodeId,
    pub fields: Vec<MirDropGlueField>,
}

impl MirGlueContract {
    fn for_type(kind: &MirTypeKind, ownership: MirOwnership) -> Self {
        if ownership == MirOwnership::Copy {
            return Self {
                move_out: MirGlueKind::Noop,
                clone: MirGlueKind::Noop,
                drop: MirGlueKind::Noop,
            };
        }
        if matches!(kind, MirTypeKind::Primitive(PrimitiveType::String))
            && ownership == MirOwnership::Move
        {
            return Self {
                move_out: MirGlueKind::OwnedString,
                clone: MirGlueKind::OwnedString,
                drop: MirGlueKind::OwnedString,
            };
        }
        if matches!(kind, MirTypeKind::List) && ownership == MirOwnership::Move {
            return Self {
                move_out: MirGlueKind::List,
                clone: MirGlueKind::List,
                drop: MirGlueKind::List,
            };
        }
        if matches!(kind, MirTypeKind::Set) && ownership == MirOwnership::Move {
            return Self {
                move_out: MirGlueKind::Set,
                clone: MirGlueKind::Set,
                drop: MirGlueKind::Set,
            };
        }
        Self {
            move_out: MirGlueKind::Unsupported,
            clone: MirGlueKind::Unsupported,
            drop: MirGlueKind::Unsupported,
        }
    }

    pub fn supports_move_out(self) -> bool {
        self.move_out != MirGlueKind::Unsupported
    }

    pub fn supports_clone(self) -> bool {
        self.clone != MirGlueKind::Unsupported
    }

    pub fn supports_drop(self) -> bool {
        self.drop != MirGlueKind::Unsupported
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirTypeKind {
    Primitive(PrimitiveType),
    GenericParameter,
    Nominal,
    List,
    /// A parameterized, move-owned Set whose element is a concrete Copy
    /// scalar in the currently materialized production island.
    Set,
    FlowStateSet,
    Reference {
        mutable: bool,
    },
    Option,
    Result,
    Tuple {
        arity: usize,
    },
    Function {
        abi: FunctionTypeAbi,
        arity: usize,
    },
    CBuffer,
    Capability,
    Ownership(OwnershipTypeKind),
    Newtype,
    Array {
        length: usize,
    },
    Slice,
    Trait,
    RawPointer {
        mutable: bool,
    },
    DynamicAny,
}

/// Backend-independent semantic layout.  This is deliberately not a byte
/// offset/size description: target ABI lowering owns those physical details,
/// while every consumer must agree on the aggregate shape and its canonical
/// field identities first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirLayout {
    Unit,
    Scalar,
    Handle,
    /// Pointer-shaped storage with an optional checker-owned target. Raw
    /// pointer-like types carry their pointee here; opaque pointer families
    /// retain `None` until their target contract is materialized.
    Pointer {
        target: Option<ResolvedTypeId>,
    },
    Tuple(Vec<ResolvedTypeId>),
    Option {
        inner: ResolvedTypeId,
        variants: Vec<MirVariantDesc>,
    },
    Result {
        ok: ResolvedTypeId,
        error: ResolvedTypeId,
        variants: Vec<MirVariantDesc>,
    },
    Array {
        element: ResolvedTypeId,
        length: usize,
    },
    Newtype {
        nominal: crate::core::NominalTypeId,
        inner: ResolvedTypeId,
    },
    Record {
        nominal: crate::core::NominalTypeId,
        fields: Vec<MirFieldDesc>,
    },
    /// Variable-length List storage. The physical representation remains
    /// backend-owned, but the element identity and ownership contract are
    /// canonical MIR facts. The first production slice admits only a
    /// concrete Copy scalar element.
    List {
        element: ResolvedTypeId,
    },
    /// Variable-length Set storage.  The runtime uses an opaque i64 handle;
    /// the element identity is nevertheless retained here so construction,
    /// operation arguments, and glue are checked before any backend.
    Set {
        element: ResolvedTypeId,
    },
    Opaque,
}

/// Canonical field contract used by aggregate lowering.  The declaration
/// order is preserved, while the field identity and type are checker-owned
/// values; no backend may recover either from a surface AST or a native struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirFieldDesc {
    pub id: NodeId,
    pub name: String,
    pub ty: ResolvedTypeId,
}

/// Canonical discriminant/payload contract for one variant. The discriminant
/// is semantic and stable; bytecode/native encodings may choose a physical
/// representation only after consuming this descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirVariantDesc {
    pub id: NodeId,
    pub name: String,
    pub discriminant: u16,
    pub fields: Vec<MirFieldDesc>,
}

/// Backend-independent receipt for one canonical variant payload projection.
///
/// The nominal/variant/field identities and declaration-order index are
/// resolved together from the TypeDesc graph. Consumers must carry this
/// receipt into their physical read instruction instead of independently
/// deriving a tag, index, or arity from a runtime representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirVariantProjectionContract {
    pub nominal: NominalTypeId,
    pub variant: NodeId,
    pub field: NodeId,
    pub field_index: usize,
    pub arity: usize,
    pub field_ty: ResolvedTypeId,
}

/// Backend-independent receipt for one canonical record field projection.
///
/// The nominal/field identities, runtime field name, declaration-order index,
/// record arity, and field type are resolved together from the TypeDesc graph.
/// A backend may encode the name or index physically, but it must carry the
/// receipt rather than infer record shape from a map, struct, or AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirRecordProjectionContract {
    pub nominal: NominalTypeId,
    pub field: NodeId,
    pub name: String,
    pub field_index: usize,
    pub arity: usize,
    pub field_ty: ResolvedTypeId,
}

/// Backend-independent receipt for one canonical tuple field projection.
///
/// Tuples have structural identity rather than a nominal field ID. The
/// source TypeDesc, declaration-order index, arity, and selected element type
/// therefore travel together so a consumer cannot infer tuple shape from a
/// physical vector or LLVM struct alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirTupleProjectionContract {
    pub tuple_ty: ResolvedTypeId,
    pub field_index: usize,
    pub arity: usize,
    pub field_ty: ResolvedTypeId,
}

/// Backend-independent receipt for one canonical read-only List index
/// projection.  The source List, element, index operand, and result identity
/// are checker-owned facts; consumers must not recover them from a runtime
/// vector, handle, or backend ABI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirListIndexProjectionContract {
    pub list_ty: ResolvedTypeId,
    pub element_ty: ResolvedTypeId,
    pub index_ty: ResolvedTypeId,
    pub result_ty: ResolvedTypeId,
}

/// Backend-independent receipt for one canonical read-only List operation.
/// The operation, receiver identity, and result identity are checker-owned
/// facts; consumers must not rediscover them from a List handle or scalar ABI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirListOperationContract {
    pub list_ty: ResolvedTypeId,
    pub element_ty: ResolvedTypeId,
    pub result_ty: ResolvedTypeId,
    /// The second List identity for `Concat`; absent for `Len`/`Reverse`.
    pub argument_ty: Option<ResolvedTypeId>,
    pub operation: crate::core::mir::MirListOperation,
}

/// Backend-independent receipt for a read-only Option/Result predicate.
/// `variant` and `discriminant` are redundant by design: the checker-owned
/// identity and the physical tag must agree at every consumer boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirVariantPredicateContract {
    pub variant_ty: ResolvedTypeId,
    pub result_ty: ResolvedTypeId,
    pub nominal: NominalTypeId,
    pub variant: NodeId,
    pub variant_name: String,
    pub alternate_variant: NodeId,
    pub alternate_variant_name: String,
    pub predicate: crate::core::mir::MirVariantPredicate,
    pub discriminant: u16,
}

/// The ownership/ABI profile proved by a direct variant call receipt.
///
/// The profile is part of the receipt rather than a backend-local decision:
/// a move-owned aggregate must not be silently treated as a Copy return when
/// a consumer lowers the same MIR Call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirVariantCallAbiMode {
    FlatCopy,
    MoveOwned,
}

/// The return-path merge proof attached to a direct variant-call receipt.
///
/// This is intentionally separate from [`MirVariantCallAbiMode`]: ABI and
/// ownership describe the value crossing the call boundary, while this field
/// describes how a callee with multiple canonical return paths may produce
/// that value.  Consumers must use the checker-owned proof instead of
/// deciding from an LLVM aggregate or a bytecode handle whether merging is
/// safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirVariantCallReturnMode {
    FlatCopyMerge,
    OwnershipPathExclusiveMerge,
}

/// Backend-independent ABI receipt for a direct call whose result is an
/// admitted Option/Result shape. The callee signature and complete variant
/// table travel with the call so a consumer cannot infer aggregate ABI or
/// ownership from an LLVM struct, a bytecode value, or a runtime tag.
/// `payload_ty` is retained as the first payload for compatibility with the
/// original flat contract; `payload_types` records every physical/semantic
/// payload type in the promoted profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirVariantCallAbiContract {
    pub callee: NodeId,
    pub type_arguments: Vec<ResolvedTypeId>,
    pub parameter_types: Vec<ResolvedTypeId>,
    pub result_ty: ResolvedTypeId,
    pub mode: MirVariantCallAbiMode,
    pub return_mode: MirVariantCallReturnMode,
    pub payload_ty: ResolvedTypeId,
    pub payload_types: Vec<ResolvedTypeId>,
    pub nominal: NominalTypeId,
    pub variants: Vec<MirVariantCallVariant>,
}

/// One variant entry in [`MirVariantCallAbiContract`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirVariantCallVariant {
    pub id: NodeId,
    pub name: String,
    pub discriminant: u16,
    pub payload_field: Option<NodeId>,
    pub payload_arity: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirTypeDesc {
    pub id: ResolvedTypeId,
    pub kind: MirTypeKind,
    pub layout: MirLayout,
    pub ownership: MirOwnership,
    pub abi: MirAbiClass,
    pub needs_drop_glue: bool,
    pub needs_clone_glue: bool,
    pub glue: MirGlueContract,
    pub drop_plan: Option<MirDropGluePlan>,
    pub variant_drop_plan: Option<Vec<MirVariantDropGluePlan>>,
}

impl MirTypeDesc {
    fn from_resolved(id: &ResolvedTypeId, ty: &ResolvedType, ownership: MirOwnership) -> Self {
        let (kind, abi, layout) = match ty {
            ResolvedType::Primitive(primitive) => (
                MirTypeKind::Primitive(*primitive),
                primitive_abi(*primitive),
                primitive_layout(*primitive),
            ),
            ResolvedType::GenericParameter(_) => (
                MirTypeKind::GenericParameter,
                MirAbiClass::OpaqueHandle,
                MirLayout::Opaque,
            ),
            ResolvedType::Nominal {
                item, arguments, ..
            } if item.as_str() == "builtin:type:List" && arguments.len() == 1 => (
                MirTypeKind::List,
                MirAbiClass::OpaqueHandle,
                MirLayout::List {
                    element: arguments[0].clone(),
                },
            ),
            ResolvedType::Nominal {
                item, arguments, ..
            } if item.as_str() == "builtin:type:Set" && arguments.len() == 1 => (
                MirTypeKind::Set,
                MirAbiClass::SetHandle,
                MirLayout::Set {
                    element: arguments[0].clone(),
                },
            ),
            ResolvedType::Nominal { .. } => (
                MirTypeKind::Nominal,
                MirAbiClass::OpaqueHandle,
                MirLayout::Opaque,
            ),
            ResolvedType::FlowStateSet { .. } => (
                MirTypeKind::FlowStateSet,
                MirAbiClass::OpaqueHandle,
                MirLayout::Handle,
            ),
            ResolvedType::Reference {
                lifetime: _,
                mutable,
                target,
            } => (
                MirTypeKind::Reference { mutable: *mutable },
                MirAbiClass::Pointer,
                MirLayout::Pointer {
                    target: Some(target.clone()),
                },
            ),
            ResolvedType::Option(inner) => (
                MirTypeKind::Option,
                MirAbiClass::Aggregate,
                MirLayout::Option {
                    inner: inner.clone(),
                    variants: option_variants(inner),
                },
            ),
            ResolvedType::Result { ok, error } => (
                MirTypeKind::Result,
                MirAbiClass::Aggregate,
                MirLayout::Result {
                    ok: ok.clone(),
                    error: error.clone(),
                    variants: result_variants(ok, error),
                },
            ),
            ResolvedType::Tuple(elements) => (
                MirTypeKind::Tuple {
                    arity: elements.len(),
                },
                MirAbiClass::Aggregate,
                MirLayout::Tuple(elements.clone()),
            ),
            ResolvedType::Function {
                abi, parameters, ..
            } => (
                MirTypeKind::Function {
                    abi: *abi,
                    arity: parameters.len(),
                },
                MirAbiClass::FunctionPointer,
                MirLayout::Handle,
            ),
            ResolvedType::CBuffer(target) => (
                MirTypeKind::CBuffer,
                MirAbiClass::Pointer,
                MirLayout::Pointer {
                    target: Some(target.clone()),
                },
            ),
            ResolvedType::Capability(_) => (
                MirTypeKind::Capability,
                MirAbiClass::OpaqueHandle,
                MirLayout::Handle,
            ),
            ResolvedType::Ownership { kind, .. } => (
                MirTypeKind::Ownership(*kind),
                MirAbiClass::OpaqueHandle,
                MirLayout::Handle,
            ),
            ResolvedType::Newtype { item, inner } => (
                MirTypeKind::Newtype,
                MirAbiClass::Aggregate,
                MirLayout::Newtype {
                    nominal: item.clone(),
                    inner: inner.clone(),
                },
            ),
            ResolvedType::Array { element, length } => (
                MirTypeKind::Array { length: *length },
                MirAbiClass::Aggregate,
                MirLayout::Array {
                    element: element.clone(),
                    length: *length,
                },
            ),
            ResolvedType::Slice(target) => (
                MirTypeKind::Slice,
                MirAbiClass::Pointer,
                MirLayout::Pointer {
                    target: Some(target.clone()),
                },
            ),
            ResolvedType::Trait { .. } => (
                MirTypeKind::Trait,
                MirAbiClass::OpaqueHandle,
                MirLayout::Opaque,
            ),
            ResolvedType::RawPointer { mutable, target } => (
                MirTypeKind::RawPointer { mutable: *mutable },
                MirAbiClass::Pointer,
                MirLayout::Pointer {
                    target: Some(target.clone()),
                },
            ),
            ResolvedType::DynamicAny { .. } => (
                MirTypeKind::DynamicAny,
                MirAbiClass::OpaqueHandle,
                MirLayout::Opaque,
            ),
        };
        let glue = MirGlueContract::for_type(&kind, ownership);
        Self {
            id: id.clone(),
            kind,
            layout,
            ownership,
            abi,
            needs_drop_glue: ownership.needs_drop(),
            needs_clone_glue: ownership.needs_clone(),
            glue,
            drop_plan: None,
            variant_drop_plan: None,
        }
    }
}

fn option_variants(inner: &ResolvedTypeId) -> Vec<MirVariantDesc> {
    vec![
        MirVariantDesc {
            id: NodeId("builtin:variant:Option::None".into()),
            name: "None".into(),
            discriminant: 0,
            fields: Vec::new(),
        },
        MirVariantDesc {
            id: NodeId("builtin:variant:Option::Some".into()),
            name: "Some".into(),
            discriminant: 1,
            fields: vec![MirFieldDesc {
                id: NodeId("builtin:variant:Option::Some/payload:0".into()),
                name: "_0".into(),
                ty: inner.clone(),
            }],
        },
    ]
}

fn result_variants(ok: &ResolvedTypeId, error: &ResolvedTypeId) -> Vec<MirVariantDesc> {
    vec![
        MirVariantDesc {
            id: NodeId("builtin:variant:Result::Ok".into()),
            name: "Ok".into(),
            discriminant: 0,
            fields: vec![MirFieldDesc {
                id: NodeId("builtin:variant:Result::Ok/payload:0".into()),
                name: "_0".into(),
                ty: ok.clone(),
            }],
        },
        MirVariantDesc {
            id: NodeId("builtin:variant:Result::Err".into()),
            name: "Err".into(),
            discriminant: 1,
            fields: vec![MirFieldDesc {
                id: NodeId("builtin:variant:Result::Err/payload:0".into()),
                name: "_0".into(),
                ty: error.clone(),
            }],
        },
    ]
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MirTypeCatalog {
    entries: BTreeMap<ResolvedTypeId, MirTypeDesc>,
}

impl MirTypeCatalog {
    pub fn from_resolved_types(table: &ResolvedTypeTable) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();
        if let Err(type_errors) = table.validate() {
            errors.extend(type_errors.into_iter().map(|error| error.to_string()));
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut entries = BTreeMap::new();
        for (id, ty) in table.iter() {
            let ownership = ownership_for(id, table, &mut Vec::new());
            let descriptor = MirTypeDesc::from_resolved(id, ty, ownership);
            entries.insert(id.clone(), descriptor);
        }
        let mut catalog = Self { entries };
        catalog.materialize_product_glue();
        Ok(catalog)
    }

    /// Build the catalog from the checker-owned program and attach record
    /// field contracts while the resolved declaration snapshot is still
    /// available.  A backend never needs to reopen `CheckedProgram` after
    /// this point.
    pub fn from_checked_program(program: &CheckedProgram) -> Result<Self, Vec<String>> {
        let mut catalog = Self::from_resolved_types(program.resolved_types())?;
        let mut errors = Vec::new();
        for (id, ty) in program.resolved_types().iter() {
            let ResolvedType::Nominal { item, .. } = ty else {
                continue;
            };
            let Some(type_def) = program.type_def(item.as_str()).or_else(|| {
                item.as_str()
                    .strip_prefix("type:")
                    .and_then(|name| program.type_def(name))
            }) else {
                continue;
            };
            if !matches!(type_def.kind, ResolvedTypeKind::Record) {
                continue;
            }
            let mut fields = Vec::with_capacity(type_def.fields.len());
            for (name, _) in &type_def.fields {
                let Some(field_id) = type_def.field_ids.get(name) else {
                    errors.push(format!(
                        "record '{}' field '{}' has no stable declaration identity",
                        type_def.qualified_name, name
                    ));
                    continue;
                };
                let Some(field_ty) = program.resolved_field_type(field_id) else {
                    errors.push(format!(
                        "record '{}' field '{}' has no resolved type",
                        type_def.qualified_name, name
                    ));
                    continue;
                };
                if catalog.get(field_ty).is_none() {
                    errors.push(format!(
                        "record '{}' field '{}' references a type absent from MIR catalog",
                        type_def.qualified_name, name
                    ));
                }
                fields.push(MirFieldDesc {
                    id: field_id.clone(),
                    name: name.clone(),
                    ty: field_ty.clone(),
                });
            }
            let ownership = fields.iter().fold(MirOwnership::Copy, |current, field| {
                let field_ownership = catalog
                    .get(&field.ty)
                    .map(|field| field.ownership)
                    .unwrap_or(MirOwnership::Move);
                combine_ownership(current, field_ownership)
            });
            if let Some(descriptor) = catalog.entries.get_mut(id) {
                descriptor.abi = MirAbiClass::Aggregate;
                descriptor.ownership = ownership;
                descriptor.needs_drop_glue = ownership.needs_drop();
                descriptor.needs_clone_glue = ownership.needs_clone();
                descriptor.glue = MirGlueContract::for_type(&descriptor.kind, ownership);
                descriptor.layout = MirLayout::Record {
                    nominal: item.clone(),
                    fields,
                };
            }
        }
        // Flow states are checker-owned nominal records, but they are not
        // ordinary `type` declarations. Materialize their payload layout here
        // so every consumer sees the same field identities, ABI class, and
        // linear aggregate glue. In particular, a state must never remain an
        // opaque handle merely because it came from a Flow declaration.
        let mut states = program
            .flows()
            .values()
            .flat_map(|flow| flow.states.values())
            .collect::<Vec<_>>();
        states.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        for state in states {
            let state_nominal = crate::core::NominalTypeId::new(state.node_id.0.clone())
                .map_err(|error| vec![error.to_string()])?;
            let Some(state_type_id) = program
                .resolved_types()
                .iter()
                .find_map(|(id, ty)| match ty {
                    ResolvedType::Nominal { item, .. } if *item == state_nominal => Some(id),
                    _ => None,
                })
                .cloned()
            else {
                errors.push(format!(
                    "flow state '{}' has no canonical nominal type identity",
                    state.node_id.0
                ));
                continue;
            };
            let mut fields = Vec::with_capacity(state.payload.len());
            for (name, _) in &state.payload {
                let Some(field_id) = state.field_ids.get(name) else {
                    errors.push(format!(
                        "flow state '{}' field '{}' has no stable declaration identity",
                        state.node_id.0, name
                    ));
                    continue;
                };
                let Some(field_ty) = program.resolved_field_type(field_id) else {
                    errors.push(format!(
                        "flow state '{}' field '{}' has no resolved type",
                        state.node_id.0, name
                    ));
                    continue;
                };
                if catalog.get(field_ty).is_none() {
                    errors.push(format!(
                        "flow state '{}' field '{}' references a type absent from MIR catalog",
                        state.node_id.0, name
                    ));
                }
                fields.push(MirFieldDesc {
                    id: field_id.clone(),
                    name: name.clone(),
                    ty: field_ty.clone(),
                });
            }
            if let Some(descriptor) = catalog.entries.get_mut(&state_type_id) {
                descriptor.abi = MirAbiClass::Aggregate;
                // Flow state linearity is a checker fact and must survive
                // layout materialization; do not collapse it to field
                // Copy-ness. The product-glue pass below supplies the
                // aggregate Move/Clone/Drop schedule.
                descriptor.layout = MirLayout::Record {
                    nominal: state_nominal,
                    fields,
                };
                descriptor.needs_drop_glue = descriptor.ownership.needs_drop();
                descriptor.needs_clone_glue = descriptor.ownership.needs_clone();
                descriptor.glue = MirGlueContract::for_type(&descriptor.kind, descriptor.ownership);
            }
        }
        // Record ownership/layout facts are attached above from the checker.
        // Re-run product materialization now so a tuple or record containing a
        // checker-described product sees the final child descriptor rather
        // than the pre-layout nominal placeholder produced by
        // `from_resolved_types`.
        catalog.materialize_product_glue();
        if errors.is_empty() {
            Ok(catalog)
        } else {
            Err(errors)
        }
    }

    pub fn get(&self, id: &ResolvedTypeId) -> Option<&MirTypeDesc> {
        self.entries.get(id)
    }

    /// Validate the closed Copy scalar leaf contract shared by the production
    /// consumers. A scalar leaf is a signed i32/i64 or bool with scalar
    /// layout and no-op glue; this is a TypeDesc fact, not a backend probe.
    pub fn validate_copy_scalar(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()))?;
        let supported_abi = matches!(
            descriptor.abi,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } | MirAbiClass::Bool
        );
        if !supported_abi
            || descriptor.kind == MirTypeKind::GenericParameter
            || descriptor.layout != MirLayout::Scalar
            || descriptor.ownership != MirOwnership::Copy
            || descriptor.glue
                != (MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
        {
            return Err(format!(
                "type '{}' is not a Copy signed scalar/bool with no-op glue",
                ty.as_str()
            ));
        }
        Ok(())
    }

    /// Validate the flat Copy record production contract shared by the
    /// selector and all consumers. Records must have stable, non-empty
    /// aggregate layout, no ownership glue, and scalar leaf fields only.
    /// Nested products and non-Copy fields stay outside this island until
    /// their own aggregate contract is promoted.
    pub fn validate_flat_copy_record(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()))?;
        let MirLayout::Record { fields, .. } = &descriptor.layout else {
            return Err(format!(
                "type '{}' has no canonical record layout",
                ty.as_str()
            ));
        };
        if descriptor.ownership != MirOwnership::Copy
            || descriptor.needs_drop_glue
            || descriptor.needs_clone_glue
            || descriptor.glue
                != (MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
        {
            return Err(format!(
                "record type '{}' is not in the flat Copy record contract",
                ty.as_str()
            ));
        }
        if descriptor.abi != MirAbiClass::Aggregate || fields.is_empty() {
            return Err(format!(
                "record type '{}' has no non-empty aggregate ABI",
                ty.as_str()
            ));
        }
        let mut field_ids = BTreeSet::new();
        for field in fields {
            if !field_ids.insert(&field.id) {
                return Err(format!(
                    "record type '{}' repeats field identity '{}'",
                    ty.as_str(),
                    field.id.0
                ));
            }
            self.validate_copy_scalar(&field.ty).map_err(|message| {
                format!(
                    "record type '{}' field '{}' is outside the flat Copy record contract: {message}",
                    ty.as_str(),
                    field.name
                )
            })?;
        }
        Ok(())
    }

    /// Validate the backend-independent flat Copy Option/Result layout.
    ///
    /// Every variant uses the same one-slot scalar payload ABI; a zero-field
    /// variant is allowed as the tag-only case.  The discriminant, variant
    /// identity, payload field identity and no-op ownership glue are all
    /// TypeDesc facts.  Native code may choose the physical `{i8, payload}`
    /// struct only after this contract succeeds; reference, bytecode and
    /// verifier consumers use the same semantic table.
    pub fn validate_flat_copy_variant(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let variants = match &descriptor.layout {
            MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => variants,
            layout => {
                return Err(format!(
                    "type '{}' layout {layout:?} is outside the flat Copy variant contract",
                    ty.as_str()
                ));
            }
        };
        if descriptor.kind != MirTypeKind::Option && descriptor.kind != MirTypeKind::Result {
            return Err(format!(
                "variant TypeDesc '{}' kind {:?} is outside the flat Copy variant contract",
                ty.as_str(),
                descriptor.kind
            ));
        }
        if descriptor.abi != MirAbiClass::Aggregate
            || descriptor.ownership != MirOwnership::Copy
            || descriptor.needs_drop_glue
            || descriptor.needs_clone_glue
            || descriptor.drop_plan.is_some()
            || descriptor.variant_drop_plan.is_some()
            || descriptor.glue
                != (MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
        {
            return Err(format!(
                "variant TypeDesc '{}' is not Aggregate/Copy with canonical no-op glue",
                ty.as_str()
            ));
        }
        if variants.is_empty() {
            return Err(format!(
                "variant TypeDesc '{}' has no variants in the flat Copy variant contract",
                ty.as_str()
            ));
        }

        let expected_nominal = match descriptor.kind {
            MirTypeKind::Option => "builtin:type:Option",
            MirTypeKind::Result => "builtin:type:Result",
            _ => unreachable!("variant kind checked above"),
        };
        let (actual_nominal, _) = self
            .variant_layout(ty)
            .expect("Option/Result layout checked above");
        if actual_nominal != expected_nominal {
            return Err(format!(
                "variant TypeDesc '{}' nominal '{}' disagrees with '{}'",
                ty.as_str(),
                actual_nominal,
                expected_nominal
            ));
        }

        let mut discriminants = BTreeSet::new();
        let mut variant_ids = BTreeSet::new();
        let mut field_ids = BTreeSet::new();
        let mut payload_type: Option<ResolvedTypeId> = None;
        for variant in variants {
            if !discriminants.insert(variant.discriminant) {
                return Err(format!(
                    "variant discriminant {} is duplicated in the flat Copy variant contract",
                    variant.discriminant
                ));
            }
            if variant.discriminant > u8::MAX as u16 {
                return Err(format!(
                    "variant discriminant {} does not fit the flat Copy variant ABI",
                    variant.discriminant
                ));
            }
            if !variant_ids.insert(variant.id.clone()) {
                return Err(format!(
                    "variant identity '{}' is duplicated in the flat Copy variant contract",
                    variant.id.0
                ));
            }
            if variant.fields.len() > 1 {
                return Err(format!(
                    "variant '{}' has {} payload fields; the flat Copy variant contract allows at most one",
                    variant.name,
                    variant.fields.len()
                ));
            }
            let Some(field) = variant.fields.first() else {
                continue;
            };
            if !field_ids.insert(field.id.clone()) {
                return Err(format!(
                    "variant payload field identity '{}' is duplicated in the flat Copy variant contract",
                    field.id.0
                ));
            }
            self.validate_copy_scalar(&field.ty).map_err(|message| {
                format!(
                    "variant '{}' payload is outside the flat Copy variant contract: {message}",
                    variant.name
                )
            })?;
            if let Some(expected) = &payload_type {
                if expected != &field.ty {
                    return Err(format!(
                        "variant payload type '{}' disagrees with '{}'; mixed payload ABI is outside the flat Copy variant contract",
                        field.ty.as_str(),
                        expected.as_str()
                    ));
                }
            } else {
                payload_type = Some(field.ty.clone());
            }
        }
        payload_type.ok_or_else(|| {
            "variant has no scalar payload; unit/zero-payload variants are outside the flat Copy variant contract".into()
        })
    }

    /// Validate the flat Copy variant contract and return one stable variant
    /// descriptor for a native switch arm.  The caller receives a TypeDesc
    /// fact rather than re-deriving the arm from a backend representation.
    pub fn validated_flat_copy_variant(
        &self,
        ty: &ResolvedTypeId,
        variant_id: &NodeId,
    ) -> Result<&MirVariantDesc, String> {
        self.validate_flat_copy_variant(ty)?;
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let variants = match &descriptor.layout {
            MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => variants,
            layout => {
                return Err(format!(
                    "type '{}' layout {layout:?} is outside the flat Copy variant contract",
                    ty.as_str()
                ));
            }
        };
        variants
            .iter()
            .find(|variant| variant.id == *variant_id)
            .ok_or_else(|| format!("variant '{}' is absent from TypeDesc", variant_id.0))
    }

    /// Validate the native flat Copy payload projection and return its
    /// canonical variant descriptor plus declaration-order field index.
    /// Native consumers must not repeat the single-payload/field-zero shape
    /// checks after selecting a variant descriptor.
    pub fn validated_flat_copy_payload_projection(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        variant_id: &NodeId,
        field_id: &NodeId,
        result_ty: &ResolvedTypeId,
    ) -> Result<(&MirVariantDesc, usize), String> {
        let variant = self.validated_flat_copy_variant(scrutinee_ty, variant_id)?;
        let projection = self.validated_variant_payload_projection_contract(
            scrutinee_ty,
            variant_id,
            field_id,
            result_ty,
        )?;
        let field_index = projection.field_index;
        if field_index != 0 || variant.fields.len() != 1 {
            return Err(format!(
                "variant '{}' payload projection is outside the single-payload flat Copy contract",
                variant.name
            ));
        }
        Ok((variant, field_index))
    }

    /// Validate a materialized receipt against the native flat-Copy
    /// single-payload policy. The receipt is the input fact; this helper does
    /// not expose a newly derived index to the native emitter.
    pub fn validate_flat_copy_payload_projection_receipt(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        variant_id: &NodeId,
        result_ty: &ResolvedTypeId,
        receipt: &MirVariantProjectionContract,
    ) -> Result<(), String> {
        self.validate_variant_payload_projection_receipt(
            scrutinee_ty,
            variant_id,
            result_ty,
            receipt,
        )?;
        let variant = self.validated_flat_copy_variant(scrutinee_ty, variant_id)?;
        if receipt.field_index != 0 || receipt.arity != 1 || variant.fields.len() != 1 {
            return Err(format!(
                "variant '{}' payload projection is outside the single-payload flat Copy contract",
                variant.name
            ));
        }
        Ok(())
    }

    /// Validate the argument side of the first concrete generic MIR
    /// instance contract.  This is deliberately narrower than the complete
    /// scalar universe: native and MIR verifier must agree on signed i32/i64
    /// and bool, with Copy/no-op glue and no ownership transfer at the call
    /// boundary.
    pub fn validate_scalar_generic_arguments(
        &self,
        arguments: &[ResolvedTypeId],
    ) -> Result<(), String> {
        if arguments.len() != 1 {
            return Err(format!(
                "scalar generic identity contract requires one type argument, got {}",
                arguments.len()
            ));
        }
        let ty = &arguments[0];
        self.validate_copy_scalar(ty)
    }

    /// Validate the concrete argument for the generic identity instance.
    /// Identity is the first generic contract whose result can carry the
    /// already-closed flat Copy Option/Result ABI receipt; Set facades remain
    /// scalar-only and continue to use `validate_scalar_generic_arguments`.
    pub fn validate_generic_identity_arguments(
        &self,
        arguments: &[ResolvedTypeId],
    ) -> Result<(), String> {
        if arguments.len() != 1 {
            return Err(format!(
                "generic identity contract requires one type argument, got {}",
                arguments.len()
            ));
        }
        let ty = &arguments[0];
        self.validate_copy_scalar(ty)
            .or_else(|_| self.validate_flat_copy_variant(ty).map(|_| ()))
            .or_else(|_| self.validate_owned_string(ty))
            .map_err(|message| {
                format!(
                    "type '{}' is outside the concrete generic identity contract: {message}",
                    ty.as_str()
                )
            })
    }

    /// Validate a value boundary against the canonical glue contract.  The
    /// result/source type equality is checked by the MIR instruction validator;
    /// this method only answers whether the operation has a materialized
    /// implementation for the descriptor.
    pub fn validate_glue(
        &self,
        ty: &ResolvedTypeId,
        operation: MirGlueOperation,
    ) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let supported = match operation {
            MirGlueOperation::MoveOut => descriptor.glue.supports_move_out(),
            MirGlueOperation::Clone => descriptor.glue.supports_clone(),
            MirGlueOperation::Drop => descriptor.glue.supports_drop(),
        };
        if !supported {
            return Err(format!(
                "type '{}' ownership {:?} has no canonical {:?} glue",
                ty.as_str(),
                descriptor.ownership,
                operation
            ));
        }
        let operation_glue = match operation {
            MirGlueOperation::MoveOut => descriptor.glue.move_out,
            MirGlueOperation::Clone => descriptor.glue.clone,
            MirGlueOperation::Drop => descriptor.glue.drop,
        };
        if operation_glue == MirGlueKind::Aggregate {
            match &descriptor.layout {
                MirLayout::Option { .. } | MirLayout::Result { .. } => {
                    self.validate_variant_glue(ty, operation)?;
                }
                _ => self.validate_aggregate_glue(ty, operation)?,
            }
        }
        if operation_glue == MirGlueKind::List {
            self.validate_list_glue(ty, operation)?;
        }
        if operation_glue == MirGlueKind::Set {
            self.validate_set_glue(ty, operation)?;
        }
        Ok(())
    }

    /// Validate the complete canonical scalar `string` contract.
    ///
    /// `StringHandle` is a semantic ABI class, not an opaque permission for a
    /// backend to choose its own representation.  The current physical
    /// contract is the length-bearing `{ptr, i64}` value described by
    /// `MirLayout::Handle`; all three ownership operations must be backed by
    /// the same `OwnedString` glue family before a consumer may materialize
    /// the value.
    pub fn validate_owned_string(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        if descriptor.kind != MirTypeKind::Primitive(PrimitiveType::String)
            || descriptor.abi != MirAbiClass::StringHandle
            || descriptor.layout != MirLayout::Handle
            || descriptor.ownership != MirOwnership::Move
        {
            return Err(format!(
                "type '{}' is not the canonical owned StringHandle contract",
                ty.as_str()
            ));
        }
        let expected = MirGlueContract {
            move_out: MirGlueKind::OwnedString,
            clone: MirGlueKind::OwnedString,
            drop: MirGlueKind::OwnedString,
        };
        if descriptor.glue != expected
            || !descriptor.needs_drop_glue
            || !descriptor.needs_clone_glue
        {
            return Err(format!(
                "type '{}' owned string glue contract is incomplete",
                ty.as_str()
            ));
        }
        Ok(())
    }

    /// Validate the first variable-length container contract. A List is a
    /// move-owned runtime handle, but its element identity and glue are still
    /// canonical MIR facts. This slice intentionally admits only concrete
    /// Copy scalars so a backend cannot accidentally inherit recursive or
    /// element-drop semantics from the VM's generic list implementation.
    pub fn validate_list_glue(
        &self,
        ty: &ResolvedTypeId,
        operation: MirGlueOperation,
    ) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let MirLayout::List { element } = &descriptor.layout else {
            return Err(format!(
                "list glue type '{}' has no canonical List layout",
                ty.as_str()
            ));
        };
        if descriptor.kind != MirTypeKind::List
            || descriptor.abi != MirAbiClass::OpaqueHandle
            || descriptor.ownership != MirOwnership::Move
        {
            return Err(format!(
                "type '{}' List TypeDesc has an inconsistent ABI/ownership contract",
                ty.as_str()
            ));
        }
        let expected = MirGlueContract {
            move_out: MirGlueKind::List,
            clone: MirGlueKind::List,
            drop: MirGlueKind::List,
        };
        if descriptor.glue != expected {
            return Err(format!(
                "type '{}' List glue contract is not fully materialized",
                ty.as_str()
            ));
        }
        let element_desc = self.get(element).ok_or_else(|| {
            format!(
                "List '{}' element type '{}' is absent from MIR type catalog",
                ty.as_str(),
                element.as_str()
            )
        })?;
        if element_desc.ownership != MirOwnership::Copy
            || element_desc.glue
                != (MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
            || !matches!(element_desc.layout, MirLayout::Scalar)
            || !matches!(
                element_desc.abi,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                } | MirAbiClass::Bool
            )
        {
            return Err(format!(
                "List '{}' element type '{}' is outside the canonical Copy scalar contract",
                ty.as_str(),
                element.as_str()
            ));
        }
        let operation_glue = match operation {
            MirGlueOperation::MoveOut => descriptor.glue.move_out,
            MirGlueOperation::Clone => descriptor.glue.clone,
            MirGlueOperation::Drop => descriptor.glue.drop,
        };
        if operation_glue != MirGlueKind::List {
            return Err(format!(
                "List '{}' operation {:?} is missing List glue",
                ty.as_str(),
                operation
            ));
        }
        Ok(())
    }

    /// Validate an operation over the canonical scalar List island.
    /// `Len` borrows the List handle and returns a Copy i32. `Reverse` borrows
    /// the source and returns a fresh move-owned List produced through Clone
    /// glue. `Concat` consumes both List handles and returns a fresh List with
    /// the union of both input obligations. The argument slot is explicit so
    /// a backend cannot infer arity or ownership from its storage ABI.
    pub fn validate_list_operation(
        &self,
        result_ty: &ResolvedTypeId,
        list_ty: &ResolvedTypeId,
        operation: crate::core::mir::MirListOperation,
    ) -> Result<(), String> {
        self.validate_list_operation_with_argument(result_ty, list_ty, None, operation)
    }

    pub fn validate_list_operation_with_argument(
        &self,
        result_ty: &ResolvedTypeId,
        list_ty: &ResolvedTypeId,
        argument_ty: Option<&ResolvedTypeId>,
        operation: crate::core::mir::MirListOperation,
    ) -> Result<(), String> {
        self.validate_list_glue(list_ty, MirGlueOperation::MoveOut)?;
        let result = self.get(result_ty).ok_or_else(|| {
            format!(
                "List operation result type '{}' is absent",
                result_ty.as_str()
            )
        })?;
        match operation {
            crate::core::mir::MirListOperation::Len => {
                if argument_ty.is_some() {
                    return Err("List.len does not accept a second List argument".into());
                }
                if result.kind != MirTypeKind::Primitive(PrimitiveType::I32)
                    || result.abi
                        != (MirAbiClass::Integer {
                            bits: 32,
                            signed: true,
                        })
                    || result.layout != MirLayout::Scalar
                    || result.ownership != MirOwnership::Copy
                {
                    return Err("List.len result must be a Copy i32 scalar".into());
                }
            }
            crate::core::mir::MirListOperation::Reverse => {
                if argument_ty.is_some() {
                    return Err("List.reverse does not accept a second List argument".into());
                }
                if result_ty != list_ty {
                    return Err(format!(
                        "List.reverse result type '{}' disagrees with receiver type '{}'",
                        result_ty.as_str(),
                        list_ty.as_str()
                    ));
                }
                // The operation is non-mutating: the source is borrowed for
                // the read, then cloned. Validate both sides explicitly so a
                // backend cannot erase the source Drop obligation or return
                // a shallow alias as an owned result.
                self.validate_list_glue(list_ty, MirGlueOperation::Clone)?;
                self.validate_list_glue(result_ty, MirGlueOperation::MoveOut)?;
            }
            crate::core::mir::MirListOperation::Concat => {
                let argument_ty = argument_ty
                    .ok_or_else(|| "List.concat requires a second List argument".to_string())?;
                self.validate_list_glue(argument_ty, MirGlueOperation::MoveOut)?;
                if result_ty != list_ty || result_ty != argument_ty {
                    return Err(format!(
                        "List.concat result '{}' and inputs '{}'/'{}' must have one canonical List type",
                        result_ty.as_str(),
                        list_ty.as_str(),
                        argument_ty.as_str()
                    ));
                }
            }
        }
        Ok(())
    }

    /// Materialize the complete TypeDesc receipt for a canonical List
    /// operation. This is the only constructor for the operation receipt;
    /// bytecode/native/verifier consumers must receive its result from MIR.
    pub fn validated_list_operation_contract(
        &self,
        result_ty: &ResolvedTypeId,
        list_ty: &ResolvedTypeId,
        operation: crate::core::mir::MirListOperation,
    ) -> Result<MirListOperationContract, String> {
        self.validated_list_operation_contract_with_argument(result_ty, list_ty, None, operation)
    }

    pub fn validated_list_operation_contract_with_argument(
        &self,
        result_ty: &ResolvedTypeId,
        list_ty: &ResolvedTypeId,
        argument_ty: Option<&ResolvedTypeId>,
        operation: crate::core::mir::MirListOperation,
    ) -> Result<MirListOperationContract, String> {
        self.validate_list_operation_with_argument(result_ty, list_ty, argument_ty, operation)?;
        let descriptor = self.get(list_ty).ok_or_else(|| {
            format!(
                "List operation receiver type '{}' is absent",
                list_ty.as_str()
            )
        })?;
        let MirLayout::List { element } = &descriptor.layout else {
            return Err(format!(
                "List operation receiver type '{}' has no canonical List layout",
                list_ty.as_str()
            ));
        };
        Ok(MirListOperationContract {
            list_ty: list_ty.clone(),
            element_ty: element.clone(),
            result_ty: result_ty.clone(),
            argument_ty: argument_ty.cloned(),
            operation,
        })
    }

    /// Validate a materialized List operation receipt against the checker
    /// TypeDesc graph. A stale operation/receiver/result tuple is invalid MIR
    /// and must be rejected before any consumer executes it.
    pub fn validate_list_operation_receipt(
        &self,
        result_ty: &ResolvedTypeId,
        list_ty: &ResolvedTypeId,
        operation: crate::core::mir::MirListOperation,
        receipt: &MirListOperationContract,
    ) -> Result<(), String> {
        self.validate_list_operation_receipt_with_argument(
            result_ty, list_ty, None, operation, receipt,
        )
    }

    pub fn validate_list_operation_receipt_with_argument(
        &self,
        result_ty: &ResolvedTypeId,
        list_ty: &ResolvedTypeId,
        argument_ty: Option<&ResolvedTypeId>,
        operation: crate::core::mir::MirListOperation,
        receipt: &MirListOperationContract,
    ) -> Result<(), String> {
        let expected = self.validated_list_operation_contract_with_argument(
            result_ty,
            list_ty,
            argument_ty,
            operation,
        )?;
        if receipt != &expected {
            return Err("List operation receipt disagrees with TypeDesc".into());
        }
        Ok(())
    }

    /// Validate the first Set production island. Set values are move-owned
    /// runtime handles, but the element identity is still part of the
    /// canonical contract. The island admits only `Set<T>` with a concrete
    /// Copy scalar `T`; erased `Set` and string/aggregate elements remain
    /// fail-closed until their own payload and equality/drop contracts exist.
    pub fn validate_set_glue(
        &self,
        ty: &ResolvedTypeId,
        operation: MirGlueOperation,
    ) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let MirLayout::Set { element } = &descriptor.layout else {
            return Err(format!(
                "set glue type '{}' has no canonical Set<T> layout",
                ty.as_str()
            ));
        };
        if descriptor.kind != MirTypeKind::Set
            || descriptor.abi != MirAbiClass::SetHandle
            || descriptor.ownership != MirOwnership::Move
        {
            return Err(format!(
                "type '{}' Set TypeDesc has an inconsistent ABI/ownership contract",
                ty.as_str()
            ));
        }
        let expected = MirGlueContract {
            move_out: MirGlueKind::Set,
            clone: MirGlueKind::Set,
            drop: MirGlueKind::Set,
        };
        if descriptor.glue != expected
            || !descriptor.needs_drop_glue
            || !descriptor.needs_clone_glue
        {
            return Err(format!(
                "type '{}' Set glue contract is not fully materialized",
                ty.as_str()
            ));
        }
        let element_desc = self.get(element).ok_or_else(|| {
            format!(
                "Set '{}' element type '{}' is absent from MIR type catalog",
                ty.as_str(),
                element.as_str()
            )
        })?;
        if element_desc.ownership != MirOwnership::Copy
            || element_desc.glue
                != (MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
            || !matches!(element_desc.layout, MirLayout::Scalar)
            || !matches!(
                element_desc.abi,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                } | MirAbiClass::Bool
            )
        {
            return Err(format!(
                "Set '{}' element type '{}' is outside the canonical Copy scalar contract",
                ty.as_str(),
                element.as_str()
            ));
        }
        let operation_glue = match operation {
            MirGlueOperation::MoveOut => descriptor.glue.move_out,
            MirGlueOperation::Clone => descriptor.glue.clone,
            MirGlueOperation::Drop => descriptor.glue.drop,
        };
        if operation_glue != MirGlueKind::Set {
            return Err(format!(
                "Set '{}' operation {:?} is missing Set glue",
                ty.as_str(),
                operation
            ));
        }
        Ok(())
    }

    /// Validate a canonical Set literal. All elements must agree with the
    /// Set<T> layout; duplicate elimination is a runtime semantic, not a
    /// backend-specific construction rule.
    pub fn validate_set_construct(
        &self,
        result_ty: &ResolvedTypeId,
        element_types: &[ResolvedTypeId],
    ) -> Result<(), String> {
        self.validate_set_glue(result_ty, MirGlueOperation::MoveOut)?;
        let descriptor = self
            .get(result_ty)
            .ok_or_else(|| format!("Set result type '{}' is absent", result_ty.as_str()))?;
        let MirLayout::Set { element } = &descriptor.layout else {
            return Err(format!(
                "Set result type '{}' has no canonical Set<T> layout",
                result_ty.as_str()
            ));
        };
        for (index, actual) in element_types.iter().enumerate() {
            if actual != element {
                return Err(format!(
                    "Set element {} type '{}' disagrees with layout element type '{}'",
                    index,
                    actual.as_str(),
                    element.as_str()
                ));
            }
        }
        Ok(())
    }

    /// Validate one canonical Set operation. Read operations preserve the
    /// receiver; insert/remove consume the receiver and return a fresh
    /// move-owned Set value. This distinction is carried by the MIR node and
    /// is not inferred from a backend's in-place handle implementation.
    pub fn validate_set_operation(
        &self,
        result_ty: &ResolvedTypeId,
        set_ty: &ResolvedTypeId,
        argument_ty: Option<&ResolvedTypeId>,
        operation: crate::core::mir::MirSetOperation,
    ) -> Result<(), String> {
        self.validate_set_glue(set_ty, MirGlueOperation::MoveOut)?;
        let set_desc = self
            .get(set_ty)
            .ok_or_else(|| format!("Set operand type '{}' is absent", set_ty.as_str()))?;
        let MirLayout::Set { element } = &set_desc.layout else {
            return Err(format!(
                "Set operand type '{}' has no canonical Set<T> layout",
                set_ty.as_str()
            ));
        };
        if result_ty != set_ty
            && matches!(
                operation,
                crate::core::mir::MirSetOperation::Insert
                    | crate::core::mir::MirSetOperation::Remove
            )
        {
            return Err(format!(
                "Set operation {:?} result type '{}' disagrees with receiver type '{}'",
                operation,
                result_ty.as_str(),
                set_ty.as_str()
            ));
        }
        let result_desc = self.get(result_ty).ok_or_else(|| {
            format!(
                "Set operation result type '{}' is absent",
                result_ty.as_str()
            )
        })?;
        match operation {
            crate::core::mir::MirSetOperation::Size => {
                if result_desc.abi
                    != (MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    })
                    || result_desc.layout != MirLayout::Scalar
                    || result_desc.ownership != MirOwnership::Copy
                {
                    return Err("Set.size result must be a Copy i32 scalar".into());
                }
                if argument_ty.is_some() {
                    return Err("Set.size does not accept an argument".into());
                }
            }
            MirSetOperation::IsEmpty | MirSetOperation::Contains => {
                if result_desc.abi != MirAbiClass::Bool
                    || result_desc.layout != MirLayout::Scalar
                    || result_desc.ownership != MirOwnership::Copy
                {
                    return Err(format!(
                        "Set.{:?} result must be a Copy bool scalar",
                        operation
                    ));
                }
                if operation == crate::core::mir::MirSetOperation::IsEmpty {
                    if argument_ty.is_some() {
                        return Err("Set.is_empty does not accept an argument".into());
                    }
                } else if argument_ty != Some(element) {
                    return Err("Set.contains argument must match the Set<T> element type".into());
                }
            }
            MirSetOperation::Insert | MirSetOperation::Remove => {
                if result_desc.ownership != MirOwnership::Move {
                    return Err(format!("Set.{:?} result must remain move-owned", operation));
                }
                if argument_ty != Some(element) {
                    return Err(format!(
                        "Set.{:?} argument must match the Set<T> element type",
                        operation
                    ));
                }
            }
            MirSetOperation::ToList => {
                if argument_ty.is_some() {
                    return Err("Set.to_list does not accept an argument".into());
                }
                let MirLayout::List {
                    element: list_element,
                } = &result_desc.layout
                else {
                    return Err("Set.to_list result must have a canonical List<T> layout".into());
                };
                if list_element != element {
                    return Err(
                        "Set.to_list result List<T> element must match the Set<T> element".into(),
                    );
                }
                self.validate_list_glue(result_ty, MirGlueOperation::MoveOut)
                    .map_err(|message| format!("Set.to_list result is unsupported: {message}"))?;
            }
        }
        Ok(())
    }

    /// Validate the recursive glue graph for an Option/Result payload.  The
    /// plan must cover every canonical variant, including zero-field `None`,
    /// and every payload child must be validated through its own TypeDesc.
    pub fn validate_variant_glue(
        &self,
        ty: &ResolvedTypeId,
        operation: MirGlueOperation,
    ) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let variants = match &descriptor.layout {
            MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => variants,
            _ => {
                return Err(format!(
                    "variant glue type '{}' has no canonical Option/Result layout",
                    ty.as_str()
                ))
            }
        };
        let expected_contract = MirGlueContract {
            move_out: MirGlueKind::Aggregate,
            clone: MirGlueKind::Aggregate,
            drop: MirGlueKind::Aggregate,
        };
        if descriptor.glue != expected_contract {
            return Err(format!(
                "type '{}' variant glue contract is not fully materialized",
                ty.as_str()
            ));
        }
        let Some(plans) = &descriptor.variant_drop_plan else {
            return Err(format!(
                "type '{}' variant glue has no variant drop plan",
                ty.as_str()
            ));
        };
        if plans.len() != variants.len() {
            return Err(format!(
                "type '{}' variant drop plan has {} variants but layout has {}",
                ty.as_str(),
                plans.len(),
                variants.len()
            ));
        }
        for (variant, plan) in variants.iter().zip(plans) {
            if plan.variant != variant.id {
                return Err(format!(
                    "type '{}' variant drop plan identity disagrees with layout",
                    ty.as_str()
                ));
            }
            let fields = if matches!(operation, MirGlueOperation::Drop) {
                if plan.fields.len() != variant.fields.len() {
                    return Err(format!(
                        "type '{}' variant '{}' drop plan has {} fields but layout has {}",
                        ty.as_str(),
                        variant.name,
                        plan.fields.len(),
                        variant.fields.len()
                    ));
                }
                for (expected_index, field) in (0..variant.fields.len()).rev().zip(&plan.fields) {
                    if field.index != expected_index || field.ty != variant.fields[field.index].ty {
                        return Err(format!(
                            "type '{}' variant '{}' drop plan is not in reverse declaration order",
                            ty.as_str(),
                            variant.name
                        ));
                    }
                    let child = self.get(&field.ty).ok_or_else(|| {
                        format!(
                            "type '{}' variant '{}' child type '{}' is absent",
                            ty.as_str(),
                            variant.name,
                            field.ty.as_str()
                        )
                    })?;
                    if child.glue.drop != field.glue {
                        return Err(format!(
                            "type '{}' variant '{}' field {} glue disagrees with child TypeDesc",
                            ty.as_str(),
                            variant.name,
                            field.index
                        ));
                    }
                }
                plan.fields
                    .iter()
                    .map(|field| &field.ty)
                    .collect::<Vec<_>>()
            } else {
                variant
                    .fields
                    .iter()
                    .map(|field| &field.ty)
                    .collect::<Vec<_>>()
            };
            for field_ty in fields {
                self.validate_glue(field_ty, operation)?;
            }
        }
        Ok(())
    }

    /// Return one active variant's already-validated drop schedule.
    ///
    /// The plan is selected by stable variant identity, never by payload
    /// arity or a backend representation.  Consumers may use the returned
    /// declaration indices to schedule child drops after this method has
    /// validated the complete recursive variant glue graph.
    pub fn validated_variant_drop_plan(
        &self,
        ty: &ResolvedTypeId,
        variant_id: &NodeId,
    ) -> Result<&MirVariantDropGluePlan, String> {
        self.validate_variant_glue(ty, MirGlueOperation::Drop)?;
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        descriptor
            .variant_drop_plan
            .as_ref()
            .and_then(|plans| plans.iter().find(|plan| plan.variant == *variant_id))
            .ok_or_else(|| {
                format!(
                    "type '{}' has no drop plan for variant '{}'",
                    ty.as_str(),
                    variant_id.0
                )
            })
    }

    /// Return the canonical nominal and active drop plan for one runtime
    /// variant value.  The plan validator proves the complete recursive glue
    /// graph and stable variant identity; runtime consumers may then use the
    /// plan length to check their concrete payload without re-deriving layout
    /// arity from a runtime representation.
    pub fn validated_variant_drop_contract(
        &self,
        ty: &ResolvedTypeId,
        variant_id: &NodeId,
    ) -> Result<(&str, &MirVariantDropGluePlan), String> {
        let expected_nominal = self
            .variant_layout(ty)
            .map(|(nominal, _)| nominal)
            .ok_or_else(|| {
                format!(
                    "type '{}' has no canonical Option/Result variant layout",
                    ty.as_str()
                )
            })?;
        let plan = self.validated_variant_drop_plan(ty, variant_id)?;
        Ok((expected_nominal, plan))
    }

    /// Return the complete canonical variant table after proving the
    /// recursive drop-glue graph.  A variant drop opcode may be reached from
    /// a default arm or a scope exit, so it cannot select one active variant
    /// statically; consumers must carry this whole TypeDesc table instead of
    /// reconstructing tag/arity pairs from a runtime representation.
    pub fn validated_variant_drop_contract_table(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<(&str, &[MirVariantDesc]), String> {
        let (expected_nominal, variants) = self.validated_variant_shape_table(ty)?;
        self.validate_variant_glue(ty, MirGlueOperation::Drop)?;
        Ok((expected_nominal, variants))
    }

    /// Return the complete canonical variant shape table without requiring
    /// ownership glue.  Copy variants still need a checked tag/discriminant/
    /// arity mapping when a backend constructs them; non-Copy consumers add
    /// the operation-specific recursive glue proof on top of this table.
    pub fn validated_variant_shape_table(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<(&str, &[MirVariantDesc]), String> {
        let (expected_nominal, variants) = self.variant_layout(ty).ok_or_else(|| {
            format!(
                "type '{}' has no canonical Option/Result variant layout",
                ty.as_str()
            )
        })?;
        if variants.is_empty() {
            return Err(format!(
                "type '{}' has an empty canonical variant table",
                ty.as_str()
            ));
        }
        let mut names = BTreeSet::new();
        let mut discriminants = BTreeSet::new();
        let mut variant_ids = BTreeSet::new();
        for variant in variants {
            if variant.id.0.is_empty() {
                return Err(format!(
                    "type '{}' has a variant with an empty canonical identity",
                    ty.as_str()
                ));
            }
            if !variant_ids.insert(&variant.id) {
                return Err(format!(
                    "type '{}' has duplicate canonical variant identity '{}'",
                    ty.as_str(),
                    variant.id.0
                ));
            }
            if !names.insert(&variant.name) {
                return Err(format!(
                    "type '{}' has duplicate variant tag '{}'",
                    ty.as_str(),
                    variant.name
                ));
            }
            if !discriminants.insert(variant.discriminant) {
                return Err(format!(
                    "type '{}' has duplicate variant discriminant {}",
                    ty.as_str(),
                    variant.discriminant
                ));
            }
        }
        Ok((expected_nominal, variants))
    }

    /// Validate the recursive product glue graph for one operation.  The
    /// caller still owns the choice of operation; this method only follows
    /// canonical child descriptors and never consults a backend ABI.
    pub fn validate_aggregate_glue(
        &self,
        ty: &ResolvedTypeId,
        operation: MirGlueOperation,
    ) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let (layout_name, elements) = match &descriptor.layout {
            MirLayout::Tuple(elements) => ("tuple", elements.clone()),
            MirLayout::Record { fields, .. } => (
                "record",
                fields.iter().map(|field| field.ty.clone()).collect(),
            ),
            _ => {
                return Err(format!(
                    "aggregate glue type '{}' has no canonical product layout",
                    ty.as_str()
                ));
            }
        };
        if elements.is_empty() {
            return Err(format!(
                "aggregate glue type '{}' has no fields",
                ty.as_str()
            ));
        }
        let expected_contract = MirGlueContract {
            move_out: MirGlueKind::Aggregate,
            clone: MirGlueKind::Aggregate,
            drop: MirGlueKind::Aggregate,
        };
        if descriptor.glue != expected_contract {
            return Err(format!(
                "type '{}' aggregate glue contract is not fully materialized",
                ty.as_str()
            ));
        }
        if matches!(operation, MirGlueOperation::Drop) {
            let Some(plan) = &descriptor.drop_plan else {
                return Err(format!(
                    "type '{}' aggregate drop glue has no drop plan",
                    ty.as_str()
                ));
            };
            if plan.fields.len() != elements.len() {
                return Err(format!(
                    "type '{}' drop plan has {} fields but {} has {}",
                    ty.as_str(),
                    plan.fields.len(),
                    layout_name,
                    elements.len()
                ));
            }
            for (expected_index, field) in (0..elements.len()).rev().zip(&plan.fields) {
                if field.index != expected_index {
                    return Err(format!(
                        "type '{}' drop plan is not in reverse declaration order",
                        ty.as_str()
                    ));
                }
                if field.ty != elements[field.index] {
                    return Err(format!(
                        "type '{}' drop plan field {} type disagrees with {} layout",
                        ty.as_str(),
                        field.index,
                        layout_name
                    ));
                }
                let child = self.get(&field.ty).ok_or_else(|| {
                    format!(
                        "type '{}' drop plan child type '{}' is absent",
                        ty.as_str(),
                        field.ty.as_str()
                    )
                })?;
                if child.glue.drop != field.glue {
                    return Err(format!(
                        "type '{}' drop plan field {} glue disagrees with child TypeDesc",
                        ty.as_str(),
                        field.index
                    ));
                }
                self.validate_glue(&field.ty, MirGlueOperation::Drop)?;
            }
        } else {
            for element in elements {
                self.validate_glue(&element, operation)?;
            }
        }
        Ok(())
    }

    /// Validate the concrete recursive tuple product contract shared by the
    /// native and MIR-verifier consumers.  This is intentionally narrower
    /// than the general aggregate glue graph: tuple leaves may only be a
    /// Copy scalar or the canonical owned String, and aggregate children may
    /// only be another tuple.  Keeping this rule in the TypeDesc catalog
    /// prevents a backend from silently widening the ABI by inspecting its
    /// physical struct representation.
    pub fn validate_recursive_tuple_abi(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let mut visiting = BTreeSet::new();
        self.validate_recursive_tuple_abi_inner(ty, &mut visiting)
    }

    fn validate_recursive_tuple_abi_inner(
        &self,
        ty: &ResolvedTypeId,
        visiting: &mut BTreeSet<ResolvedTypeId>,
    ) -> Result<(), String> {
        if !visiting.insert(ty.clone()) {
            return Err(format!(
                "tuple TypeDesc '{}' contains a recursive ABI cycle",
                ty.as_str()
            ));
        }
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let MirLayout::Tuple(elements) = &descriptor.layout else {
            return Err(format!(
                "type '{}' is not a canonical tuple layout",
                ty.as_str()
            ));
        };
        if elements.is_empty() {
            return Err(format!(
                "tuple TypeDesc '{}' has no fields in the scalar/String/tuple ABI",
                ty.as_str()
            ));
        }
        if !matches!(
            &descriptor.kind,
            MirTypeKind::Tuple { arity } if *arity == elements.len()
        ) {
            return Err(format!(
                "tuple TypeDesc '{}' kind/layout arity disagrees",
                ty.as_str()
            ));
        }
        if descriptor.abi != MirAbiClass::Aggregate {
            return Err(format!(
                "tuple TypeDesc '{}' has ABI {:?}, expected Aggregate",
                ty.as_str(),
                descriptor.abi
            ));
        }

        let noop = MirGlueContract {
            move_out: MirGlueKind::Noop,
            clone: MirGlueKind::Noop,
            drop: MirGlueKind::Noop,
        };
        if descriptor.ownership == MirOwnership::Copy {
            if descriptor.glue != noop
                || descriptor.needs_drop_glue
                || descriptor.needs_clone_glue
                || descriptor.drop_plan.is_some()
            {
                return Err(format!(
                    "Copy tuple TypeDesc '{}' does not carry the canonical no-op glue contract",
                    ty.as_str()
                ));
            }
        } else {
            if descriptor.ownership != MirOwnership::Move {
                return Err(format!(
                    "tuple TypeDesc '{}' ownership {:?} is outside the concrete Move contract",
                    ty.as_str(),
                    descriptor.ownership
                ));
            }
            let aggregate = MirGlueContract {
                move_out: MirGlueKind::Aggregate,
                clone: MirGlueKind::Aggregate,
                drop: MirGlueKind::Aggregate,
            };
            if descriptor.glue != aggregate
                || !descriptor.needs_drop_glue
                || !descriptor.needs_clone_glue
                || descriptor.drop_plan.is_none()
            {
                return Err(format!(
                    "tuple TypeDesc '{}' aggregate glue/drop plan is incomplete",
                    ty.as_str()
                ));
            }
            for operation in [
                MirGlueOperation::MoveOut,
                MirGlueOperation::Clone,
                MirGlueOperation::Drop,
            ] {
                self.validate_glue(ty, operation)?;
            }
        }

        for (index, element) in elements.iter().enumerate() {
            let child = self.get(element).ok_or_else(|| {
                format!(
                    "tuple '{}' field {} type '{}' is absent from MIR TypeDesc catalog",
                    ty.as_str(),
                    index,
                    element.as_str()
                )
            })?;
            if descriptor.ownership == MirOwnership::Copy && child.ownership != MirOwnership::Copy {
                return Err(format!(
                    "Copy tuple '{}' field {} type '{}' is non-Copy and cannot be hidden by a Copy parent",
                    ty.as_str(),
                    index,
                    element.as_str()
                ));
            }
            let supported = self.validate_copy_scalar(element).is_ok()
                || self.validate_owned_string(element).is_ok()
                || matches!(child.layout, MirLayout::Tuple(_));
            if !supported {
                return Err(format!(
                    "tuple '{}' field {} type '{}' is outside the scalar/String/tuple ABI",
                    ty.as_str(),
                    index,
                    element.as_str()
                ));
            }
            if matches!(child.layout, MirLayout::Tuple(_)) {
                self.validate_recursive_tuple_abi_inner(element, visiting)?;
            }
        }
        visiting.remove(ty);
        Ok(())
    }

    fn materialize_product_glue(&mut self) {
        let ids = self.entries.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let mut visiting = BTreeSet::new();
            let _ = self.materialize_glue_for(&id, &mut visiting);
        }
    }

    fn materialize_product_glue_for(
        &mut self,
        id: &ResolvedTypeId,
        visiting: &mut BTreeSet<ResolvedTypeId>,
    ) -> bool {
        if !visiting.insert(id.clone()) {
            return false;
        }
        let layout = self.get(id).map(|descriptor| descriptor.layout.clone());
        let Some(elements) = (match layout {
            Some(MirLayout::Tuple(elements)) => Some(elements),
            Some(MirLayout::Record { fields, .. }) => {
                Some(fields.into_iter().map(|field| field.ty).collect())
            }
            _ => None,
        }) else {
            visiting.remove(id);
            return false;
        };
        let mut children = Vec::with_capacity(elements.len());
        for (index, child_id) in elements.iter().enumerate() {
            let child_is_composite = self.get(child_id).is_some_and(|child| {
                matches!(
                    child.layout,
                    MirLayout::Tuple(_)
                        | MirLayout::Record { .. }
                        | MirLayout::Option { .. }
                        | MirLayout::Result { .. }
                )
            });
            let child_is_copy = self
                .get(child_id)
                .is_some_and(|child| child.ownership == MirOwnership::Copy);
            if child_is_composite
                && !child_is_copy
                && !self.materialize_glue_for(child_id, visiting)
            {
                visiting.remove(id);
                return false;
            }
            let Some(child) = self.get(child_id) else {
                visiting.remove(id);
                return false;
            };
            if !child.glue.supports_move_out()
                || !child.glue.supports_clone()
                || !child.glue.supports_drop()
            {
                visiting.remove(id);
                return false;
            }
            children.push(MirDropGlueField {
                index,
                ty: child_id.clone(),
                glue: child.glue.drop,
            });
        }
        let Some(descriptor) = self.entries.get_mut(id) else {
            visiting.remove(id);
            return false;
        };
        if descriptor.ownership != MirOwnership::Copy && !children.is_empty() {
            descriptor.glue = MirGlueContract {
                move_out: MirGlueKind::Aggregate,
                clone: MirGlueKind::Aggregate,
                drop: MirGlueKind::Aggregate,
            };
            children.reverse();
            descriptor.drop_plan = Some(MirDropGluePlan { fields: children });
        }
        visiting.remove(id);
        descriptor.glue.move_out == MirGlueKind::Aggregate
    }

    fn materialize_glue_for(
        &mut self,
        id: &ResolvedTypeId,
        visiting: &mut BTreeSet<ResolvedTypeId>,
    ) -> bool {
        let layout = self.get(id).map(|descriptor| descriptor.layout.clone());
        match layout {
            Some(MirLayout::Tuple(elements)) => {
                self.materialize_product_glue_for(id, visiting)
                    || elements.is_empty()
                        && self
                            .get(id)
                            .is_some_and(|descriptor| descriptor.ownership == MirOwnership::Copy)
            }
            Some(MirLayout::Record { .. }) => self.materialize_product_glue_for(id, visiting),
            Some(MirLayout::Option { variants, .. }) | Some(MirLayout::Result { variants, .. }) => {
                self.materialize_variant_glue_for(id, variants, visiting)
            }
            _ => self
                .get(id)
                .is_some_and(|descriptor| descriptor.glue.supports_move_out()),
        }
    }

    fn materialize_variant_glue_for(
        &mut self,
        id: &ResolvedTypeId,
        variants: Vec<MirVariantDesc>,
        visiting: &mut BTreeSet<ResolvedTypeId>,
    ) -> bool {
        if !visiting.insert(id.clone()) {
            return false;
        }
        let mut plans = Vec::with_capacity(variants.len());
        for variant in &variants {
            let mut fields = Vec::with_capacity(variant.fields.len());
            for (index, field) in variant.fields.iter().enumerate() {
                let child_is_composite = self.get(&field.ty).is_some_and(|child| {
                    matches!(
                        child.layout,
                        MirLayout::Tuple(_)
                            | MirLayout::Record { .. }
                            | MirLayout::Option { .. }
                            | MirLayout::Result { .. }
                    )
                });
                let child_is_copy = self
                    .get(&field.ty)
                    .is_some_and(|child| child.ownership == MirOwnership::Copy);
                if child_is_composite
                    && !child_is_copy
                    && !self.materialize_glue_for(&field.ty, visiting)
                {
                    visiting.remove(id);
                    return false;
                }
                let Some(child) = self.get(&field.ty) else {
                    visiting.remove(id);
                    return false;
                };
                if !child.glue.supports_move_out()
                    || !child.glue.supports_clone()
                    || !child.glue.supports_drop()
                {
                    visiting.remove(id);
                    return false;
                }
                fields.push(MirDropGlueField {
                    index,
                    ty: field.ty.clone(),
                    glue: child.glue.drop,
                });
            }
            fields.reverse();
            plans.push(MirVariantDropGluePlan {
                variant: variant.id.clone(),
                fields,
            });
        }
        let Some(descriptor) = self.entries.get_mut(id) else {
            visiting.remove(id);
            return false;
        };
        if descriptor.ownership != MirOwnership::Copy {
            descriptor.glue = MirGlueContract {
                move_out: MirGlueKind::Aggregate,
                clone: MirGlueKind::Aggregate,
                drop: MirGlueKind::Aggregate,
            };
            descriptor.variant_drop_plan = Some(plans);
        }
        visiting.remove(id);
        descriptor.glue.move_out == MirGlueKind::Aggregate
    }

    pub fn validate_copy(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        if descriptor.ownership == MirOwnership::Copy {
            Ok(())
        } else {
            Err(format!(
                "copy instruction is invalid for ownership {:?} type '{}'",
                descriptor.ownership,
                ty.as_str()
            ))
        }
    }

    /// Validate the reference representation admitted by the first canonical
    /// borrow slice.  An immutable reference to a Copy scalar is represented
    /// as the scalar value in the reference backend/bytecode register; the
    /// pointer ABI and target identity remain explicit TypeDesc facts.  This
    /// is deliberately narrower than the surface language: mutable borrows,
    /// aggregate targets, and owned targets need an aliasing/storage contract
    /// before they can cross the MIR boundary.
    pub fn validate_reference_type(
        &self,
        reference_ty: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, String> {
        let descriptor = self.get(reference_ty).ok_or_else(|| {
            format!(
                "reference type '{}' is absent from MIR type catalog",
                reference_ty.as_str()
            )
        })?;
        let MirTypeKind::Reference { mutable: false } = descriptor.kind else {
            return Err(format!(
                "reference type '{}' is mutable or not a canonical shared reference",
                reference_ty.as_str()
            ));
        };
        let MirLayout::Pointer {
            target: Some(target),
        } = &descriptor.layout
        else {
            return Err(format!(
                "reference type '{}' has no checker-owned pointer target",
                reference_ty.as_str()
            ));
        };
        if descriptor.abi != MirAbiClass::Pointer
            || descriptor.ownership != MirOwnership::SharedBorrow
        {
            return Err(format!(
                "reference type '{}' has inconsistent pointer ABI/ownership",
                reference_ty.as_str()
            ));
        }
        let target_desc = self.get(target).ok_or_else(|| {
            format!(
                "reference type '{}' target '{}' is absent from MIR type catalog",
                reference_ty.as_str(),
                target.as_str()
            )
        })?;
        if !is_copy_scalar(target_desc) {
            return Err(format!(
                "reference target '{}' is outside the immutable Copy scalar borrow contract",
                target.as_str()
            ));
        }
        Ok(target.clone())
    }

    /// Validate a Borrow node's complete TypeDesc contract.  The source
    /// operand is the pointee value, while the result is the typed reference
    /// value; no backend is allowed to infer that relationship from a native
    /// pointer or a VM reference object.
    pub fn validate_borrow(
        &self,
        source_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
        mutable: bool,
    ) -> Result<(), String> {
        if mutable {
            return Err(
                "mutable Borrow is outside the canonical immutable Copy scalar contract".into(),
            );
        }
        let target = self.validate_reference_type(result_ty)?;
        if &target != source_ty {
            return Err(format!(
                "reference target '{}' disagrees with Borrow source type '{}'",
                target.as_str(),
                source_ty.as_str()
            ));
        }
        Ok(())
    }

    /// Validate a dereference projection against the same explicit reference
    /// target contract used by Borrow.
    pub fn validate_dereference(
        &self,
        reference_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
    ) -> Result<(), String> {
        let target = self.validate_reference_type(reference_ty)?;
        if &target != result_ty {
            return Err(format!(
                "dereference result type '{}' disagrees with reference target '{}'",
                result_ty.as_str(),
                target.as_str()
            ));
        }
        Ok(())
    }

    /// Resolve and validate a checked `Convert` against the closed canonical
    /// scalar conversion contract.  The returned contract is shared by the
    /// reference executor and every production adapter; no backend may grow
    /// its own ABI-pair table.
    pub fn validate_conversion(
        &self,
        source_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
    ) -> Result<MirConversionContract, String> {
        let source = self.get(source_ty).ok_or_else(|| {
            format!(
                "conversion source type '{}' is absent from MIR type catalog",
                source_ty.as_str()
            )
        })?;
        let result = self.get(result_ty).ok_or_else(|| {
            format!(
                "conversion result type '{}' is absent from MIR type catalog",
                result_ty.as_str()
            )
        })?;
        MirConversionContract::for_descriptors(source, result).ok_or_else(|| {
            format!(
                "conversion from ABI {:?}/layout {:?}/ownership {:?} to ABI {:?}/layout {:?}/ownership {:?} is outside the canonical contract (accepted: {})",
                source.abi,
                source.layout,
                source.ownership,
                result.abi,
                result.layout,
                result.ownership,
                MirConversionContract::accepted_description()
            )
        })
    }

    pub fn validate_value_operation(
        &self,
        result_ty: &ResolvedTypeId,
        source_ty: &ResolvedTypeId,
        operation: MirGlueOperation,
    ) -> Result<(), String> {
        if result_ty != source_ty {
            return Err(format!(
                "{:?} result type '{}' disagrees with source type '{}'",
                operation,
                result_ty.as_str(),
                source_ty.as_str()
            ));
        }
        self.validate_glue(source_ty, operation)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ResolvedTypeId, &MirTypeDesc)> {
        self.entries.iter()
    }

    #[cfg(test)]
    pub(crate) fn replace_for_test_only(&mut self, id: ResolvedTypeId, descriptor: MirTypeDesc) {
        self.entries.insert(id, descriptor);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Validate that an aggregate construction agrees with the checker-owned
    /// layout contract.  This is intentionally structural and backend-free:
    /// native offsets, bytecode registers, and drop glue are downstream
    /// concerns.  Unknown layouts fail closed instead of being treated as an
    /// untyped product.
    pub fn validate_aggregate(
        &self,
        result_ty: &ResolvedTypeId,
        kind: &crate::core::mir::MirAggregateKind,
        field_types: &[ResolvedTypeId],
    ) -> Result<(), String> {
        let descriptor = self
            .get(result_ty)
            .ok_or_else(|| format!("aggregate result type '{}' is absent", result_ty.as_str()))?;
        match (kind, &descriptor.layout) {
            (crate::core::mir::MirAggregateKind::Tuple, MirLayout::Tuple(elements)) => {
                if elements.len() != field_types.len() {
                    return Err(format!(
                        "tuple construction has {} fields but layout expects {}",
                        field_types.len(),
                        elements.len()
                    ));
                }
                for (index, (actual, expected)) in field_types.iter().zip(elements).enumerate() {
                    if actual != expected {
                        return Err(format!(
                            "tuple field {} type '{}' disagrees with layout type '{}'",
                            index,
                            actual.as_str(),
                            expected.as_str()
                        ));
                    }
                }
                Ok(())
            }
            (
                crate::core::mir::MirAggregateKind::Record { nominal, fields },
                MirLayout::Record {
                    nominal: expected_nominal,
                    fields: expected_fields,
                },
            ) => {
                if nominal != expected_nominal {
                    return Err(format!(
                        "record nominal '{}' disagrees with layout nominal '{}'",
                        nominal.as_str(),
                        expected_nominal.as_str()
                    ));
                }
                if fields.len() != field_types.len() {
                    return Err(format!(
                        "record construction has {} fields but layout expects {}",
                        field_types.len(),
                        fields.len()
                    ));
                }
                if fields.len() != expected_fields.len() {
                    return Err(format!(
                        "record construction names {} fields but declaration has {}",
                        fields.len(),
                        expected_fields.len()
                    ));
                }
                let mut seen = std::collections::BTreeSet::new();
                for (index, (field, actual)) in fields.iter().zip(field_types).enumerate() {
                    if !seen.insert(field) {
                        return Err(format!("record field {} is repeated", field.0));
                    }
                    let Some(expected) = expected_fields
                        .iter()
                        .find(|candidate| candidate.id == *field)
                    else {
                        return Err(format!(
                            "record field '{}' is absent from declaration",
                            field.0
                        ));
                    };
                    if actual != &expected.ty {
                        return Err(format!(
                            "record field {} type '{}' disagrees with layout type '{}'",
                            index,
                            actual.as_str(),
                            expected.ty.as_str()
                        ));
                    }
                }
                Ok(())
            }
            (kind, layout) => Err(format!(
                "aggregate kind {:?} does not match result layout {:?}",
                kind, layout
            )),
        }
    }

    /// Validate a List construction against the closed first container
    /// contract. Empty lists still carry an element TypeDesc through their
    /// result type, so they are checked just as strictly as non-empty lists.
    pub fn validate_list_construct(
        &self,
        result_ty: &ResolvedTypeId,
        element_types: &[ResolvedTypeId],
    ) -> Result<(), String> {
        self.validate_list_glue(result_ty, MirGlueOperation::MoveOut)?;
        let descriptor = self
            .get(result_ty)
            .ok_or_else(|| format!("List result type '{}' is absent", result_ty.as_str()))?;
        let MirLayout::List { element } = &descriptor.layout else {
            return Err(format!(
                "List result type '{}' has no canonical List layout",
                result_ty.as_str()
            ));
        };
        for (index, actual) in element_types.iter().enumerate() {
            if actual != element {
                return Err(format!(
                    "List element {} type '{}' disagrees with layout element type '{}'",
                    index,
                    actual.as_str(),
                    element.as_str()
                ));
            }
        }
        Ok(())
    }

    /// Validate a read-only List index projection. The List root is borrowed
    /// by `Project`; only the selected element is copied out. Every index,
    /// including a source-level constant, is represented by an explicit
    /// signed `i32`/`i64` scalar MIR value operand.
    pub fn validate_list_index(
        &self,
        base_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
        index_ty: &ResolvedTypeId,
    ) -> Result<(), String> {
        let base = self
            .get(base_ty)
            .ok_or_else(|| format!("List index base type '{}' is absent", base_ty.as_str()))?;
        let MirLayout::List { element } = &base.layout else {
            return Err(format!(
                "List index base type '{}' has no canonical List layout",
                base_ty.as_str()
            ));
        };
        self.validate_list_glue(base_ty, MirGlueOperation::MoveOut)?;
        if element != result_ty {
            return Err(format!(
                "List index result type '{}' disagrees with element type '{}'",
                result_ty.as_str(),
                element.as_str()
            ));
        }
        let result = self.get(result_ty).ok_or_else(|| {
            format!(
                "List index result type '{}' is absent from MIR type catalog",
                result_ty.as_str()
            )
        })?;
        if result.ownership != MirOwnership::Copy
            || result.glue
                != (MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
        {
            return Err(format!(
                "List index result type '{}' is not a Copy/no-op element",
                result_ty.as_str()
            ));
        }
        let index = self.get(index_ty).ok_or_else(|| {
            format!(
                "List index operand type '{}' is absent from MIR type catalog",
                index_ty.as_str()
            )
        })?;
        if index.ownership != MirOwnership::Copy
            || index.glue
                != (MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
            || !matches!(index.layout, MirLayout::Scalar)
            || !matches!(
                index.abi,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                }
            )
        {
            return Err(format!(
                "List index operand type '{}' is outside the signed Copy scalar contract",
                index_ty.as_str()
            ));
        }
        Ok(())
    }

    /// Materialize the complete TypeDesc receipt for a read-only List index
    /// projection.  This is the only constructor for the canonical index
    /// receipt; bytecode/native/verifier consumers must receive its result
    /// from the MIR instruction rather than reconstructing it.
    pub fn validated_list_index_projection_contract(
        &self,
        base_ty: &ResolvedTypeId,
        index_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
    ) -> Result<MirListIndexProjectionContract, String> {
        self.validate_list_index(base_ty, result_ty, index_ty)?;
        let base = self
            .get(base_ty)
            .ok_or_else(|| format!("List index base type '{}' is absent", base_ty.as_str()))?;
        let MirLayout::List { element } = &base.layout else {
            return Err(format!(
                "List index base type '{}' has no canonical List layout",
                base_ty.as_str()
            ));
        };
        Ok(MirListIndexProjectionContract {
            list_ty: base_ty.clone(),
            element_ty: element.clone(),
            index_ty: index_ty.clone(),
            result_ty: result_ty.clone(),
        })
    }

    /// Validate a materialized List index receipt against the checker-owned
    /// TypeDesc graph.  A stale or forged receipt is an invalid MIR shape and
    /// must be rejected before any backend is invoked.
    pub fn validate_list_index_projection_receipt(
        &self,
        base_ty: &ResolvedTypeId,
        index_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
        receipt: &MirListIndexProjectionContract,
    ) -> Result<(), String> {
        let expected =
            self.validated_list_index_projection_contract(base_ty, index_ty, result_ty)?;
        if receipt != &expected {
            return Err("List index projection receipt disagrees with TypeDesc".into());
        }
        Ok(())
    }

    /// Resolve the complete TypeDesc receipt for one canonical record field
    /// projection. The result type is part of the contract so a consumer
    /// cannot select a physically valid field with a semantically unrelated
    /// destination type. Ownership policy is deliberately checked by the
    /// caller because read and move projections have different contracts.
    pub fn validated_record_field_projection_contract(
        &self,
        base_ty: &ResolvedTypeId,
        field_id: &NodeId,
        result_ty: &ResolvedTypeId,
    ) -> Result<MirRecordProjectionContract, String> {
        let descriptor = self.get(base_ty).ok_or_else(|| {
            format!(
                "record projection base type '{}' is absent",
                base_ty.as_str()
            )
        })?;
        self.get(result_ty).ok_or_else(|| {
            format!(
                "record projection result type '{}' is absent",
                result_ty.as_str()
            )
        })?;
        let MirLayout::Record { nominal, fields } = &descriptor.layout else {
            return Err(format!(
                "record projection base type '{}' has no canonical record layout",
                base_ty.as_str()
            ));
        };
        let (field_index, field) = fields
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.id == *field_id)
            .ok_or_else(|| format!("record projection field '{}' is absent", field_id.0))?;
        if field.ty != *result_ty {
            return Err(format!(
                "record projection field '{}' type '{}' disagrees with result type '{}'",
                field_id.0,
                field.ty.as_str(),
                result_ty.as_str()
            ));
        }
        Ok(MirRecordProjectionContract {
            nominal: nominal.clone(),
            field: field.id.clone(),
            name: field.name.clone(),
            field_index,
            arity: fields.len(),
            field_ty: field.ty.clone(),
        })
    }

    /// Resolve the complete TypeDesc receipt for one canonical tuple field
    /// projection. The result type is part of the contract so a consumer
    /// cannot select a physically valid slot with a semantically unrelated
    /// destination type.
    pub fn validated_tuple_field_projection_contract(
        &self,
        base_ty: &ResolvedTypeId,
        field_index: usize,
        result_ty: &ResolvedTypeId,
    ) -> Result<MirTupleProjectionContract, String> {
        let descriptor = self.get(base_ty).ok_or_else(|| {
            format!(
                "tuple projection base type '{}' is absent",
                base_ty.as_str()
            )
        })?;
        self.get(result_ty).ok_or_else(|| {
            format!(
                "tuple projection result type '{}' is absent",
                result_ty.as_str()
            )
        })?;
        let MirLayout::Tuple(elements) = &descriptor.layout else {
            return Err(format!(
                "tuple projection base type '{}' has no canonical tuple layout",
                base_ty.as_str()
            ));
        };
        let field_ty = elements
            .get(field_index)
            .ok_or_else(|| format!("tuple projection index {} is out of bounds", field_index))?;
        if field_ty != result_ty {
            return Err(format!(
                "tuple projection result type '{}' disagrees with layout type '{}'",
                result_ty.as_str(),
                field_ty.as_str()
            ));
        }
        Ok(MirTupleProjectionContract {
            tuple_ty: base_ty.clone(),
            field_index,
            arity: elements.len(),
            field_ty: field_ty.clone(),
        })
    }

    pub fn projection_result_type(
        &self,
        base_ty: &ResolvedTypeId,
        projection: &crate::core::mir::MirProjection,
    ) -> Result<ResolvedTypeId, String> {
        let base = self
            .get(base_ty)
            .ok_or_else(|| format!("projection base type '{}' is absent", base_ty.as_str()))?;
        match (&base.layout, projection) {
            (MirLayout::Tuple(elements), crate::core::mir::MirProjection::Tuple(index)) => elements
                .get(*index)
                .cloned()
                .ok_or_else(|| format!("tuple projection index {} is out of bounds", index)),
            (MirLayout::Record { fields, .. }, crate::core::mir::MirProjection::Field(field)) => {
                fields
                    .iter()
                    .find(|candidate| candidate.id == *field)
                    .map(|candidate| candidate.ty.clone())
                    .ok_or_else(|| format!("record projection field '{}' is absent", field.0))
            }
            (MirLayout::List { element }, crate::core::mir::MirProjection::Index(_)) => {
                Ok(element.clone())
            }
            (_, crate::core::mir::MirProjection::Index(_)) => {
                Err("indexed projection requires a canonical List layout".into())
            }
            (_, crate::core::mir::MirProjection::Dereference) => {
                self.validate_reference_type(base_ty)
            }
            (layout, projection) => Err(format!(
                "projection {:?} does not match base layout {:?}",
                projection, layout
            )),
        }
    }

    pub fn validate_projection(
        &self,
        base_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
        projection: &crate::core::mir::MirProjection,
    ) -> Result<(), String> {
        let base = self
            .get(base_ty)
            .ok_or_else(|| format!("projection base type '{}' is absent", base_ty.as_str()))?;
        let result = self
            .get(result_ty)
            .ok_or_else(|| format!("projection result type '{}' is absent", result_ty.as_str()))?;
        match (&base.layout, projection) {
            (MirLayout::Tuple(_), crate::core::mir::MirProjection::Tuple(index)) => {
                let _ =
                    self.validated_tuple_field_projection_contract(base_ty, *index, result_ty)?;
                if result.ownership != MirOwnership::Copy {
                    return Err(format!(
                        "tuple projection result type '{}' is non-Copy and has no explicit move projection contract",
                        result_ty.as_str()
                    ));
                }
                Ok(())
            }
            (MirLayout::Record { .. }, crate::core::mir::MirProjection::Field(field)) => {
                let _ =
                    self.validated_record_field_projection_contract(base_ty, field, result_ty)?;
                if result.ownership != MirOwnership::Copy {
                    return Err(format!(
                        "record projection result type '{}' is non-Copy and has no explicit move projection contract",
                        result_ty.as_str()
                    ));
                }
                Ok(())
            }
            (MirLayout::List { element }, crate::core::mir::MirProjection::Index(_)) => {
                if element != result_ty {
                    return Err(format!(
                        "List index result type '{}' disagrees with element type '{}'",
                        result_ty.as_str(),
                        element.as_str()
                    ));
                }
                if result.ownership != MirOwnership::Copy {
                    return Err(format!(
                        "List index result type '{}' is non-Copy",
                        result_ty.as_str()
                    ));
                }
                Ok(())
            }
            (_, crate::core::mir::MirProjection::Index(_)) => {
                Err("indexed projection requires a canonical List layout".into())
            }
            (_, crate::core::mir::MirProjection::Dereference) => {
                self.validate_dereference(base_ty, result_ty)
            }
            (layout, projection) => Err(format!(
                "projection {:?} does not match base layout {:?}",
                projection, layout
            )),
        }
    }

    /// Validate the narrow ownership-safe field move projection contract.
    ///
    /// This operation consumes the complete record and returns one owned
    /// field. It is only sound without a residual value when every sibling is
    /// Copy; records with two or more non-Copy fields therefore remain
    /// unsupported until MIR carries an explicit residual/partial-move node.
    pub fn validate_move_projection(
        &self,
        base_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
        projection: &crate::core::mir::MirProjection,
    ) -> Result<(), String> {
        let base = self
            .get(base_ty)
            .ok_or_else(|| format!("move projection base type '{}' is absent", base_ty.as_str()))?;
        let result = self.get(result_ty).ok_or_else(|| {
            format!(
                "move projection result type '{}' is absent",
                result_ty.as_str()
            )
        })?;
        let MirLayout::Record { fields, .. } = &base.layout else {
            return Err("move projection requires a record product base".into());
        };
        if base.ownership == MirOwnership::Copy {
            return Err("move projection base must be non-Copy".into());
        }
        self.validate_glue(base_ty, MirGlueOperation::MoveOut)?;
        self.validate_aggregate_glue(base_ty, MirGlueOperation::Drop)?;
        let crate::core::mir::MirProjection::Field(field) = projection else {
            return Err("move projection currently supports direct record fields only".into());
        };
        let receipt = self
            .validated_record_field_projection_contract(base_ty, field, result_ty)
            .map_err(|message| message.replacen("record projection", "move projection", 1))?;
        if result.ownership == MirOwnership::Copy {
            return Err(format!(
                "move projection result type '{}' must be non-Copy",
                result_ty.as_str()
            ));
        }
        if result.glue.move_out != MirGlueKind::OwnedString
            || result.glue.clone != MirGlueKind::OwnedString
            || result.glue.drop != MirGlueKind::OwnedString
        {
            return Err(format!(
                "move projection result type '{}' requires owned-string field glue",
                result_ty.as_str()
            ));
        }
        for sibling in fields
            .iter()
            .filter(|candidate| candidate.id != receipt.field)
        {
            let sibling_desc = self.get(&sibling.ty).ok_or_else(|| {
                format!(
                    "move projection sibling field '{}' type '{}' is absent",
                    sibling.name,
                    sibling.ty.as_str()
                )
            })?;
            if sibling_desc.ownership != MirOwnership::Copy {
                return Err(format!(
                    "move projection of '{}' leaves non-Copy sibling field '{}' without a residual contract",
                    field.0, sibling.name
                ));
            }
        }
        Ok(())
    }

    /// Validate a place load one projection at a time.  This keeps lvalue
    /// projection type facts in MIR's TypeDesc contract instead of asking a
    /// backend to rediscover them from `ResolvedPlace` names.
    pub fn validate_place(
        &self,
        base_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
        projections: &[ResolvedProjection],
    ) -> Result<(), String> {
        let mut current_ty = base_ty.clone();
        for projection in projections {
            let mir_projection = match projection {
                ResolvedProjection::Field { field, .. } => {
                    crate::core::mir::MirProjection::Field(field.clone())
                }
                ResolvedProjection::Tuple { index, .. } => {
                    crate::core::mir::MirProjection::Tuple(*index)
                }
                ResolvedProjection::Index { .. } => {
                    return Err(
                        "indexed place projection has no canonical MIR layout contract".into(),
                    )
                }
                ResolvedProjection::Deref { .. } => crate::core::mir::MirProjection::Dereference,
            };
            self.validate_projection(&current_ty, projection.ty(), &mir_projection)?;
            current_ty = projection.ty().clone();
        }
        if &current_ty != result_ty {
            return Err(format!(
                "place load result type '{}' disagrees with projected type '{}'",
                result_ty.as_str(),
                current_ty.as_str()
            ));
        }
        Ok(())
    }

    /// Validate a record update.  The base and result are the same nominal
    /// record, while the explicit field set may be a declaration-order
    /// independent subset.  The base is still an explicit MIR operand so a
    /// future ownership pass can prove its consume/clone behavior.
    pub fn validate_record_update(
        &self,
        result_ty: &ResolvedTypeId,
        base_ty: &ResolvedTypeId,
        kind: &crate::core::mir::MirAggregateKind,
        field_types: &[ResolvedTypeId],
    ) -> Result<(), String> {
        let (result_nominal, result_fields) = match self
            .get(result_ty)
            .ok_or_else(|| {
                format!(
                    "record update result type '{}' is absent",
                    result_ty.as_str()
                )
            })?
            .layout
            .clone()
        {
            MirLayout::Record { nominal, fields } => (nominal, fields),
            layout => {
                return Err(format!(
                    "record update result layout {:?} is not a record",
                    layout
                ))
            }
        };
        let (base_nominal, base_fields) = match self
            .get(base_ty)
            .ok_or_else(|| format!("record update base type '{}' is absent", base_ty.as_str()))?
            .layout
            .clone()
        {
            MirLayout::Record { nominal, fields } => (nominal, fields),
            layout => {
                return Err(format!(
                    "record update base layout {:?} is not a record",
                    layout
                ))
            }
        };
        if result_nominal != base_nominal {
            return Err(format!(
                "record update base nominal '{}' disagrees with result nominal '{}'",
                base_nominal.as_str(),
                result_nominal.as_str()
            ));
        }
        let crate::core::mir::MirAggregateKind::Record { nominal, fields } = kind else {
            return Err("record update requires a record aggregate kind".into());
        };
        if nominal != &result_nominal {
            return Err(format!(
                "record update nominal '{}' disagrees with layout nominal '{}'",
                nominal.as_str(),
                result_nominal.as_str()
            ));
        }
        if fields.len() != field_types.len() {
            return Err(format!(
                "record update names {} fields but carries {} values",
                fields.len(),
                field_types.len()
            ));
        }
        if result_fields.len() != base_fields.len()
            || result_fields
                .iter()
                .zip(&base_fields)
                .any(|(left, right)| left != right)
        {
            return Err("record update base and result layouts disagree".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for (field, actual) in fields.iter().zip(field_types) {
            if !seen.insert(field) {
                return Err(format!("record update field '{}' is repeated", field.0));
            }
            let Some(expected) = result_fields
                .iter()
                .find(|candidate| candidate.id == *field)
            else {
                return Err(format!(
                    "record update field '{}' is absent from declaration",
                    field.0
                ));
            };
            if actual != &expected.ty {
                return Err(format!(
                    "record update field '{}' type '{}' disagrees with layout type '{}'",
                    field.0,
                    actual.as_str(),
                    expected.ty.as_str()
                ));
            }
        }
        Ok(())
    }

    /// Validate one canonical variant construction and return its TypeDesc
    /// variant descriptor.  The instruction carries stable variant/member
    /// identities; this method supplies the semantic discriminant and
    /// payload ABI from TypeDesc.
    pub fn validated_variant_construct(
        &self,
        result_ty: &ResolvedTypeId,
        nominal: &crate::core::NominalTypeId,
        variant: &NodeId,
        field_ids: &[NodeId],
        field_types: &[ResolvedTypeId],
    ) -> Result<&MirVariantDesc, String> {
        if field_ids.len() != field_types.len() {
            return Err(format!(
                "variant '{}' names {} fields but carries {} values",
                variant.0,
                field_ids.len(),
                field_types.len()
            ));
        }
        let (expected_nominal, variants) = self.variant_layout(result_ty).ok_or_else(|| {
            format!(
                "type '{}' has no canonical variant layout",
                result_ty.as_str()
            )
        })?;
        if nominal.as_str() != expected_nominal {
            return Err(format!(
                "variant nominal '{}' disagrees with canonical nominal '{}'",
                nominal.as_str(),
                expected_nominal
            ));
        }
        let expected = variants
            .iter()
            .find(|candidate| candidate.id == *variant)
            .ok_or_else(|| format!("variant '{}' is absent from TypeDesc", variant.0))?;
        validate_variant_fields(expected, field_ids, field_types).map(|()| expected)
    }

    /// Validate a variant construction and expose the payload shape admitted
    /// by the current native single-payload ABI.  The ordinary construction
    /// contract remains responsible for field identity and type equality;
    /// this narrower view only turns the already-proven descriptor into an
    /// optional canonical payload field.  Consumers must not rediscover
    /// payload arity with a target aggregate or a surface representation.
    pub fn validated_single_payload_variant_construct(
        &self,
        result_ty: &ResolvedTypeId,
        nominal: &crate::core::NominalTypeId,
        variant: &NodeId,
        field_ids: &[NodeId],
        field_types: &[ResolvedTypeId],
    ) -> Result<(&MirVariantDesc, Option<&MirFieldDesc>), String> {
        let variant_desc =
            self.validated_variant_construct(result_ty, nominal, variant, field_ids, field_types)?;
        if variant_desc.fields.len() > 1 {
            return Err(format!(
                "variant '{}' has {} payload fields; the single-payload native ABI allows at most one",
                variant_desc.name,
                variant_desc.fields.len()
            ));
        }
        Ok((variant_desc, variant_desc.fields.first()))
    }

    /// Validate one canonical variant construction without exposing the
    /// descriptor to callers that only need a pass/fail result.
    pub fn validate_variant_construct(
        &self,
        result_ty: &ResolvedTypeId,
        nominal: &crate::core::NominalTypeId,
        variant: &NodeId,
        field_ids: &[NodeId],
        field_types: &[ResolvedTypeId],
    ) -> Result<(), String> {
        self.validated_variant_construct(result_ty, nominal, variant, field_ids, field_types)
            .map(|_| ())
    }

    /// Validate and materialize the receipt for a read-only Option/Result
    /// predicate. Only flat Copy variants are admitted in this slice: the
    /// predicate itself does not move the source, but its physical tag must
    /// still have the same deterministic ABI as the canonical construction
    /// and switch contracts.
    pub fn validated_variant_predicate_contract(
        &self,
        result_ty: &ResolvedTypeId,
        variant_ty: &ResolvedTypeId,
        predicate: crate::core::mir::MirVariantPredicate,
    ) -> Result<MirVariantPredicateContract, String> {
        self.validate_flat_copy_variant(variant_ty)?;
        let result = self.get(result_ty).ok_or_else(|| {
            format!(
                "variant predicate result type '{}' is absent from MIR TypeDesc catalog",
                result_ty.as_str()
            )
        })?;
        if result.kind != MirTypeKind::Primitive(PrimitiveType::Bool)
            || result.abi != MirAbiClass::Bool
            || result.layout != MirLayout::Scalar
            || result.ownership != MirOwnership::Copy
            || result.glue
                != (MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
        {
            return Err("variant predicate result must be the canonical Copy bool TypeDesc".into());
        }

        let (expected_nominal, expected_variant, expected_name) = match predicate {
            crate::core::mir::MirVariantPredicate::IsSome => (
                "builtin:type:Option",
                "builtin:variant:Option::Some",
                "Some",
            ),
            crate::core::mir::MirVariantPredicate::IsNone => (
                "builtin:type:Option",
                "builtin:variant:Option::None",
                "None",
            ),
            crate::core::mir::MirVariantPredicate::IsOk => {
                ("builtin:type:Result", "builtin:variant:Result::Ok", "Ok")
            }
            crate::core::mir::MirVariantPredicate::IsErr => {
                ("builtin:type:Result", "builtin:variant:Result::Err", "Err")
            }
        };
        let (actual_nominal, variants) = self.variant_layout(variant_ty).ok_or_else(|| {
            format!(
                "variant predicate source type '{}' has no canonical Option/Result layout",
                variant_ty.as_str()
            )
        })?;
        if actual_nominal != expected_nominal {
            return Err(format!(
                "variant predicate {:?} requires '{}', found '{}'",
                predicate, expected_nominal, actual_nominal
            ));
        }
        let variant = variants
            .iter()
            .find(|candidate| candidate.id.0 == expected_variant && candidate.name == expected_name)
            .ok_or_else(|| {
                format!(
                    "variant predicate {:?} target '{}' is absent from TypeDesc",
                    predicate, expected_variant
                )
            })?;
        if variants.len() != 2 {
            return Err(format!(
                "variant predicate source '{}' has {} variants; canonical Option/Result predicate requires exactly two",
                variant_ty.as_str(),
                variants.len()
            ));
        }
        let alternate = variants
            .iter()
            .find(|candidate| candidate.id != variant.id)
            .ok_or_else(|| {
                "variant predicate alternate variant is absent from TypeDesc".to_string()
            })?;
        Ok(MirVariantPredicateContract {
            variant_ty: variant_ty.clone(),
            result_ty: result_ty.clone(),
            nominal: NominalTypeId::new(expected_nominal)
                .expect("static canonical variant nominal"),
            variant: variant.id.clone(),
            variant_name: variant.name.clone(),
            alternate_variant: alternate.id.clone(),
            alternate_variant_name: alternate.name.clone(),
            predicate,
            discriminant: variant.discriminant,
        })
    }

    /// Validate a materialized predicate receipt against the checker-owned
    /// TypeDesc graph. A stale discriminant, family, or predicate identity is
    /// invalid MIR and must be rejected before any backend reads a value.
    pub fn validate_variant_predicate_receipt(
        &self,
        result_ty: &ResolvedTypeId,
        variant_ty: &ResolvedTypeId,
        predicate: crate::core::mir::MirVariantPredicate,
        receipt: &MirVariantPredicateContract,
    ) -> Result<(), String> {
        let expected =
            self.validated_variant_predicate_contract(result_ty, variant_ty, predicate)?;
        if receipt != &expected {
            return Err("variant predicate receipt disagrees with TypeDesc".into());
        }
        Ok(())
    }

    /// Materialize the complete TypeDesc receipt for a direct call returning
    /// a flat Copy Option/Result. Calls returning other shapes deliberately
    /// return an error and remain outside this ABI island until their own
    /// call contract is promoted.
    pub fn validated_variant_call_abi_contract(
        &self,
        callee: &NodeId,
        type_arguments: &[ResolvedTypeId],
        parameter_types: &[ResolvedTypeId],
        result_ty: &ResolvedTypeId,
    ) -> Result<MirVariantCallAbiContract, String> {
        let payload_ty = self.validate_flat_copy_variant(result_ty)?;
        let (nominal, variants) = self.variant_layout(result_ty).ok_or_else(|| {
            format!(
                "variant call result '{}' has no canonical Option/Result layout",
                result_ty.as_str()
            )
        })?;
        if variants.is_empty() {
            return Err(format!(
                "variant call result '{}' has an empty canonical variant table",
                result_ty.as_str()
            ));
        }
        let variants = variants
            .iter()
            .map(|variant| MirVariantCallVariant {
                id: variant.id.clone(),
                name: variant.name.clone(),
                discriminant: variant.discriminant,
                payload_field: variant.fields.first().map(|field| field.id.clone()),
                payload_arity: variant.fields.len(),
            })
            .collect();
        Ok(MirVariantCallAbiContract {
            callee: callee.clone(),
            type_arguments: type_arguments.to_vec(),
            parameter_types: parameter_types.to_vec(),
            result_ty: result_ty.clone(),
            mode: MirVariantCallAbiMode::FlatCopy,
            return_mode: MirVariantCallReturnMode::FlatCopyMerge,
            payload_ty: payload_ty.clone(),
            payload_types: vec![payload_ty.clone()],
            nominal: NominalTypeId::new(nominal).expect("static canonical variant nominal"),
            variants,
        })
    }

    /// Materialize the narrow move-owned direct-call ABI for
    /// `Result<string, i32>`. The callee may return either canonical variant,
    /// but the ownership-bearing payload is merged only under the narrow
    /// path-exclusive return contract validated by Canonical MIR.
    pub fn validated_result_string_i32_call_abi_contract(
        &self,
        callee: &NodeId,
        type_arguments: &[ResolvedTypeId],
        parameter_types: &[ResolvedTypeId],
        result_ty: &ResolvedTypeId,
    ) -> Result<MirVariantCallAbiContract, String> {
        self.validate_result_string_i32_variant(result_ty)?;
        let (nominal, variants) = self.variant_layout(result_ty).ok_or_else(|| {
            format!(
                "variant call result '{}' has no canonical Result layout",
                result_ty.as_str()
            )
        })?;
        let (ok, error) = match self.get(result_ty).map(|descriptor| &descriptor.layout) {
            Some(MirLayout::Result { ok, error, .. }) => (ok.clone(), error.clone()),
            _ => {
                return Err(format!(
                    "variant call result '{}' is not the canonical move-owned Result layout",
                    result_ty.as_str()
                ));
            }
        };
        let variants = variants
            .iter()
            .map(|variant| MirVariantCallVariant {
                id: variant.id.clone(),
                name: variant.name.clone(),
                discriminant: variant.discriminant,
                payload_field: variant.fields.first().map(|field| field.id.clone()),
                payload_arity: variant.fields.len(),
            })
            .collect();
        Ok(MirVariantCallAbiContract {
            callee: callee.clone(),
            type_arguments: type_arguments.to_vec(),
            parameter_types: parameter_types.to_vec(),
            result_ty: result_ty.clone(),
            mode: MirVariantCallAbiMode::MoveOwned,
            return_mode: MirVariantCallReturnMode::OwnershipPathExclusiveMerge,
            payload_ty: ok.clone(),
            payload_types: vec![ok, error],
            nominal: NominalTypeId::new(nominal).expect("static canonical variant nominal"),
            variants,
        })
    }

    /// Validate a materialized call receipt against the callee result and
    /// checker-owned signature facts.  This is the shared pre-backend gate;
    /// backend adapters may add physical checks but may not replace it.
    pub fn validate_variant_call_abi_receipt(
        &self,
        callee: &NodeId,
        type_arguments: &[ResolvedTypeId],
        parameter_types: &[ResolvedTypeId],
        result_ty: &ResolvedTypeId,
        receipt: &MirVariantCallAbiContract,
    ) -> Result<(), String> {
        let expected = match receipt.mode {
            MirVariantCallAbiMode::FlatCopy => self.validated_variant_call_abi_contract(
                callee,
                type_arguments,
                parameter_types,
                result_ty,
            )?,
            MirVariantCallAbiMode::MoveOwned => self
                .validated_result_string_i32_call_abi_contract(
                    callee,
                    type_arguments,
                    parameter_types,
                    result_ty,
                )?,
        };
        if receipt != &expected {
            return Err("variant call ABI receipt disagrees with TypeDesc".into());
        }
        Ok(())
    }

    /// Validate the first backend-independent non-Copy variant contract.
    ///
    /// `Option<string>` is the initial move-owned variant island.  Its
    /// discriminants, stable payload identity, aggregate ABI, and complete
    /// Move/Clone/Drop proof are TypeDesc facts shared by every consumer.
    /// Result, nested payloads, and user-defined variants deliberately remain
    /// outside this narrow contract until their own consumer matrix is closed.
    pub fn validate_option_string_variant(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let (inner, variants) = match &descriptor.layout {
            MirLayout::Option { inner, variants } => (inner, variants),
            layout => {
                return Err(format!(
                    "layout {layout:?} is outside the canonical non-Copy Option<string> variant contract"
                ));
            }
        };
        if descriptor.kind != MirTypeKind::Option
            || descriptor.abi != MirAbiClass::Aggregate
            || descriptor.ownership != MirOwnership::Move
        {
            return Err(format!(
                "variant TypeDesc kind/ABI/ownership ({:?}/{:?}/{:?}) is outside the canonical non-Copy Option<string> variant contract",
                descriptor.kind, descriptor.abi, descriptor.ownership
            ));
        }
        let expected = MirGlueContract {
            move_out: MirGlueKind::Aggregate,
            clone: MirGlueKind::Aggregate,
            drop: MirGlueKind::Aggregate,
        };
        if descriptor.glue != expected
            || !descriptor.needs_drop_glue
            || !descriptor.needs_clone_glue
            || descriptor.variant_drop_plan.is_none()
        {
            return Err(
                "variant TypeDesc aggregate glue/drop plan is incomplete for the canonical non-Copy Option<string> variant contract".into(),
            );
        }
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            self.validate_glue(ty, operation)?;
        }
        if variants.len() != 2 {
            return Err(format!(
                "Option TypeDesc has {} variants; the canonical non-Copy Option<string> contract requires None and Some",
                variants.len()
            ));
        }
        let none = variants.iter().find(|variant| {
            variant.id.0 == "builtin:variant:Option::None"
                && variant.name == "None"
                && variant.discriminant == 0
                && variant.fields.is_empty()
        });
        let some = variants.iter().find(|variant| {
            variant.id.0 == "builtin:variant:Option::Some"
                && variant.name == "Some"
                && variant.discriminant == 1
                && variant.fields.len() == 1
        });
        if none.is_none() || some.is_none() {
            return Err(
                "Option TypeDesc variants do not match the canonical None/Some non-Copy contract"
                    .into(),
            );
        }
        let field = &some.expect("checked above").fields[0];
        if field.id.0 != "builtin:variant:Option::Some/payload:0" || field.ty != *inner {
            return Err(
                "Option Some payload identity/type disagrees with the canonical non-Copy contract"
                    .into(),
            );
        }
        self.validate_owned_string(inner)?;
        Ok(inner.clone())
    }

    /// Validate the first non-Copy Result variant contract.
    ///
    /// This deliberately admits exactly `Result<string, i32>`: the `Ok`
    /// payload is an owned StringHandle and the `Err` payload is a Copy signed
    /// i32.  The aggregate ABI, variant identities, discriminants, payload
    /// identities, and complete recursive Move/Clone/Drop proof are all
    /// TypeDesc facts.  Other Result payloads remain outside the verifier
    /// move-variant island until their own consumer matrix is closed.
    pub fn validate_result_string_i32_variant(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let variants = match &descriptor.layout {
            MirLayout::Result { variants, .. } => variants,
            layout => {
                return Err(format!(
                    "layout {layout:?} is outside the canonical non-Copy Result<string, i32> variant contract"
                ));
            }
        };
        if descriptor.kind != MirTypeKind::Result
            || descriptor.abi != MirAbiClass::Aggregate
            || descriptor.ownership != MirOwnership::Move
        {
            return Err(format!(
                "variant TypeDesc kind/ABI/ownership ({:?}/{:?}/{:?}) is outside the canonical non-Copy Result<string, i32> variant contract",
                descriptor.kind, descriptor.abi, descriptor.ownership
            ));
        }
        let expected = MirGlueContract {
            move_out: MirGlueKind::Aggregate,
            clone: MirGlueKind::Aggregate,
            drop: MirGlueKind::Aggregate,
        };
        if descriptor.glue != expected
            || !descriptor.needs_drop_glue
            || !descriptor.needs_clone_glue
            || descriptor.variant_drop_plan.is_none()
        {
            return Err(
                "variant TypeDesc aggregate glue/drop plan is incomplete for the canonical non-Copy Result<string, i32> variant contract".into(),
            );
        }
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            self.validate_glue(ty, operation)?;
        }
        if variants.len() != 2 {
            return Err(format!(
                "Result TypeDesc has {} variants; the canonical non-Copy Result<string, i32> contract requires Ok and Err",
                variants.len()
            ));
        }
        let ok = variants.iter().find(|variant| {
            variant.id.0 == "builtin:variant:Result::Ok"
                && variant.name == "Ok"
                && variant.discriminant == 0
                && variant.fields.len() == 1
        });
        let err = variants.iter().find(|variant| {
            variant.id.0 == "builtin:variant:Result::Err"
                && variant.name == "Err"
                && variant.discriminant == 1
                && variant.fields.len() == 1
        });
        let (Some(ok), Some(err)) = (ok, err) else {
            return Err(
                "Result TypeDesc variants do not match the canonical Ok/Err non-Copy contract"
                    .into(),
            );
        };
        let ok_field = &ok.fields[0];
        if ok_field.id.0 != "builtin:variant:Result::Ok/payload:0" {
            return Err(
                "Result Ok payload identity disagrees with the canonical non-Copy contract".into(),
            );
        }
        self.validate_owned_string(&ok_field.ty)?;

        let err_field = &err.fields[0];
        if err_field.id.0 != "builtin:variant:Result::Err/payload:0" {
            return Err(
                "Result Err payload identity disagrees with the canonical non-Copy contract".into(),
            );
        }
        let err_descriptor = self.get(&err_field.ty).ok_or_else(|| {
            format!(
                "Result Err payload type '{}' is absent from MIR type catalog",
                err_field.ty.as_str()
            )
        })?;
        if err_descriptor.kind != MirTypeKind::Primitive(PrimitiveType::I32)
            || err_descriptor.abi
                != (MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            || err_descriptor.layout != MirLayout::Scalar
            || err_descriptor.ownership != MirOwnership::Copy
        {
            return Err("Result Err payload must be the canonical Copy signed i32 TypeDesc".into());
        }
        self.validate_copy_scalar(&err_field.ty)
            .map_err(|message| {
                format!(
                "Result Err payload is outside the canonical Copy signed i32 contract: {message}"
            )
            })?;
        Ok(())
    }

    /// Validate either currently admitted non-Copy built-in variant shape.
    /// Consumers use this single TypeDesc boundary so they cannot silently
    /// widen a verifier switch/constructor by accepting a new layout in one
    /// backend only.
    pub fn validate_non_copy_variant_contract(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let Some(descriptor) = self.get(ty) else {
            return Err(format!(
                "type '{}' is absent from MIR type catalog",
                ty.as_str()
            ));
        };
        match &descriptor.layout {
            MirLayout::Option { .. } => self
                .validate_option_string_variant(ty)
                .map(|_| ())
                .map_err(|message| {
                    format!(
                        "type '{}' is outside the canonical non-Copy Option<string> variant contract: {message}",
                        ty.as_str()
                    )
                }),
            MirLayout::Result { .. } => self
                .validate_result_string_i32_variant(ty)
                .map_err(|message| {
                    format!(
                        "type '{}' is outside the canonical non-Copy Result<string, i32> variant contract: {message}",
                        ty.as_str()
                    )
                }),
            _ => Err(format!(
                "type '{}' has no canonical non-Copy Option/Result variant layout",
                ty.as_str()
            )),
        }
    }

    /// Return the canonical active/inactive variant descriptors for the
    /// materialized non-Copy Option<string> ABI.  The validator above proves
    /// the complete recursive MoveOut/Clone/Drop contract and exact
    /// None/Some shape; this method only exposes those stable descriptors to
    /// glue emitters, so they cannot rediscover branches from payload arity.
    pub fn validated_option_string_variants(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<(&MirVariantDesc, &MirVariantDesc, ResolvedTypeId), String> {
        let inner = self.validate_option_string_variant(ty)?;
        let descriptor = self
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        let variants = match &descriptor.layout {
            MirLayout::Option { variants, .. } => variants,
            layout => {
                return Err(format!(
                    "layout {layout:?} is outside the canonical non-Copy Option<string> variant contract"
                ));
            }
        };
        let none = variants
            .iter()
            .find(|variant| variant.id.0 == "builtin:variant:Option::None")
            .ok_or_else(|| "canonical Option<string> None descriptor is absent".to_string())?;
        let some = variants
            .iter()
            .find(|variant| variant.id.0 == "builtin:variant:Option::Some")
            .ok_or_else(|| "canonical Option<string> Some descriptor is absent".to_string())?;
        Ok((none, some, inner))
    }

    /// Return the canonical None/Some descriptors and the already-validated
    /// Some payload field for Option<string> glue consumers.  Clone/drop
    /// emitters must consume this field fact instead of rechecking payload
    /// arity or type against a target representation.
    pub fn validated_option_string_payload(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<(&MirVariantDesc, &MirVariantDesc, &MirFieldDesc), String> {
        let (none, some, _) = self.validated_option_string_variants(ty)?;
        let payload = some
            .fields
            .first()
            .ok_or_else(|| "canonical Option<string> Some payload field is absent".to_string())?;
        Ok((none, some, payload))
    }

    /// Return the complete Option<string> variant/drop contract for a drop
    /// emitter.  This selects stable None/Some descriptors and proves their
    /// canonical drop-plan shapes before a backend can branch on the tag.
    pub fn validated_option_string_drop_contract(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<
        (
            &MirVariantDesc,
            &MirVariantDesc,
            &MirVariantDropGluePlan,
            &MirVariantDropGluePlan,
        ),
        String,
    > {
        let (none, some, _) = self.validated_option_string_payload(ty)?;
        let payload_plan = self.validated_variant_drop_plan(ty, &some.id)?;
        let empty_plan = self.validated_variant_drop_plan(ty, &none.id)?;
        if payload_plan.fields.len() != 1
            || payload_plan.fields[0].index != 0
            || !empty_plan.fields.is_empty()
        {
            return Err(
                "Option<string> variant drop plans are outside the canonical one-payload ABI"
                    .into(),
            );
        }
        Ok((none, some, payload_plan, empty_plan))
    }

    /// Resolve one active variant from the already-validated narrow
    /// Option<string> contract.  Consumers must select by stable identity;
    /// payload arity and physical representation are not variant semantics.
    pub fn validated_option_string_variant(
        &self,
        ty: &ResolvedTypeId,
        variant_id: &NodeId,
    ) -> Result<&MirVariantDesc, String> {
        let (none, some, _) = self.validated_option_string_variants(ty)?;
        if variant_id == &none.id {
            return Ok(none);
        }
        if variant_id == &some.id {
            return Ok(some);
        }
        Err(format!(
            "variant '{}' is absent from TypeDesc for canonical Option<string> type '{}'",
            variant_id.0,
            ty.as_str()
        ))
    }

    /// Return the canonical nominal label and discriminant/payload table for
    /// the built-in Option/Result families.  User enum layouts remain
    /// fail-closed until their schema is promoted into this catalog.
    pub fn variant_layout(&self, ty: &ResolvedTypeId) -> Option<(&str, &[MirVariantDesc])> {
        let descriptor = self.get(ty)?;
        match &descriptor.layout {
            MirLayout::Option { variants, .. } => {
                Some(("builtin:type:Option", variants.as_slice()))
            }
            MirLayout::Result { variants, .. } => {
                Some(("builtin:type:Result", variants.as_slice()))
            }
            _ => None,
        }
    }

    pub fn variant(&self, ty: &ResolvedTypeId, variant: &NodeId) -> Option<&MirVariantDesc> {
        self.variant_layout(ty)?
            .1
            .iter()
            .find(|candidate| candidate.id == *variant)
    }

    /// Resolve one checked variant-switch case from the canonical family.
    ///
    /// Switch validation owns exhaustiveness and duplicate-case policy; this
    /// companion returns the stable nominal label and descriptor that a
    /// backend needs to encode the already-validated case.  Keeping the
    /// lookup here prevents a consumer from rebuilding variant identity from
    /// its own tag table or runtime representation.
    pub fn validated_variant_switch_case(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        variant_id: &NodeId,
    ) -> Result<(&str, &MirVariantDesc), String> {
        let (nominal, variants) = self.variant_layout(scrutinee_ty).ok_or_else(|| {
            format!(
                "switch scrutinee type '{}' has no canonical variant layout",
                scrutinee_ty.as_str()
            )
        })?;
        let variant = variants
            .iter()
            .find(|candidate| candidate.id == *variant_id)
            .ok_or_else(|| {
                format!(
                    "variant switch case '{}' is absent from TypeDesc",
                    variant_id.0
                )
            })?;
        Ok((nominal, variant))
    }

    /// Resolve the complete TypeDesc receipt for one variant-switch payload
    /// projection. A `MirSwitchBinding` carries stable variant-field
    /// identity, while the physical payload index, arity, nominal family and
    /// field type are layout facts. Keeping this lookup here prevents
    /// reference, bytecode, native, and verifier consumers from independently
    /// re-deriving the active-variant ABI.
    pub fn validated_variant_payload_projection_contract(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        variant_id: &NodeId,
        field_id: &NodeId,
        result_ty: &ResolvedTypeId,
    ) -> Result<MirVariantProjectionContract, String> {
        self.get(result_ty).ok_or_else(|| {
            format!(
                "variant payload projection result type '{}' is absent",
                result_ty.as_str()
            )
        })?;
        let (nominal, variant) = self.validated_variant_switch_case(scrutinee_ty, variant_id)?;
        let nominal = NominalTypeId::new(nominal.to_string()).map_err(|_| {
            format!(
                "variant payload projection nominal '{}' has invalid identity",
                nominal
            )
        })?;
        let (field_index, field) = variant
            .fields
            .iter()
            .enumerate()
            .find(|(_, field)| field.id == *field_id)
            .ok_or_else(|| {
                format!(
                    "variant payload projection field '{}' is absent from variant '{}'",
                    field_id.0, variant.name
                )
            })?;
        if field.ty != *result_ty {
            return Err(format!(
                "variant payload projection field '{}' type '{}' disagrees with result type '{}'",
                field_id.0,
                field.ty.as_str(),
                result_ty.as_str()
            ));
        }
        Ok(MirVariantProjectionContract {
            nominal,
            variant: variant.id.clone(),
            field: field.id.clone(),
            field_index,
            arity: variant.fields.len(),
            field_ty: field.ty.clone(),
        })
    }

    /// Validate the payload projection made by one variant-switch binding.
    /// The returned index is declaration order in the canonical TypeDesc;
    /// callers that need the full identity/arity receipt should use
    /// [`Self::validated_variant_payload_projection_contract`].
    pub fn validate_variant_payload_projection(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        variant_id: &NodeId,
        field_id: &NodeId,
        result_ty: &ResolvedTypeId,
    ) -> Result<usize, String> {
        self.validated_variant_payload_projection_contract(
            scrutinee_ty,
            variant_id,
            field_id,
            result_ty,
        )
        .map(|contract| contract.field_index)
    }

    /// Validate a payload receipt already materialized in canonical MIR.
    /// This is the program-boundary check: consumers may carry the receipt
    /// forward, but must not regenerate it from the TypeDesc independently.
    pub fn validate_variant_payload_projection_receipt(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        variant_id: &NodeId,
        result_ty: &ResolvedTypeId,
        receipt: &MirVariantProjectionContract,
    ) -> Result<(), String> {
        let expected = self.validated_variant_payload_projection_contract(
            scrutinee_ty,
            variant_id,
            &receipt.field,
            result_ty,
        )?;
        if receipt != &expected {
            return Err(format!(
                "variant payload projection receipt for field '{}' disagrees with TypeDesc",
                receipt.field.0
            ));
        }
        Ok(())
    }

    /// Validate a switch over a canonical variant family.  Exhaustiveness is
    /// part of the MIR contract: either every discriminant is listed exactly
    /// once or the final arm is an explicit default.
    pub fn validate_switch(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        arms: &[crate::core::mir::MirSwitchArm],
    ) -> Result<(), String> {
        let Some((_, variants)) = self.variant_layout(scrutinee_ty) else {
            if arms
                .iter()
                .any(|arm| matches!(arm.case, crate::core::mir::MirSwitchCase::Variant(_)))
            {
                return Err(format!(
                    "switch scrutinee type '{}' has no canonical variant layout",
                    scrutinee_ty.as_str()
                ));
            }
            return Ok(());
        };
        if arms.is_empty() {
            return Err("variant switch has no arms".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut has_default = false;
        for (index, arm) in arms.iter().enumerate() {
            match &arm.case {
                crate::core::mir::MirSwitchCase::Variant(variant) => {
                    if has_default {
                        return Err("variant switch has an arm after its default".into());
                    }
                    self.validated_variant_switch_case(scrutinee_ty, variant)
                        .map(|_| ())?;
                    if !seen.insert(variant) {
                        return Err(format!("variant switch case '{}' is repeated", variant.0));
                    }
                }
                crate::core::mir::MirSwitchCase::Default => {
                    if has_default {
                        return Err("variant switch has more than one default arm".into());
                    }
                    if index + 1 != arms.len() {
                        return Err("variant switch default arm must be last".into());
                    }
                    if !arm.bindings.is_empty() {
                        return Err("variant switch default arm cannot bind a payload".into());
                    }
                    has_default = true;
                }
                crate::core::mir::MirSwitchCase::Literal(_) => {
                    return Err("variant switch cannot use a literal case".into());
                }
            }
        }
        if !has_default && seen.len() != variants.len() {
            let missing = variants
                .iter()
                .filter(|candidate| !seen.contains(&candidate.id))
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "variant switch is not exhaustive; missing: {missing}"
            ));
        }
        Ok(())
    }

    /// Validate a consuming switch over a non-Copy built-in variant.  Every
    /// active payload field is either moved into an arm binding or released
    /// by the variant drop plan; a backend must not treat this as a read-only
    /// tag dispatch.
    pub fn validate_switch_move(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        arms: &[crate::core::mir::MirSwitchArm],
    ) -> Result<(), String> {
        let descriptor = self.get(scrutinee_ty).ok_or_else(|| {
            format!(
                "switch-move scrutinee type '{}' is absent from TypeDesc",
                scrutinee_ty.as_str()
            )
        })?;
        if descriptor.ownership == MirOwnership::Copy {
            return Err(format!(
                "switch-move scrutinee type '{}' is Copy; use Switch",
                scrutinee_ty.as_str()
            ));
        }
        if !matches!(
            descriptor.layout,
            MirLayout::Option { .. } | MirLayout::Result { .. }
        ) {
            return Err(format!(
                "switch-move scrutinee type '{}' has no canonical Option/Result layout",
                scrutinee_ty.as_str()
            ));
        }
        self.validate_glue(scrutinee_ty, MirGlueOperation::MoveOut)?;
        self.validate_glue(scrutinee_ty, MirGlueOperation::Drop)?;
        self.validate_switch(scrutinee_ty, arms)?;
        for arm in arms {
            let crate::core::mir::MirSwitchCase::Variant(variant_id) = &arm.case else {
                continue;
            };
            let (_, variant) = self.validated_variant_switch_case(scrutinee_ty, variant_id)?;
            let mut bound_fields = BTreeSet::new();
            for binding in &arm.bindings {
                if !bound_fields.insert(&binding.projection.field) {
                    return Err(format!(
                        "switch-move binding field '{}' is repeated",
                        binding.projection.field.0
                    ));
                }
                if !variant
                    .fields
                    .iter()
                    .any(|field| field.id == binding.projection.field)
                {
                    return Err(format!(
                        "switch-move binding field '{}' is absent from variant '{}'",
                        binding.projection.field.0, variant.name
                    ));
                }
            }
        }
        Ok(())
    }

    /// Validate the complete TypeDesc-side contract for a consuming variant
    /// switch.  `validate_switch_move` proves that the scrutinee is a
    /// non-Copy built-in variant and that its CFG is exhaustive; this
    /// companion additionally proves every materialized payload binding
    /// receipt and the child MoveOut glue for fields transferred into an arm.
    /// Fields without a binding are released by the already-validated active
    /// variant drop plan.  Consumers must call this entry point instead of
    /// independently deciding whether a binding is a move, clone, or drop.
    pub fn validate_variant_switch_move_contract(
        &self,
        scrutinee_ty: &ResolvedTypeId,
        arms: &[crate::core::mir::MirSwitchArm],
    ) -> Result<(), String> {
        self.validate_switch_move(scrutinee_ty, arms)?;
        for arm in arms {
            let crate::core::mir::MirSwitchCase::Variant(variant_id) = &arm.case else {
                continue;
            };
            let (_, variant) = self.validated_variant_switch_case(scrutinee_ty, variant_id)?;
            let mut bound_fields = BTreeSet::new();
            for binding in &arm.bindings {
                self.validate_variant_payload_projection_receipt(
                    scrutinee_ty,
                    variant_id,
                    &binding.projection.field_ty,
                    &binding.projection,
                )?;
                let field = variant
                    .fields
                    .get(binding.projection.field_index)
                    .ok_or_else(|| {
                        format!(
                            "switch-move binding field '{}' is outside variant '{}'",
                            binding.projection.field.0, variant.name
                        )
                    })?;
                if !bound_fields.insert(field.id.clone()) {
                    return Err(format!(
                        "switch-move binding field '{}' is repeated",
                        field.id.0
                    ));
                }
                self.validate_glue(&field.ty, MirGlueOperation::MoveOut)
                    .map_err(|message| {
                        format!(
                            "switch-move bound field '{}' has no canonical MoveOut glue: {message}",
                            field.id.0
                        )
                    })?;
            }
        }
        Ok(())
    }

    pub fn canonical_text(&self) -> String {
        let mut output = format!("mir.type-catalog {MIR_TYPE_DESC_SCHEMA_VERSION}\n");
        for (id, descriptor) in &self.entries {
            output.push_str(&format!(
                "{} kind={:?} layout={:?} ownership={:?} abi={:?} glue={:?} drop_plan={:?} variant_drop_plan={:?} drop={} clone={}\n",
                id.as_str(),
                descriptor.kind,
                descriptor.layout,
                descriptor.ownership,
                descriptor.abi,
                descriptor.glue,
                descriptor.drop_plan,
                descriptor.variant_drop_plan,
                descriptor.needs_drop_glue,
                descriptor.needs_clone_glue,
            ));
        }
        output
    }

    /// Return the ABI-facing portion of the TypeDesc contract in a stable
    /// order. This is separate from `canonical_text` so a route receipt can
    /// distinguish an ABI/layout/glue change from other descriptor facts
    /// without either backend reconstructing the contract.
    pub fn abi_canonical_text(&self) -> String {
        let mut output = format!("mir.abi-catalog {MIR_TYPE_DESC_SCHEMA_VERSION}\n");
        for (id, descriptor) in &self.entries {
            output.push_str(&format!(
                "{} layout={:?} abi={:?} glue={:?} drop={} clone={}\n",
                id.as_str(),
                descriptor.layout,
                descriptor.abi,
                descriptor.glue,
                descriptor.needs_drop_glue,
                descriptor.needs_clone_glue,
            ));
        }
        output
    }
}

fn validate_variant_fields(
    variant: &MirVariantDesc,
    field_ids: &[NodeId],
    field_types: &[ResolvedTypeId],
) -> Result<(), String> {
    if variant.fields.len() != field_ids.len() {
        return Err(format!(
            "variant '{}' expects {} payload fields but carries {}",
            variant.name,
            variant.fields.len(),
            field_ids.len()
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (field_id, actual) in field_ids.iter().zip(field_types) {
        if !seen.insert(field_id) {
            return Err(format!(
                "variant payload field '{}' is repeated",
                field_id.0
            ));
        }
        let expected = variant
            .fields
            .iter()
            .find(|field| field.id == *field_id)
            .ok_or_else(|| format!("variant payload field '{}' is absent", field_id.0))?;
        if actual != &expected.ty {
            return Err(format!(
                "variant payload field '{}' type '{}' disagrees with layout type '{}'",
                field_id.0,
                actual.as_str(),
                expected.ty.as_str()
            ));
        }
    }
    if variant.fields.iter().any(|field| !seen.contains(&field.id)) {
        return Err(format!("variant '{}' payload is incomplete", variant.name));
    }
    Ok(())
}

fn primitive_layout(primitive: PrimitiveType) -> MirLayout {
    match primitive {
        PrimitiveType::Unit => MirLayout::Unit,
        PrimitiveType::String => MirLayout::Handle,
        PrimitiveType::I8
        | PrimitiveType::I16
        | PrimitiveType::I32
        | PrimitiveType::I64
        | PrimitiveType::I128
        | PrimitiveType::U8
        | PrimitiveType::U16
        | PrimitiveType::U32
        | PrimitiveType::U64
        | PrimitiveType::U128
        | PrimitiveType::Isize
        | PrimitiveType::Usize
        | PrimitiveType::F32
        | PrimitiveType::F64
        | PrimitiveType::Bool
        | PrimitiveType::Char => MirLayout::Scalar,
    }
}

fn primitive_abi(primitive: PrimitiveType) -> MirAbiClass {
    match primitive {
        PrimitiveType::I8 => MirAbiClass::Integer {
            bits: 8,
            signed: true,
        },
        PrimitiveType::I16 => MirAbiClass::Integer {
            bits: 16,
            signed: true,
        },
        PrimitiveType::I32 => MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
        PrimitiveType::I64 | PrimitiveType::Isize => MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
        PrimitiveType::I128 => MirAbiClass::Integer {
            bits: 128,
            signed: true,
        },
        PrimitiveType::U8 => MirAbiClass::Integer {
            bits: 8,
            signed: false,
        },
        PrimitiveType::U16 => MirAbiClass::Integer {
            bits: 16,
            signed: false,
        },
        PrimitiveType::U32 => MirAbiClass::Integer {
            bits: 32,
            signed: false,
        },
        PrimitiveType::U64 | PrimitiveType::Usize => MirAbiClass::Integer {
            bits: 64,
            signed: false,
        },
        PrimitiveType::U128 => MirAbiClass::Integer {
            bits: 128,
            signed: false,
        },
        PrimitiveType::F32 => MirAbiClass::Float { bits: 32 },
        PrimitiveType::F64 => MirAbiClass::Float { bits: 64 },
        PrimitiveType::Bool => MirAbiClass::Bool,
        PrimitiveType::Char => MirAbiClass::Char,
        PrimitiveType::String => MirAbiClass::StringHandle,
        PrimitiveType::Unit => MirAbiClass::Unit,
    }
}

fn is_copy_scalar(descriptor: &MirTypeDesc) -> bool {
    descriptor.ownership == MirOwnership::Copy
        && descriptor.glue
            == (MirGlueContract {
                move_out: MirGlueKind::Noop,
                clone: MirGlueKind::Noop,
                drop: MirGlueKind::Noop,
            })
        && matches!(descriptor.layout, MirLayout::Scalar)
}

fn ownership_for(
    id: &ResolvedTypeId,
    table: &ResolvedTypeTable,
    visiting: &mut Vec<ResolvedTypeId>,
) -> MirOwnership {
    if visiting.iter().any(|seen| seen == id) {
        return MirOwnership::Move;
    }
    let Some(ty) = table.get(id) else {
        return MirOwnership::Move;
    };
    visiting.push(id.clone());
    let ownership = match ty {
        ResolvedType::Primitive(PrimitiveType::String) => MirOwnership::Move,
        ResolvedType::Primitive(_) | ResolvedType::GenericParameter(_) => MirOwnership::Copy,
        ResolvedType::Nominal { is_linear, .. } => {
            if *is_linear {
                MirOwnership::Linear
            } else {
                MirOwnership::Move
            }
        }
        ResolvedType::Capability(_) => MirOwnership::Linear,
        ResolvedType::Reference { mutable, .. } => {
            if *mutable {
                MirOwnership::Move
            } else {
                MirOwnership::SharedBorrow
            }
        }
        ResolvedType::RawPointer { .. } => MirOwnership::Copy,
        ResolvedType::Ownership { kind, .. } => match kind {
            OwnershipTypeKind::Shared => MirOwnership::SharedBorrow,
            OwnershipTypeKind::Weak => MirOwnership::WeakBorrow,
        },
        ResolvedType::Tuple(elements) => aggregate_ownership(elements, table, visiting),
        ResolvedType::Option(inner)
        | ResolvedType::CBuffer(inner)
        | ResolvedType::Slice(inner)
        | ResolvedType::Array { element: inner, .. }
        | ResolvedType::Newtype { inner, .. } => ownership_for(inner, table, visiting),
        ResolvedType::Result { ok, error } => combine_ownership(
            ownership_for(ok, table, visiting),
            ownership_for(error, table, visiting),
        ),
        ResolvedType::Function { .. } => MirOwnership::Copy,
        ResolvedType::FlowStateSet { .. } => MirOwnership::Linear,
        ResolvedType::Trait { .. } | ResolvedType::DynamicAny { .. } => MirOwnership::Move,
    };
    visiting.pop();
    ownership
}

fn aggregate_ownership(
    elements: &[ResolvedTypeId],
    table: &ResolvedTypeTable,
    visiting: &mut Vec<ResolvedTypeId>,
) -> MirOwnership {
    elements
        .iter()
        .fold(MirOwnership::Copy, |current, element| {
            combine_ownership(current, ownership_for(element, table, visiting))
        })
}

fn combine_ownership(left: MirOwnership, right: MirOwnership) -> MirOwnership {
    use MirOwnership::{Copy, Linear, Move, SharedBorrow, WeakBorrow};
    match (left, right) {
        (Linear, _) | (_, Linear) => Linear,
        (Move, _) | (_, Move) => Move,
        (SharedBorrow, _) | (_, SharedBorrow) => SharedBorrow,
        (WeakBorrow, _) | (_, WeakBorrow) => WeakBorrow,
        (Copy, Copy) => Copy,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MirAbiClass, MirBuiltinContract, MirBuiltinEffect, MirBuiltinKind, MirGlueKind,
        MirGlueOperation, MirLayout, MirOwnership, MirTypeCatalog, MirTypeKind,
    };
    use crate::core::ir::{PrimitiveType, ResolvedType, ResolvedTypeTable};
    use crate::core::mir::{MirAggregateKind, MirListOperation, MirProjection, MirSetOperation};

    #[test]
    fn materializes_scalar_abi_and_copy_ownership() {
        let mut table = ResolvedTypeTable::new();
        let id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("type");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let descriptor = catalog.get(&id).expect("descriptor");
        assert_eq!(descriptor.ownership, MirOwnership::Copy);
        assert_eq!(
            descriptor.abi,
            MirAbiClass::Integer {
                bits: 32,
                signed: true
            }
        );
        assert!(!descriptor.needs_drop_glue);
    }

    #[test]
    fn println_bool_contract_is_copy_unit_stdout_line() {
        let contract = MirBuiltinContract::for_kind(MirBuiltinKind::PrintlnBool);

        assert_eq!(contract.name, "println");
        assert_eq!(contract.arity, 1);
        assert_eq!(contract.input_abi, MirAbiClass::Bool);
        assert!(contract.requires_copy);
        assert!(contract.result_must_be_unit);
        assert_eq!(contract.effect, MirBuiltinEffect::StdoutLine);
        assert!(contract.accepts_layout(&MirLayout::Scalar));
        assert!(contract.accepts_abi(MirAbiClass::Bool));
        assert!(!contract.accepts_abi(MirAbiClass::Integer {
            bits: 64,
            signed: true,
        }));
    }

    #[test]
    fn println_int_contract_is_copy_unit_stdout_line_for_both_widths() {
        let contract = MirBuiltinContract::for_kind(MirBuiltinKind::PrintlnInt);

        assert_eq!(contract.name, "println");
        assert_eq!(contract.arity, 1);
        assert!(contract.requires_copy);
        assert!(contract.result_must_be_unit);
        assert_eq!(contract.effect, MirBuiltinEffect::StdoutLine);
        assert!(contract.accepts_abi(MirAbiClass::Integer {
            bits: 32,
            signed: true,
        }));
        assert!(contract.accepts_abi(MirAbiClass::Integer {
            bits: 64,
            signed: true,
        }));
        assert!(!contract.accepts_abi(MirAbiClass::Integer {
            bits: 32,
            signed: false,
        }));
        assert!(!contract.accepts_abi(MirAbiClass::Bool));
    }

    #[test]
    fn reference_typedesc_carries_target_and_rejects_mutable_or_mismatched_borrows() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let reference_id = table
            .intern_resolved(ResolvedType::Reference {
                lifetime: None,
                mutable: false,
                target: i32_id.clone(),
            })
            .expect("shared reference");
        let mutable_reference_id = table
            .intern_resolved(ResolvedType::Reference {
                lifetime: None,
                mutable: true,
                target: i32_id.clone(),
            })
            .expect("mutable reference");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");

        assert_eq!(
            catalog
                .get(&reference_id)
                .expect("reference descriptor")
                .layout,
            MirLayout::Pointer {
                target: Some(i32_id.clone())
            }
        );
        assert_eq!(
            catalog
                .validate_borrow(&i32_id, &reference_id, false)
                .expect("immutable Copy scalar borrow"),
            ()
        );
        let mutable_error = catalog
            .validate_borrow(&i32_id, &mutable_reference_id, true)
            .expect_err("mutable borrow is outside this canonical slice");
        assert!(mutable_error.contains("mutable Borrow"));
        let mismatch_error = catalog
            .validate_borrow(&bool_id, &reference_id, false)
            .expect_err("reference target identity must be exact");
        assert!(mismatch_error.contains("target") || mismatch_error.contains("source"));
    }

    #[test]
    fn materializes_string_drop_and_clone_contract() {
        let mut table = ResolvedTypeTable::new();
        let id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("type");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let descriptor = catalog.get(&id).expect("descriptor");
        assert_eq!(descriptor.ownership, MirOwnership::Move);
        assert!(descriptor.needs_drop_glue);
        assert!(descriptor.needs_clone_glue);
        assert_eq!(descriptor.glue.move_out, MirGlueKind::OwnedString);
        assert_eq!(descriptor.glue.clone, MirGlueKind::OwnedString);
        assert_eq!(descriptor.glue.drop, MirGlueKind::OwnedString);
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            assert!(catalog.validate_glue(&id, operation).is_ok());
        }
        assert!(catalog.validate_owned_string(&id).is_ok());
    }

    #[test]
    fn materializes_parameterized_set_handle_contract() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let set_id = table
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("builtin:type:Set").expect("Set"),
                arguments: vec![i32_id.clone()],
                is_linear: false,
            })
            .expect("Set<i32>");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let descriptor = catalog.get(&set_id).expect("Set descriptor");
        assert_eq!(descriptor.kind, MirTypeKind::Set);
        assert_eq!(descriptor.abi, MirAbiClass::SetHandle);
        assert_eq!(
            descriptor.layout,
            MirLayout::Set {
                element: i32_id.clone()
            }
        );
        assert_eq!(descriptor.ownership, MirOwnership::Move);
        assert_eq!(descriptor.glue.move_out, MirGlueKind::Set);
        assert_eq!(descriptor.glue.clone, MirGlueKind::Set);
        assert_eq!(descriptor.glue.drop, MirGlueKind::Set);
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            assert!(catalog.validate_set_glue(&set_id, operation).is_ok());
        }
        assert!(catalog
            .validate_set_construct(&set_id, &[i32_id.clone(), i32_id.clone()])
            .is_ok());
        assert!(catalog
            .validate_set_operation(&set_id, &set_id, Some(&i32_id), MirSetOperation::Insert,)
            .is_ok());
        assert!(catalog
            .validate_set_operation(&bool_id, &set_id, None, MirSetOperation::IsEmpty,)
            .is_ok());
        let list_id = table
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("builtin:type:List").expect("List"),
                arguments: vec![i32_id.clone()],
                is_linear: false,
            })
            .expect("List<i32>");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog with List");
        assert!(catalog
            .validate_set_operation(&list_id, &set_id, None, MirSetOperation::ToList,)
            .is_ok());
        assert!(catalog
            .validate_set_operation(&set_id, &set_id, None, MirSetOperation::ToList,)
            .is_err());
    }

    #[test]
    fn list_len_contract_is_read_only_and_rejects_erased_or_wrong_results() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let list_id = table
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("builtin:type:List").expect("List"),
                arguments: vec![i32_id.clone()],
                is_linear: false,
            })
            .expect("List<i32>");
        let erased_list_id = table
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("builtin:type:List").expect("List"),
                arguments: Vec::new(),
                is_linear: false,
            })
            .expect("erased List");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");

        assert!(catalog
            .validate_list_operation(&i32_id, &list_id, MirListOperation::Len)
            .is_ok());
        let wrong_result = catalog
            .validate_list_operation(&bool_id, &list_id, MirListOperation::Len)
            .expect_err("List.len must return i32");
        assert!(wrong_result.contains("Copy i32"));
        let erased = catalog
            .validate_list_operation(&i32_id, &erased_list_id, MirListOperation::Len)
            .expect_err("erased List has no element ABI contract");
        assert!(erased.contains("List<T>") || erased.contains("canonical"));

        assert!(catalog
            .validate_list_operation(&list_id, &list_id, MirListOperation::Reverse)
            .is_ok());
        let wrong_reverse_result = catalog
            .validate_list_operation(&i32_id, &list_id, MirListOperation::Reverse)
            .expect_err("List.reverse must return the same owned List type");
        assert!(wrong_reverse_result.contains("result type"));

        let concat = catalog
            .validated_list_operation_contract_with_argument(
                &list_id,
                &list_id,
                Some(&list_id),
                MirListOperation::Concat,
            )
            .expect("List.concat contract");
        assert_eq!(concat.argument_ty, Some(list_id.clone()));
        assert_eq!(concat.result_ty, list_id);
        assert!(catalog
            .validate_list_operation_receipt_with_argument(
                &concat.result_ty,
                &concat.list_ty,
                concat.argument_ty.as_ref(),
                concat.operation,
                &concat,
            )
            .is_ok());
        let missing_argument = catalog
            .validate_list_operation(&concat.result_ty, &concat.list_ty, MirListOperation::Concat)
            .expect_err("List.concat must carry its second input");
        assert!(missing_argument.contains("second List argument"));
    }

    #[test]
    fn set_handle_contract_rejects_wrong_element_and_erased_set() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let set_id = table
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("builtin:type:Set").expect("Set"),
                arguments: vec![i32_id.clone()],
                is_linear: false,
            })
            .expect("Set<i32>");
        let erased_set_id = table
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("builtin:type:Set").expect("Set"),
                arguments: Vec::new(),
                is_linear: false,
            })
            .expect("erased Set");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let wrong_element = catalog
            .validate_set_construct(&set_id, &[bool_id])
            .expect_err("Set<bool> element must not enter Set<i32>");
        assert!(wrong_element.contains("disagrees with layout element"));
        let erased = catalog
            .validate_set_glue(&erased_set_id, MirGlueOperation::MoveOut)
            .expect_err("erased Set has no payload contract");
        assert!(erased.contains("Set<T>") || erased.contains("canonical"));
    }

    #[test]
    fn owned_string_contract_rejects_incomplete_glue() {
        let mut table = ResolvedTypeTable::new();
        let id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("type");
        let mut catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let mut descriptor = catalog.get(&id).expect("descriptor").clone();
        descriptor.glue.drop = MirGlueKind::Noop;
        catalog.replace_for_test_only(id.clone(), descriptor);
        let error = catalog
            .validate_owned_string(&id)
            .expect_err("incomplete owned String glue must fail closed");
        assert!(error.contains("glue contract is incomplete"));
    }

    #[test]
    fn materializes_move_owned_variant_glue_and_drop_schedule() {
        let mut table = ResolvedTypeTable::new();
        let string_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("string");
        let option_id = table
            .intern_resolved(ResolvedType::Option(string_id.clone()))
            .expect("option");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let descriptor = catalog.get(&option_id).expect("descriptor");
        assert_eq!(descriptor.ownership, MirOwnership::Move);
        assert_eq!(descriptor.glue.move_out, MirGlueKind::Aggregate);
        let plans = descriptor
            .variant_drop_plan
            .as_ref()
            .expect("variant drop plans");
        assert_eq!(plans.len(), 2);
        assert!(plans[0].fields.is_empty(), "None has no payload");
        assert_eq!(plans[1].fields.len(), 1);
        assert_eq!(plans[1].fields[0].index, 0);
        assert_eq!(plans[1].fields[0].ty, string_id);
        assert_eq!(plans[1].fields[0].glue, MirGlueKind::OwnedString);
        let some_id = crate::core::NodeId("builtin:variant:Option::Some".into());
        let none_id = crate::core::NodeId("builtin:variant:Option::None".into());
        let (switch_nominal, switch_some) = catalog
            .validated_variant_switch_case(&option_id, &some_id)
            .expect("canonical Some switch case");
        assert_eq!(switch_nominal, "builtin:type:Option");
        assert_eq!(switch_some.discriminant, 1);
        let switch_error = catalog
            .validated_variant_switch_case(
                &string_id,
                &crate::core::NodeId("builtin:variant:Option::Some".into()),
            )
            .expect_err("bare string has no variant switch case");
        assert!(switch_error.contains("variant layout"));
        let (none_variant, some_variant, inner) = catalog
            .validated_option_string_variants(&option_id)
            .expect("canonical Option<string> variants");
        assert_eq!(none_variant.id, none_id);
        assert_eq!(some_variant.id, some_id);
        assert_eq!(inner, string_id);
        let (payload_none, payload_some, payload_field) = catalog
            .validated_option_string_payload(&option_id)
            .expect("canonical Option<string> payload field");
        assert_eq!(payload_none.id, none_id);
        assert_eq!(payload_some.id, some_id);
        assert_eq!(
            payload_field.id,
            crate::core::NodeId("builtin:variant:Option::Some/payload:0".into())
        );
        assert_eq!(payload_field.ty, string_id);
        let payload_error = catalog
            .validated_option_string_payload(&string_id)
            .expect_err("bare string has no Option<string> payload contract");
        assert!(payload_error.contains("Option<string>"), "{payload_error}");
        assert_eq!(
            catalog
                .validated_variant_drop_plan(&option_id, &some_id)
                .expect("Some drop plan")
                .fields[0]
                .index,
            0
        );
        let (drop_nominal, drop_plan) = catalog
            .validated_variant_drop_contract(&option_id, &some_id)
            .expect("Some runtime drop contract");
        assert_eq!(drop_nominal, "builtin:type:Option");
        assert_eq!(drop_plan.fields[0].index, 0);
        let (drop_table_nominal, drop_table) = catalog
            .validated_variant_drop_contract_table(&option_id)
            .expect("complete Option variant drop contract");
        assert_eq!(drop_table_nominal, "builtin:type:Option");
        assert_eq!(
            drop_table
                .iter()
                .map(|variant| (variant.name.as_str(), variant.fields.len()))
                .collect::<Vec<_>>(),
            vec![("None", 0), ("Some", 1)]
        );
        let (shape_nominal, shape_table) = catalog
            .validated_variant_shape_table(&option_id)
            .expect("complete Option variant construction shape contract");
        assert_eq!(shape_nominal, "builtin:type:Option");
        assert_eq!(
            shape_table
                .iter()
                .map(|variant| (
                    variant.name.as_str(),
                    variant.discriminant,
                    variant.fields.len()
                ))
                .collect::<Vec<_>>(),
            vec![("None", 0, 0), ("Some", 1, 1)]
        );
        let drop_contract_error = catalog
            .validated_variant_drop_contract(&string_id, &some_id)
            .expect_err("bare string has no variant drop contract");
        assert!(drop_contract_error.contains("variant layout"));
        let drop_table_error = catalog
            .validated_variant_drop_contract_table(&string_id)
            .expect_err("bare string has no variant drop contract table");
        assert!(drop_table_error.contains("variant layout"));
        assert!(catalog
            .validated_variant_drop_plan(&option_id, &none_id)
            .expect("None drop plan")
            .fields
            .is_empty());
        let (drop_none, drop_some, some_plan, none_plan) = catalog
            .validated_option_string_drop_contract(&option_id)
            .expect("canonical Option<string> drop contract");
        assert_eq!(drop_none.id, none_id);
        assert_eq!(drop_some.id, some_id);
        assert_eq!(some_plan.fields[0].index, 0);
        assert!(none_plan.fields.is_empty());
        let missing = catalog
            .validated_variant_drop_plan(
                &option_id,
                &crate::core::NodeId("builtin:variant:Option::Missing".into()),
            )
            .expect_err("unknown active variant must fail closed");
        assert!(missing.contains("no drop plan for variant"), "{missing}");
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            assert!(catalog.validate_glue(&option_id, operation).is_ok());
        }
        assert!(catalog
            .validated_option_string_variants(&string_id)
            .is_err());
    }

    #[test]
    fn materializes_result_variant_glue_for_each_payload_family() {
        let mut table = ResolvedTypeTable::new();
        let string_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("string");
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let result_id = table
            .intern_resolved(ResolvedType::Result {
                ok: string_id.clone(),
                error: i32_id.clone(),
            })
            .expect("result");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let descriptor = catalog.get(&result_id).expect("result descriptor");
        assert_eq!(descriptor.glue.move_out, MirGlueKind::Aggregate);
        let plans = descriptor
            .variant_drop_plan
            .as_ref()
            .expect("result variant drop plans");
        assert_eq!(plans[0].fields[0].ty, string_id);
        assert_eq!(plans[0].fields[0].glue, MirGlueKind::OwnedString);
        assert_eq!(plans[1].fields[0].ty, i32_id);
        assert_eq!(plans[1].fields[0].glue, MirGlueKind::Noop);
        assert!(catalog
            .validate_variant_glue(&result_id, MirGlueOperation::Drop)
            .is_ok());
    }

    #[test]
    fn admits_only_result_string_i32_as_non_copy_variant_contract() {
        let mut table = ResolvedTypeTable::new();
        let string_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("string");
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let second_string_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("second string");
        let result_id = table
            .intern_resolved(ResolvedType::Result {
                ok: string_id.clone(),
                error: i32_id.clone(),
            })
            .expect("Result<string, i32>");
        let rejected_id = table
            .intern_resolved(ResolvedType::Result {
                ok: string_id,
                error: second_string_id,
            })
            .expect("Result<string, string>");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");

        catalog
            .validate_result_string_i32_variant(&result_id)
            .expect("Result<string, i32> must satisfy the canonical contract");
        catalog
            .validate_non_copy_variant_contract(&result_id)
            .expect("shared non-Copy variant boundary");
        let error = catalog
            .validate_result_string_i32_variant(&rejected_id)
            .expect_err("Result<string, string> must remain fail-closed");
        assert!(error.contains("Result Err payload must be the canonical Copy signed i32"));
        let combined = catalog
            .validate_non_copy_variant_contract(&rejected_id)
            .expect_err("unsupported Result shape must remain outside the shared boundary");
        assert!(combined.contains("Result<string, i32>"));
    }

    #[test]
    fn variant_shape_table_rejects_duplicate_tags_and_discriminants() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let option_id = table
            .intern_resolved(ResolvedType::Option(i32_id))
            .expect("option");
        let mut catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let original = catalog.get(&option_id).expect("option descriptor").clone();

        let mut duplicate_tag = original.clone();
        let MirLayout::Option { variants, .. } = &mut duplicate_tag.layout else {
            unreachable!("Option layout");
        };
        variants[1].name = variants[0].name.clone();
        catalog.replace_for_test_only(option_id.clone(), duplicate_tag);
        let tag_error = catalog
            .validated_variant_shape_table(&option_id)
            .expect_err("duplicate variant tags must fail closed");
        assert!(tag_error.contains("duplicate variant tag"), "{tag_error}");

        let mut duplicate_discriminant = original;
        let MirLayout::Option { variants, .. } = &mut duplicate_discriminant.layout else {
            unreachable!("Option layout");
        };
        variants[1].discriminant = variants[0].discriminant;
        catalog.replace_for_test_only(option_id.clone(), duplicate_discriminant);
        let discriminant_error = catalog
            .validated_variant_shape_table(&option_id)
            .expect_err("duplicate variant discriminants must fail closed");
        assert!(
            discriminant_error.contains("duplicate variant discriminant"),
            "{discriminant_error}"
        );

        let mut duplicate_identity = catalog.get(&option_id).expect("option descriptor").clone();
        let MirLayout::Option { variants, .. } = &mut duplicate_identity.layout else {
            unreachable!("Option layout");
        };
        variants[1].id = variants[0].id.clone();
        catalog.replace_for_test_only(option_id.clone(), duplicate_identity);
        let identity_error = catalog
            .validated_variant_shape_table(&option_id)
            .expect_err("duplicate variant identities must fail closed");
        assert!(
            identity_error.contains("duplicate canonical variant identity"),
            "{identity_error}"
        );
    }

    #[test]
    fn materializes_recursive_tuple_drop_schedule() {
        let mut table = ResolvedTypeTable::new();
        let string_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("string type");
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32 type");
        let pair_id = table
            .intern_resolved(ResolvedType::Tuple(vec![string_id.clone(), i32_id.clone()]))
            .expect("pair type");
        let nested_id = table
            .intern_resolved(ResolvedType::Tuple(vec![pair_id.clone(), i32_id.clone()]))
            .expect("nested tuple type");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");

        let pair = catalog.get(&pair_id).expect("pair descriptor");
        assert_eq!(pair.glue.move_out, MirGlueKind::Aggregate);
        assert_eq!(pair.glue.clone, MirGlueKind::Aggregate);
        assert_eq!(pair.glue.drop, MirGlueKind::Aggregate);
        let tuple_receipt = catalog
            .validated_tuple_field_projection_contract(&pair_id, 1, &i32_id)
            .expect("tuple projection receipt");
        assert_eq!(tuple_receipt.tuple_ty, pair_id);
        assert_eq!(tuple_receipt.field_index, 1);
        assert_eq!(tuple_receipt.arity, 2);
        assert_eq!(tuple_receipt.field_ty, i32_id);
        let tuple_type_error = catalog
            .validated_tuple_field_projection_contract(&pair_id, 0, &i32_id)
            .expect_err("tuple projection must reject a forged result type");
        assert!(tuple_type_error.contains("disagrees with layout type"));
        let plan = pair.drop_plan.as_ref().expect("pair drop plan");
        assert_eq!(
            plan.fields
                .iter()
                .map(|field| field.index)
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
        assert_eq!(plan.fields[0].ty, i32_id);
        assert_eq!(plan.fields[0].glue, MirGlueKind::Noop);
        assert_eq!(plan.fields[1].ty, string_id);
        assert_eq!(plan.fields[1].glue, MirGlueKind::OwnedString);
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            assert!(catalog.validate_glue(&pair_id, operation).is_ok());
        }
        let projection_error = catalog
            .validate_projection(&pair_id, &string_id, &MirProjection::Tuple(0))
            .expect_err("non-Copy projection needs an explicit move contract");
        assert!(projection_error.contains("explicit move projection contract"));

        let nested = catalog.get(&nested_id).expect("nested descriptor");
        assert_eq!(nested.glue.drop, MirGlueKind::Aggregate);
        assert!(catalog
            .validate_aggregate_glue(&nested_id, MirGlueOperation::Drop)
            .is_ok());
    }

    #[test]
    fn recursive_tuple_rejects_copy_parent_hiding_move_child() {
        let mut table = ResolvedTypeTable::new();
        let string_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::String))
            .expect("string type");
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32 type");
        let move_child_id = table
            .intern_resolved(ResolvedType::Tuple(vec![string_id, i32_id]))
            .expect("move child tuple");
        let parent_id = table
            .intern_resolved(ResolvedType::Tuple(vec![move_child_id.clone()]))
            .expect("parent tuple");
        let mut catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");

        // A production catalog derives this ownership from the child graph.
        // Mutating the descriptor here models malformed MIR supplied to a
        // consumer and ensures the shared validator does not trust the parent
        // flag over the child TypeDesc contract.
        let mut forged_parent = catalog.get(&parent_id).expect("parent descriptor").clone();
        forged_parent.ownership = MirOwnership::Copy;
        forged_parent.needs_drop_glue = false;
        forged_parent.needs_clone_glue = false;
        forged_parent.glue = super::MirGlueContract {
            move_out: MirGlueKind::Noop,
            clone: MirGlueKind::Noop,
            drop: MirGlueKind::Noop,
        };
        forged_parent.drop_plan = None;
        catalog.replace_for_test_only(parent_id.clone(), forged_parent);

        let error = catalog
            .validate_recursive_tuple_abi(&parent_id)
            .expect_err("Copy parent must not hide a move-owned tuple child");
        assert!(error.contains("Copy tuple"), "{error}");
        assert!(error.contains("non-Copy"), "{error}");
    }

    #[test]
    fn variant_with_unmaterialized_child_glue_stays_fail_closed() {
        let mut table = ResolvedTypeTable::new();
        let opaque_id = table
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("user:type:Opaque").expect("nominal"),
                arguments: Vec::new(),
                is_linear: false,
            })
            .expect("opaque type");
        let option_id = table
            .intern_resolved(ResolvedType::Option(opaque_id))
            .expect("option type");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let option = catalog.get(&option_id).expect("option descriptor");
        assert_eq!(option.glue.move_out, MirGlueKind::Unsupported);
        assert!(option.variant_drop_plan.is_none());
        assert!(catalog
            .validate_glue(&option_id, MirGlueOperation::Drop)
            .is_err());
    }

    #[test]
    fn materializes_product_layout_from_canonical_type_shape() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let tuple_id = table
            .intern_resolved(ResolvedType::Tuple(vec![i32_id.clone(), bool_id.clone()]))
            .expect("tuple");
        let option_id = table
            .intern_resolved(ResolvedType::Option(i32_id.clone()))
            .expect("option");
        let result_id = table
            .intern_resolved(ResolvedType::Result {
                ok: i32_id.clone(),
                error: bool_id.clone(),
            })
            .expect("result");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        assert_eq!(
            catalog.get(&tuple_id).expect("tuple descriptor").layout,
            MirLayout::Tuple(vec![i32_id.clone(), bool_id.clone()])
        );
        assert!(matches!(
            &catalog.get(&option_id).expect("option descriptor").layout,
            MirLayout::Option { inner, variants }
                if inner == &i32_id
                    && variants.iter().map(|variant| variant.name.as_str()).collect::<Vec<_>>()
                        == ["None", "Some"]
                    && variants[0].discriminant == 0
                    && variants[1].discriminant == 1
                    && variants[1].fields[0].id.0
                        == "builtin:variant:Option::Some/payload:0"
                    && variants[1].fields[0].ty == i32_id
        ));
        assert!(matches!(
            &catalog.get(&result_id).expect("result descriptor").layout,
            MirLayout::Result { ok, error, variants }
                if ok == &i32_id
                    && error == &bool_id
                    && variants.iter().map(|variant| variant.name.as_str()).collect::<Vec<_>>()
                        == ["Ok", "Err"]
                    && variants[0].discriminant == 0
                    && variants[1].discriminant == 1
                    && variants[0].fields[0].ty == i32_id
                    && variants[1].fields[0].ty == bool_id
        ));
        assert!(catalog
            .validate_aggregate(
                &tuple_id,
                &crate::core::mir::MirAggregateKind::Tuple,
                &[i32_id.clone(), bool_id.clone()]
            )
            .is_ok());
        assert!(catalog
            .validate_aggregate(
                &tuple_id,
                &crate::core::mir::MirAggregateKind::Tuple,
                &[bool_id, i32_id]
            )
            .is_err());
    }

    #[test]
    fn flat_copy_variant_layout_contract_is_shared_and_rejects_mixed_payloads() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let option_id = table
            .intern_resolved(ResolvedType::Option(i32_id.clone()))
            .expect("option");
        let result_id = table
            .intern_resolved(ResolvedType::Result {
                ok: i32_id.clone(),
                error: bool_id,
            })
            .expect("mixed result");
        let catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");

        assert_eq!(catalog.validate_flat_copy_variant(&option_id), Ok(i32_id));
        let error = catalog
            .validate_flat_copy_variant(&result_id)
            .expect_err("mixed Result payload ABI must fail closed");
        assert!(error.contains("mixed payload ABI"), "{error}");
    }

    #[test]
    fn materializes_checker_record_field_contract() {
        let source =
            "type Point { x: i32, y: bool }\nfunc main() -> i32 { let p = Point { x: 1, y: true }; if p.y { 0 } else { 1 } }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("catalog");
        let point = catalog
            .iter()
            .find_map(|(_, descriptor)| match &descriptor.layout {
                MirLayout::Record { nominal, fields } if nominal.as_str().ends_with("Point") => {
                    Some((descriptor, fields))
                }
                _ => None,
            })
            .expect("Point record contract");
        assert_eq!(point.0.abi, MirAbiClass::Aggregate);
        assert_eq!(point.0.ownership, MirOwnership::Copy);
        assert_eq!(
            point
                .1
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["x", "y"]
        );
        assert!(point.1.iter().all(|field| catalog.get(&field.ty).is_some()));
        assert!(catalog.validate_flat_copy_record(&point.0.id).is_ok());
    }

    #[test]
    fn materializes_non_copy_record_product_glue_and_drop_schedule() {
        let source = "type Named { name: string, count: i32 }\nfunc main() -> i32 { let p = Named { count: 41, name: \"owned\" }; drop(p); 42 }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("catalog");
        let named = catalog
            .iter()
            .find_map(|(_, descriptor)| match &descriptor.layout {
                MirLayout::Record { nominal, fields } if nominal.as_str().ends_with("Named") => {
                    Some((descriptor, fields))
                }
                _ => None,
            })
            .expect("Named record contract");
        assert_eq!(named.0.ownership, MirOwnership::Move);
        assert_eq!(named.0.abi, MirAbiClass::Aggregate);
        assert_eq!(
            named.0.glue,
            crate::core::mir::types::MirGlueContract {
                move_out: MirGlueKind::Aggregate,
                clone: MirGlueKind::Aggregate,
                drop: MirGlueKind::Aggregate,
            }
        );
        assert_eq!(
            named
                .0
                .drop_plan
                .as_ref()
                .expect("record drop plan")
                .fields
                .iter()
                .map(|field| field.index)
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
        assert_eq!(
            named
                .1
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["name", "count"]
        );
        assert!(catalog.validate_flat_copy_record(&named.0.id).is_err());
        catalog
            .validate_aggregate_glue(&named.0.id, MirGlueOperation::Drop)
            .expect("record drop schedule");
    }

    #[test]
    fn record_projection_and_update_contracts_are_field_id_based() {
        let source =
            "type Point { x: i32, y: bool }\nfunc main() -> i32 { Point { x: 1, y: true }.x }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("catalog");
        let (point_ty, fields) = catalog
            .iter()
            .find_map(|(id, descriptor)| match &descriptor.layout {
                MirLayout::Record { nominal, fields } if nominal.as_str().ends_with("Point") => {
                    Some((id.clone(), fields.clone()))
                }
                _ => None,
            })
            .expect("Point layout");
        let x = fields.iter().find(|field| field.name == "x").expect("x");
        let y = fields.iter().find(|field| field.name == "y").expect("y");
        assert!(catalog
            .validate_projection(&point_ty, &x.ty, &MirProjection::Field(x.id.clone()),)
            .is_ok());
        let receipt = catalog
            .validated_record_field_projection_contract(&point_ty, &x.id, &x.ty)
            .expect("record projection receipt");
        assert_eq!(receipt.field, x.id);
        assert_eq!(receipt.name, "x");
        assert_eq!(receipt.field_index, 0);
        assert_eq!(receipt.arity, 2);
        assert_eq!(receipt.field_ty, x.ty);
        assert!(catalog
            .validated_record_field_projection_contract(&point_ty, &x.id, &y.ty)
            .is_err());
        let unknown = catalog
            .validate_projection(
                &point_ty,
                &x.ty,
                &MirProjection::Field(crate::core::NodeId("field:missing".into())),
            )
            .expect_err("unknown field must fail closed");
        assert!(unknown.contains("absent"));
        let wrong_type = catalog
            .validate_projection(&point_ty, &y.ty, &MirProjection::Field(x.id.clone()))
            .expect_err("wrong projection result type must fail closed");
        assert!(wrong_type.contains("disagrees"));
        assert!(catalog
            .validate_record_update(
                &point_ty,
                &point_ty,
                &MirAggregateKind::Record {
                    nominal: match &catalog.get(&point_ty).expect("point").layout {
                        MirLayout::Record { nominal, .. } => nominal.clone(),
                        _ => unreachable!(),
                    },
                    fields: vec![y.id.clone()],
                },
                std::slice::from_ref(&y.ty),
            )
            .is_ok());
    }

    #[test]
    fn record_move_projection_requires_owned_string_and_copy_siblings() {
        let source =
            "type Named { name: string, count: i32 }\nfunc main() -> string { let p = Named { name: \"owned\", count: 41 }; p.name }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("catalog");
        let (named_ty, name_ty, name_id) = catalog
            .iter()
            .find_map(|(id, descriptor)| match &descriptor.layout {
                MirLayout::Record { nominal, fields } if nominal.as_str().ends_with("Named") => {
                    let name = fields.iter().find(|field| field.name == "name")?;
                    Some((id.clone(), name.ty.clone(), name.id.clone()))
                }
                _ => None,
            })
            .expect("Named field contract");
        catalog
            .validate_move_projection(
                &named_ty,
                &name_ty,
                &crate::core::mir::MirProjection::Field(name_id),
            )
            .expect("owned string field with Copy sibling is movable");

        let source =
            "type Pair { left: string, right: string }\nfunc main() -> string { let p = Pair { left: \"left\", right: \"right\" }; p.left }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("catalog");
        let (pair_ty, left_ty, left_id) = catalog
            .iter()
            .find_map(|(id, descriptor)| match &descriptor.layout {
                MirLayout::Record { nominal, fields } if nominal.as_str().ends_with("Pair") => {
                    let left = fields.iter().find(|field| field.name == "left")?;
                    Some((id.clone(), left.ty.clone(), left.id.clone()))
                }
                _ => None,
            })
            .expect("Pair field contract");
        let error = catalog
            .validate_move_projection(
                &pair_ty,
                &left_ty,
                &crate::core::mir::MirProjection::Field(left_id),
            )
            .expect_err("non-Copy sibling requires a residual partial-move contract");
        assert!(error.contains("non-Copy sibling"), "{error}");
    }

    #[test]
    fn variant_layout_rejects_bad_payload_and_non_exhaustive_switches() {
        let mut table = ResolvedTypeTable::new();
        let i32_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("i32");
        let bool_id = table
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("bool");
        let option_id = table
            .intern_resolved(ResolvedType::Option(i32_id.clone()))
            .expect("option");
        let mut catalog = MirTypeCatalog::from_resolved_types(&table).expect("catalog");
        let option_nominal =
            crate::core::ir::NominalTypeId::new("builtin:type:Option").expect("Option nominal");
        let some = crate::core::NodeId("builtin:variant:Option::Some".into());
        let some_field = crate::core::NodeId("builtin:variant:Option::Some/payload:0".into());
        assert_eq!(
            catalog
                .validate_variant_payload_projection(&option_id, &some, &some_field, &i32_id,)
                .expect("Some payload projection"),
            0
        );
        let projection = catalog
            .validated_variant_payload_projection_contract(&option_id, &some, &some_field, &i32_id)
            .expect("Some payload projection contract");
        assert_eq!(projection.nominal.as_str(), "builtin:type:Option");
        assert_eq!(projection.variant, some);
        assert_eq!(projection.field, some_field);
        assert_eq!(projection.field_index, 0);
        assert_eq!(projection.arity, 1);
        assert_eq!(projection.field_ty, i32_id);
        let projection_error = catalog
            .validate_variant_payload_projection(&option_id, &some, &some_field, &bool_id)
            .expect_err("payload result type must match TypeDesc");
        assert!(
            projection_error.contains("disagrees with result type"),
            "{projection_error}"
        );
        let missing_field = catalog
            .validate_variant_payload_projection(
                &option_id,
                &some,
                &crate::core::NodeId("builtin:variant:Option::Some/payload:missing".into()),
                &i32_id,
            )
            .expect_err("unknown payload identity must fail closed");
        assert!(
            missing_field.contains("absent from variant"),
            "{missing_field}"
        );
        assert!(catalog
            .validate_variant_construct(
                &option_id,
                &option_nominal,
                &some,
                std::slice::from_ref(&some_field),
                std::slice::from_ref(&bool_id),
            )
            .is_err());
        let some_desc = catalog
            .validated_variant_construct(
                &option_id,
                &option_nominal,
                &some,
                std::slice::from_ref(&some_field),
                std::slice::from_ref(&i32_id),
            )
            .expect("valid construction returns canonical descriptor");
        assert_eq!(some_desc.name, "Some");
        assert_eq!(some_desc.discriminant, 1);
        let (constructed_some, constructed_payload) = catalog
            .validated_single_payload_variant_construct(
                &option_id,
                &option_nominal,
                &some,
                std::slice::from_ref(&some_field),
                std::slice::from_ref(&i32_id),
            )
            .expect("single-payload construction contract");
        assert_eq!(constructed_some.id, some);
        assert_eq!(constructed_payload.expect("Some payload").id, some_field);
        let constructed_none = catalog
            .validated_single_payload_variant_construct(
                &option_id,
                &option_nominal,
                &crate::core::NodeId("builtin:variant:Option::None".into()),
                &[],
                &[],
            )
            .expect("zero-payload construction contract");
        assert!(constructed_none.1.is_none());
        let single_payload_error = catalog
            .validated_single_payload_variant_construct(
                &option_id,
                &option_nominal,
                &some,
                std::slice::from_ref(&some_field),
                std::slice::from_ref(&bool_id),
            )
            .expect_err("single-payload construction must preserve type rejection");
        assert!(single_payload_error.contains("disagrees with layout type"));
        let flat_some = catalog
            .validated_flat_copy_variant(&option_id, &some)
            .expect("flat Copy Some descriptor");
        assert_eq!(flat_some.id, some);
        let (projected_variant, projected_index) = catalog
            .validated_flat_copy_payload_projection(&option_id, &some, &some_field, &i32_id)
            .expect("flat Copy payload projection");
        assert_eq!(projected_variant.id, some);
        assert_eq!(projected_index, 0);
        let projected_error = catalog
            .validated_flat_copy_payload_projection(&option_id, &some, &some_field, &bool_id)
            .expect_err("flat Copy payload type drift must fail closed");
        assert!(
            projected_error.contains("disagrees with result type"),
            "{projected_error}"
        );
        let flat_missing = catalog
            .validated_flat_copy_variant(
                &option_id,
                &crate::core::NodeId("builtin:variant:Option::Missing".into()),
            )
            .expect_err("unknown flat Copy variant must fail closed");
        assert!(
            flat_missing.contains("absent from TypeDesc"),
            "{flat_missing}"
        );
        assert!(catalog
            .validate_variant_construct(
                &option_id,
                &option_nominal,
                &crate::core::NodeId("builtin:variant:Option::Missing".into()),
                &[],
                &[],
            )
            .is_err());

        let only_some = crate::core::mir::MirSwitchArm {
            edge: crate::core::mir::MirEdgeId::new("edge:some").expect("edge"),
            target: crate::core::mir::MirBlockId::new("bb:some").expect("block"),
            arguments: Vec::new(),
            bindings: Vec::new(),
            case: crate::core::mir::MirSwitchCase::Variant(some.clone()),
        };
        let error = catalog
            .validate_switch(&option_id, &[only_some])
            .expect_err("missing None must fail closed");
        assert!(error.contains("None"));

        let extra_field = crate::core::NodeId("builtin:variant:Option::Some/payload:1".into());
        let mut forged = catalog.get(&option_id).expect("option descriptor").clone();
        let MirLayout::Option { variants, .. } = &mut forged.layout else {
            unreachable!("Option layout");
        };
        let forged_some = variants
            .iter_mut()
            .find(|candidate| candidate.id == some)
            .expect("Some variant");
        let mut second_field = forged_some.fields[0].clone();
        second_field.id = extra_field.clone();
        forged_some.fields.push(second_field);
        catalog.replace_for_test_only(option_id.clone(), forged);
        let multi_payload_error = catalog
            .validated_single_payload_variant_construct(
                &option_id,
                &option_nominal,
                &some,
                &[some_field, extra_field],
                &[i32_id.clone(), i32_id],
            )
            .expect_err("multi-payload native construction must fail closed");
        assert!(multi_payload_error.contains("single-payload native ABI"));
    }

    #[test]
    fn canonical_text_is_declaration_order_independent() {
        let mut first = ResolvedTypeTable::new();
        let _ = first
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("type");
        let _ = first
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("type");
        let mut second = ResolvedTypeTable::new();
        let _ = second
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
            .expect("type");
        let _ = second
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::I32))
            .expect("type");
        let first = MirTypeCatalog::from_resolved_types(&first)
            .expect("catalog")
            .canonical_text();
        let second = MirTypeCatalog::from_resolved_types(&second)
            .expect("catalog")
            .canonical_text();
        assert_eq!(first, second);
    }
}
