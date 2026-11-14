use std::collections::{BTreeMap, BTreeSet};

use crate::span::Span;

use super::cfg::{
    BasicBlockId, CallableCfg, CfgEdge, CfgPoint, CfgPointKind, CfgSource, EdgeId, Terminator,
};
use super::ir::{
    BuiltinId, CheckedConversionKind, EffectId, Permission, ResolvedBlock, ResolvedBody,
    ResolvedExpr, ResolvedExprKind, ResolvedFStringPart, ResolvedPatternKind, ResolvedStmt,
    ResolvedStmtKind,
};
use super::{NodeId, Origin, ResolvedLocalId, ResolvedParameterId, ResolvedTypeId};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub NodeId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceId(pub NodeId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LoanId(pub NodeId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IndexProjection {
    Constant(i64),
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlaceProjection {
    Field { field: NodeId, name: String },
    Tuple(usize),
    Index(IndexProjection),
    Deref,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Place {
    pub base: LocalId,
    pub base_name: String,
    pub projections: Vec<PlaceProjection>,
}

impl Place {
    pub fn root(base: LocalId, base_name: impl Into<String>) -> Self {
        Self {
            base,
            base_name: base_name.into(),
            projections: Vec::new(),
        }
    }

    pub fn conflicts_with(&self, other: &Self) -> bool {
        // G-5 residual (closed 0.36.107 by design): deref projections are
        // treated as overlapping with every other place. We cannot yet walk
        // through the pointee's shape to distinguish `*p.a` from `*p.b`;
        // conservatively reporting overlap keeps E0415 fail-closed at the
        // cost of a possible false positive, which is safer than a missed
        // aliased borrow.
        if self.projections.contains(&PlaceProjection::Deref)
            || other.projections.contains(&PlaceProjection::Deref)
        {
            return true;
        }
        if self.base != other.base {
            return false;
        }
        for (left, right) in self.projections.iter().zip(other.projections.iter()) {
            if left == right {
                continue;
            }
            return match (left, right) {
                (
                    PlaceProjection::Field { field: left, .. },
                    PlaceProjection::Field { field: right, .. },
                ) => left == right,
                (PlaceProjection::Tuple(left), PlaceProjection::Tuple(right)) => left == right,
                (
                    PlaceProjection::Index(IndexProjection::Constant(left)),
                    PlaceProjection::Index(IndexProjection::Constant(right)),
                ) => left == right,
                (PlaceProjection::Index(_), PlaceProjection::Index(_)) => true,
                _ => true,
            };
        }
        // Equal paths and root/prefix relationships overlap.
        true
    }

    pub fn display(&self) -> String {
        let mut value = self.base_name.clone();
        for projection in &self.projections {
            match projection {
                PlaceProjection::Field { name: field, .. } => {
                    value.push('.');
                    value.push_str(field);
                }
                PlaceProjection::Tuple(index) => {
                    value.push('.');
                    value.push_str(&index.to_string());
                }
                PlaceProjection::Index(IndexProjection::Constant(index)) => {
                    value.push('[');
                    value.push_str(&index.to_string());
                    value.push(']');
                }
                PlaceProjection::Index(IndexProjection::Dynamic) => value.push_str("[*]"),
                PlaceProjection::Deref => value.insert(0, '*'),
            }
        }
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoanKind {
    Shared,
    Mutable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgLocation {
    pub block: BasicBlockId,
    pub point: NodeId,
    pub edge: Option<EdgeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonicalActionKind {
    Read,
    Write,
    Introduce,
    Move,
    Drop,
    Return,
    TransferSession,
    TransferChild,
    BorrowShared,
    BorrowMut,
    BorrowEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalResourceAction {
    pub kind: CanonicalActionKind,
    pub resource: ResourceId,
    pub source: Option<Place>,
    pub target: Option<Place>,
    pub loan: Option<LoanId>,
    pub location: CfgLocation,
    pub span: Span,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    Consumed,
    MaybeConsumed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceFact {
    pub availability: Availability,
    pub owner: Option<Place>,
    /// Span where this resource obligation was introduced (e.g. the `let`
    /// binding that created the linear capability). Used by E0256/E0304 to
    /// point back to the resource origin, satisfying the DX manifest's
    /// "resource introduced here" requirement.
    pub introduced_span: Option<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loan {
    pub id: LoanId,
    pub parent: Option<LoanId>,
    pub kind: LoanKind,
    pub place: Place,
    pub reference: Option<LocalId>,
    pub reference_name: Option<String>,
    pub start: CfgLocation,
    pub end_edges: Vec<EdgeId>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceAnalysis {
    pub owner: NodeId,
    pub actions: Vec<CanonicalResourceAction>,
    pub loans: Vec<Loan>,
    /// Checker-owned lifecycle facts for the closed local Map-root slice.
    /// These facts are separate from ordinary Place/Loan events: a Map root
    /// is an opaque runtime handle whose persistent `map_set` result and
    /// `map_size` borrow cannot be reconstructed from a surface call name by
    /// MIR consumers.
    pub(crate) map_root_actions: Vec<MapRootAction>,
    /// Every resolved zero-argument `map_new()` call observed in this
    /// callable. Route admission compares these stable node ids against the
    /// Checker New receipts so one complete lifecycle cannot mask an
    /// additional unreceipted temporary root.
    pub(crate) map_root_new_attempts: BTreeSet<NodeId>,
    /// Independently recomputed Checker-owned lifecycle facts for the
    /// versioned MapAnyOwner profile. These are not inferred from the
    /// MapRoot ledger: the profile has a separate schema marker, root
    /// identity namespace, and construction-attempt coverage set.
    pub(crate) map_any_owner_actions: Vec<MapAnyOwnerAction>,
    /// Every `map_new()` construction attempt observed by the independent
    /// MapAnyOwner analysis, including unsupported temporary roots.
    pub(crate) map_any_owner_new_attempts: BTreeSet<NodeId>,
    /// Independently recomputed Checker-owned facts for the first ordinary
    /// Map owner lifecycle slice. This profile has its own version marker,
    /// root identity namespace, and attempt ledger; it is not derived from
    /// either MapRoot or MapAnyOwner actions.
    pub(crate) ordinary_map_owner_lifecycle_v1_actions: Vec<OrdinaryMapOwnerLifecycleAction>,
    /// Every `map_new()` call found while scanning this callable, including
    /// temporary or otherwise unsupported roots.
    pub(crate) ordinary_map_owner_lifecycle_v1_new_attempts: BTreeSet<NodeId>,
    /// Every recognized Map operation call, including operations outside the
    /// admitted lifecycle slice. Kept when the receipt is withheld so later
    /// route selection can account for the whole callable.
    pub(crate) ordinary_map_owner_lifecycle_v1_operation_attempts:
        BTreeSet<OrdinaryMapOwnerLifecycleAttempt>,
    /// Independent Checker census of direct JSON/stdout calls whose checked
    /// argument includes a builtin Map root. Every such attempt must have one
    /// exact owner-observer receipt before a consumer may use it.
    pub(crate) ordinary_map_owner_observer_v1_attempts: BTreeSet<OrdinaryMapOwnerObserverAttempt>,
    /// Exact non-consuming observations of a live lifecycle-v1 Map root.
    /// These are separate from lifecycle actions so only the dedicated
    /// observer profile can authorize the corresponding direct root load.
    pub(crate) ordinary_map_owner_observer_v1_receipts: Vec<OrdinaryMapOwnerObserverReceipt>,
    /// Independent Checker receipt for the typed owned Map getter profile.
    /// This ledger has a distinct root namespace and operation-attempt set;
    /// neither the older ordinary lifecycle receipt nor MapRoot/MapAnyOwner
    /// actions can authorize it.
    pub(crate) ordinary_map_owned_get_i32_string_v1_actions:
        Vec<OrdinaryMapOwnedGetI32StringV1Action>,
    /// Every `map_new()` construction attempt scanned for the independent
    /// typed owned-get profile, including temporary/unsupported roots.
    pub(crate) ordinary_map_owned_get_i32_string_v1_new_attempts: BTreeSet<NodeId>,
    /// Every recognized Map operation scanned for the typed owned-get
    /// profile, including old tuple `map_get` and unsupported operations.
    pub(crate) ordinary_map_owned_get_i32_string_v1_operation_attempts:
        BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
    /// Versioned source-CFG proof for the currently admitted entry-closed
    /// Map lifecycle with a Map-free control-flow tail.  The MIR validator
    /// binds this graph snapshot to the actual reachable MIR graph before a
    /// consumer may rely on the flat per-operation receipt.
    pub(crate) ordinary_map_owned_get_i32_string_v1_cfg_receipt:
        Option<OrdinaryMapOwnedGetI32StringV1CfgReceipt>,
    /// Independent Checker receipt for the ordinary Map/Any owner-v2 slice.
    /// This profile admits concrete nominal record payloads in addition to
    /// the primitive i32 payload.  It is deliberately separate from the
    /// older i32/String lifecycle so a legacy receipt cannot authorize a v2
    /// Map root.
    pub(crate) ordinary_map_owned_any_v2_actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    /// Every Map/Any operation attempt seen by the v2 source scan, including
    /// supported keys snapshots, unsupported to_list/map literals, and temporary roots. A complete
    /// action list without a matching attempt census is not an admissible
    /// receipt.
    pub(crate) ordinary_map_owned_any_v2_operation_attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    /// Versioned source receipt for the v2 profile.  The MIR route may bind
    /// its own graph digest to this snapshot without re-reading surface AST.
    pub(crate) ordinary_map_owned_any_v2_cfg_receipt: Option<OrdinaryMapOwnedAnyV2CfgReceipt>,
    /// Independent source authority for the first recoverable New result.
    /// This receipt does not grant a bare-root V2 action or route admission.
    /// The Ok binding, Err binding, and branch-local root settlement must be
    /// consumed together by the dedicated Result lowering before execution.
    pub(crate) ordinary_map_owned_any_v2_new_result_receipt:
        Option<OrdinaryMapOwnedAnyV2NewResultReceipt>,
    /// Independent source authority for a two-stage Result lineage through
    /// ordinary Map `map_new` and persistent `map_set`. This receipt is not
    /// derived from, or interchangeable with, the New-only Result receipt.
    pub(crate) ordinary_map_owned_any_v2_set_lineage_receipt:
        Option<OrdinaryMapOwnedAnyV2SetLineageReceipt>,
    /// Independent Checker source authority for one exact typed i32 Get on
    /// the persistent root produced by the closed SetResult lineage. This
    /// profile keeps the SetResult shape unchanged and proves both Get arms
    /// settle the updated root.
    pub(crate) ordinary_map_owned_any_v2_set_get_i32_lineage_receipt:
        Option<OrdinaryMapOwnedAnyV2SetGetI32LineageReceipt>,
    /// Independent Checker source authority for the exact fallible
    /// New/Set/GetAny i32 lineage. This binds both New and Set Result
    /// dispatches, the generic `(bool, Any)` Get projections and unpack, and
    /// explicit pair/root settlement; it does not borrow authority from the
    /// direct `map_get_i32` specialization.
    pub(crate) ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt:
        Option<OrdinaryMapOwnedAnyV2SetGetAnyI32LineageReceipt>,
    /// Independent Checker authority for the old tuple-valued Map.Get Any
    /// API when the exact winning Result-aware Set stores a String and the
    /// detached payload is checked with `unpack_string`. This receipt is
    /// deliberately narrower than the generic V2 static-key proof and cannot
    /// be substituted by the Result-valued `map_get_any` profile.
    pub(crate) ordinary_map_owned_any_v2_set_get_any_string_lineage_receipt:
        Option<OrdinaryMapOwnedAnyV2SetGetAnyStringLineageReceipt>,
    /// Independent L1 authority for the public tuple-valued `map_get`
    /// snapshot after a direct `match map_new()` and an exact chain of
    /// Result-matched persistent Sets. This source/CFG receipt is not a MIR
    /// route credential and does not prove runtime owner survival.
    pub(crate) ordinary_map_owned_any_v2_direct_tuple_get_unpack_receipt:
        Option<OrdinaryMapOwnedAnyV2DirectTupleGetUnpackReceipt>,
    /// Independent L1 source/CFG proof for the exact direct-print fixture:
    /// Result-matched `map_new` and `map_set`, tuple-valued `map_get`,
    /// `println(pair.1)`, and explicit settlement of both Map roots and the
    /// pair snapshot. This receipt is source evidence only and grants no MIR
    /// route by itself.
    pub(crate) ordinary_map_owned_any_v2_direct_print_result_receipt:
        Option<OrdinaryMapOwnedAnyV2DirectPrintResultReceipt>,
    /// Independent Checker source authority for the fixed fallible
    /// New/Set → legacy tuple-valued `map_get -> (bool, Any)` fixture with
    /// String and pure-i32 Any payloads. This is source evidence only: it is
    /// not the V2 Result-valued GetAny receipt and grants no route by itself.
    pub(crate) ordinary_map_owned_get_any_tuple_result_v1_receipt:
        Option<OrdinaryMapOwnedGetAnyTupleResultV1Receipt>,
    /// Independent Checker authority for the Result-valued `map_get_any`
    /// API. This does not reuse the legacy tuple-valued `map_get` receipt:
    /// its Ok/Err dispatch and payload cleanup obligations are different.
    pub(crate) ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt:
        Option<OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt>,
    /// Independent Checker authority for the exact ordinary Map/Any
    /// New/Set<String>/GetString lineage. This remains a separate receipt
    /// from the i32 specialization, including its Map-root and detached
    /// String owner identities.
    pub(crate) ordinary_map_owned_any_v2_set_get_string_lineage_receipt:
        Option<OrdinaryMapOwnedAnyV2SetGetStringLineageReceipt>,
    /// Checker-owned source-CFG snapshot shared by ownership-sensitive MIR
    /// validators. Unlike operation-specific Map receipts, this only commits
    /// the exact reachable source blocks, points, terminators, and edges.
    pub(crate) managed_any_cfg_receipt: Option<ManagedAnyCfgReceipt>,
    pub in_states: BTreeMap<BasicBlockId, BTreeMap<ResourceId, ResourceFact>>,
    pub out_states: BTreeMap<BasicBlockId, BTreeMap<ResourceId, ResourceFact>>,
}

/// One reachable Checker CFG block in the general source-CFG commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedAnyCfgBlock {
    pub(crate) source: CfgSource,
    pub(crate) points: Vec<CfgPoint>,
    pub(crate) terminator: Terminator,
}

/// Exact Checker source-CFG snapshot for a callable. This receipt is
/// deliberately operation-neutral: a MIR consumer must still prove every
/// owner, transfer, borrow, and cleanup obligation on each path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedAnyCfgReceipt {
    pub(crate) owner: NodeId,
    pub(crate) entry: BasicBlockId,
    pub(crate) reachable_blocks: BTreeMap<BasicBlockId, ManagedAnyCfgBlock>,
    pub(crate) reachable_edges: BTreeMap<EdgeId, CfgEdge>,
    /// Exact source match-arm dispatch cases keyed by the Checker block that
    /// owns the match terminator. Edge topology alone does not establish which
    /// MIR switch case selects a given source arm.
    pub(crate) match_arms: BTreeMap<BasicBlockId, Vec<ManagedAnyCfgMatchArm>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedAnyCfgMatchArm {
    pub(crate) edge: EdgeId,
    pub(crate) source_arm: NodeId,
    pub(crate) case: ManagedAnyCfgMatchCase,
    pub(crate) guarded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagedAnyCfgMatchCase {
    Default,
    Literal(crate::core::ir::ResolvedLiteral),
    Variant(NodeId),
    Unsupported,
}

impl ManagedAnyCfgReceipt {
    pub(crate) fn from_cfg(cfg: &CallableCfg, body: &ResolvedBody) -> Self {
        let mut source_match_arms = BTreeMap::new();
        collect_managed_any_match_arms_body(body, &mut source_match_arms);
        let reachable_blocks = cfg
            .reachable
            .iter()
            .filter_map(|id| {
                cfg.blocks.get(id).map(|block| {
                    (
                        id.clone(),
                        ManagedAnyCfgBlock {
                            source: block.source.clone(),
                            points: block.points.clone(),
                            terminator: block.terminator.clone(),
                        },
                    )
                })
            })
            .collect();
        let reachable_edges = cfg
            .edges
            .iter()
            .filter(|(_, edge)| {
                cfg.reachable.contains(&edge.from) && cfg.reachable.contains(&edge.to)
            })
            .map(|(id, edge)| (id.clone(), edge.clone()))
            .collect();
        let match_arms = cfg
            .reachable
            .iter()
            .filter_map(|block_id| {
                let block = cfg.blocks.get(block_id)?;
                let Terminator::Match { arms, .. } = &block.terminator else {
                    return None;
                };
                let receipt_arms = arms
                    .iter()
                    .map(|edge_id| {
                        let source_arm = cfg
                            .edges
                            .get(edge_id)
                            .map(|edge| edge.source.node.clone())
                            .unwrap_or_else(|| NodeId("missing-match-arm-edge".into()));
                        let (case, guarded) = source_match_arms
                            .get(&source_arm)
                            .cloned()
                            .unwrap_or((ManagedAnyCfgMatchCase::Unsupported, false));
                        ManagedAnyCfgMatchArm {
                            edge: edge_id.clone(),
                            source_arm,
                            case,
                            guarded,
                        }
                    })
                    .collect();
                Some((block_id.clone(), receipt_arms))
            })
            .collect();
        Self {
            owner: cfg.owner.clone(),
            entry: cfg.entry.clone(),
            reachable_blocks,
            reachable_edges,
            match_arms,
        }
    }
}

fn collect_managed_any_match_arms_body(
    body: &ResolvedBody,
    output: &mut BTreeMap<NodeId, (ManagedAnyCfgMatchCase, bool)>,
) {
    for expression in body
        .place_inputs
        .values()
        .chain(body.default_values.values())
    {
        collect_managed_any_match_arms_expr(expression, output);
    }
    collect_managed_any_match_arms_block(&body.root, output);
}

fn collect_managed_any_match_arms_block(
    block: &ResolvedBlock,
    output: &mut BTreeMap<NodeId, (ManagedAnyCfgMatchCase, bool)>,
) {
    for statement in &block.statements {
        collect_managed_any_match_arms_stmt(statement, output);
    }
    if let Some(result) = &block.result {
        collect_managed_any_match_arms_expr(result, output);
    }
}

fn collect_managed_any_match_arms_stmt(
    statement: &ResolvedStmt,
    output: &mut BTreeMap<NodeId, (ManagedAnyCfgMatchCase, bool)>,
) {
    match &statement.kind {
        ResolvedStmtKind::Bind { initializer, .. } => {
            if let Some(initializer) = initializer {
                collect_managed_any_match_arms_expr(initializer, output);
            }
        }
        ResolvedStmtKind::Assign { value, .. } => {
            collect_managed_any_match_arms_expr(value, output)
        }
        ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
            if let Some(value) = value {
                collect_managed_any_match_arms_expr(value, output);
            }
        }
        ResolvedStmtKind::Continue
        | ResolvedStmtKind::Drop(_)
        | ResolvedStmtKind::NestedCallable(_) => {}
        ResolvedStmtKind::Expr(expression) => {
            collect_managed_any_match_arms_expr(expression, output)
        }
        ResolvedStmtKind::While { condition, body } => {
            collect_managed_any_match_arms_expr(condition, output);
            collect_managed_any_match_arms_block(body, output);
        }
        ResolvedStmtKind::WhileLet {
            initializer, body, ..
        } => {
            collect_managed_any_match_arms_expr(initializer, output);
            collect_managed_any_match_arms_block(body, output);
        }
        ResolvedStmtKind::IfLet {
            initializer,
            then_block,
            else_block,
            ..
        } => {
            collect_managed_any_match_arms_expr(initializer, output);
            collect_managed_any_match_arms_block(then_block, output);
            if let Some(else_block) = else_block {
                collect_managed_any_match_arms_block(else_block, output);
            }
        }
        ResolvedStmtKind::Loop(body)
        | ResolvedStmtKind::Scope { body, .. }
        | ResolvedStmtKind::Pinned { body, .. } => {
            if let ResolvedStmtKind::Pinned { value, .. } = &statement.kind {
                collect_managed_any_match_arms_expr(value, output);
            }
            collect_managed_any_match_arms_block(body, output);
        }
        ResolvedStmtKind::For { iterable, body, .. } => {
            collect_managed_any_match_arms_expr(iterable, output);
            collect_managed_any_match_arms_block(body, output);
        }
        ResolvedStmtKind::Contract { condition, .. } => {
            collect_managed_any_match_arms_expr(condition, output)
        }
        ResolvedStmtKind::Math(expressions) => {
            for expression in expressions {
                collect_managed_any_match_arms_expr(expression, output);
            }
        }
    }
}

fn collect_managed_any_match_arms_expr(
    expression: &ResolvedExpr,
    output: &mut BTreeMap<NodeId, (ManagedAnyCfgMatchCase, bool)>,
) {
    match &expression.kind {
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let ResolvedFStringPart::Interpolation(value) = part {
                    collect_managed_any_match_arms_expr(value, output);
                }
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            collect_managed_any_match_arms_expr(value, output);
            if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                collect_managed_any_match_arms_expr(index, output);
            }
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            collect_managed_any_match_arms_expr(left, output);
            collect_managed_any_match_arms_expr(right, output);
        }
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::TypeOf(operand)
        | ResolvedExprKind::Old(operand)
        | ResolvedExprKind::Try { value: operand, .. }
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand) => collect_managed_any_match_arms_expr(operand, output),
        ResolvedExprKind::Call(call) => {
            for argument in &call.arguments {
                collect_managed_any_match_arms_expr(&argument.value, output);
            }
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => {
            for value in values {
                collect_managed_any_match_arms_expr(value, output);
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                collect_managed_any_match_arms_expr(key, output);
                collect_managed_any_match_arms_expr(value, output);
            }
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            collect_managed_any_match_arms_expr(value, output);
            collect_managed_any_match_arms_expr(iterable, output);
            if let Some(guard) = guard {
                collect_managed_any_match_arms_expr(guard, output);
            }
        }
        ResolvedExprKind::OptionalChain { receiver, .. } => {
            collect_managed_any_match_arms_expr(receiver, output)
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                collect_managed_any_match_arms_expr(&field.value, output);
            }
            if let Some(rest) = rest {
                collect_managed_any_match_arms_expr(rest, output);
            }
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => collect_managed_any_match_arms_block(block, output),
        ResolvedExprKind::Scope { body, .. } => collect_managed_any_match_arms_block(body, output),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_managed_any_match_arms_expr(condition, output);
            collect_managed_any_match_arms_block(then_block, output);
            collect_managed_any_match_arms_block(else_block, output);
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            collect_managed_any_match_arms_expr(scrutinee, output);
            for arm in arms {
                let case = match &arm.pattern.kind {
                    ResolvedPatternKind::Wildcard => ManagedAnyCfgMatchCase::Default,
                    ResolvedPatternKind::Binding { .. } => ManagedAnyCfgMatchCase::Unsupported,
                    ResolvedPatternKind::Literal(literal) => {
                        ManagedAnyCfgMatchCase::Literal(literal.clone())
                    }
                    ResolvedPatternKind::Constructor { variant, .. } => {
                        ManagedAnyCfgMatchCase::Variant(variant.clone())
                    }
                    ResolvedPatternKind::Tuple(_)
                    | ResolvedPatternKind::Array(_)
                    | ResolvedPatternKind::Slice { .. } => ManagedAnyCfgMatchCase::Unsupported,
                };
                output.insert(arm.node_id.clone(), (case, arm.guard.is_some()));
                if let Some(guard) = &arm.guard {
                    collect_managed_any_match_arms_expr(guard, output);
                }
                collect_managed_any_match_arms_expr(&arm.body, output);
            }
        }
        ResolvedExprKind::Range { start, end } => {
            collect_managed_any_match_arms_expr(start, output);
            collect_managed_any_match_arms_expr(end, output);
        }
        ResolvedExprKind::Slice { target, start, end } => {
            collect_managed_any_match_arms_expr(target, output);
            if let Some(start) = start {
                collect_managed_any_match_arms_expr(start, output);
            }
            if let Some(end) = end {
                collect_managed_any_match_arms_expr(end, output);
            }
        }
        ResolvedExprKind::Cast { value, .. } => collect_managed_any_match_arms_expr(value, output),
        ResolvedExprKind::Lambda(lambda) => {
            collect_managed_any_match_arms_block(&lambda.body, output)
        }
    }
}

/// Closed checker observation of one Map-root lifecycle operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MapRootActionKind {
    New,
    Set,
    Remove,
    Size,
    Contains,
    Drop,
}

/// Checker-owned root identity and operation point for Map lifecycle work.
/// `root` is the result root for `New`/`Set`, and the observed/consumed root
/// for `Size`/`Contains`/`Drop`. `source` is present only for a persistent
/// `Set` or `Remove` update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MapRootAction {
    pub(crate) kind: MapRootActionKind,
    pub(crate) point: NodeId,
    /// Local that owns the root at this point. For `New`/`Set` this is the
    /// result binding; for `Size`/`Drop` this is the observed/consumed root.
    pub(crate) local: ResolvedLocalId,
    pub(crate) root: ResourceId,
    /// Static UTF-8 key for `Set`, `Remove`, or `Contains`; the MapRoot profile
    /// only admits literals without embedded NUL so every consumer observes
    /// the same key.
    pub(crate) key: Option<String>,
    /// Exact resolved key expression for key-taking operations. Keeping it
    /// alongside the static key lets independent lifecycle receipts join on
    /// source operand identity rather than key contents alone.
    pub(crate) key_expression: Option<NodeId>,
    /// Value expression consumed by `Set`. The canonical lowering maps this
    /// stable node identity to its SSA value and the MIR receipt binds it.
    pub(crate) value: Option<NodeId>,
    /// Exact Checker-resolved payload type for `Set`. The first typed
    /// MapRoot extensions admit only i32 and owned String values.
    pub(crate) value_type: Option<ResolvedTypeId>,
    /// Previous local consumed by persistent `Set`.
    pub(crate) source_local: Option<ResolvedLocalId>,
    pub(crate) source: Option<ResourceId>,
}

/// Schema/profile version for the separately materialized MapAnyOwner
/// identity. A type, action, or MIR receipt from MapRoot cannot stand in for
/// this marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MapAnyOwnerProfileVersion {
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MapAnyOwnerActionKind {
    New,
    Set,
    Remove,
    Size,
    Contains,
    Drop,
}

impl MapAnyOwnerProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "map_any_owner_v1",
        }
    }
}

impl MapAnyOwnerActionKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Set => "set",
            Self::Remove => "remove",
            Self::Size => "size",
            Self::Contains => "contains",
            Self::Drop => "drop",
        }
    }
}

/// Checker-owned root identity and operation point for the closed
/// MapAnyOwner v1 lifecycle. The fields intentionally mirror MapRoot so the
/// first MIR adapter can compare the same typed facts while retaining an
/// independent ledger and synthetic TypeDesc identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MapAnyOwnerAction {
    pub(crate) profile_version: MapAnyOwnerProfileVersion,
    pub(crate) kind: MapAnyOwnerActionKind,
    pub(crate) point: NodeId,
    pub(crate) local: ResolvedLocalId,
    pub(crate) root: ResourceId,
    pub(crate) key: Option<String>,
    /// Exact resolved key operand for key-taking operations.
    pub(crate) key_expression: Option<NodeId>,
    pub(crate) value: Option<NodeId>,
    /// Exact Checker-resolved payload type for a Set. MapAnyOwner v1 admits
    /// only the primitive i32 and owned String payload descriptors.
    pub(crate) value_type: Option<ResolvedTypeId>,
    pub(crate) source_local: Option<ResolvedLocalId>,
    pub(crate) source: Option<ResourceId>,
}

/// Schema/profile version for the typed ordinary Map owner lifecycle slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnerLifecycleProfileVersion {
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnerLifecycleActionKind {
    New,
    Clone,
    Set,
    Remove,
    Size,
    HasKey,
    Drop,
}

impl OrdinaryMapOwnerLifecycleProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "ordinary-map-owner-lifecycle-i32-string-v1",
        }
    }
}

impl OrdinaryMapOwnerLifecycleActionKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Clone => "clone",
            Self::Set => "set",
            Self::Remove => "remove",
            Self::Size => "size",
            Self::HasKey => "has_key",
            Self::Drop => "drop",
        }
    }
}

/// One independently Checker-recognized operation in the i32/String ordinary
/// Map owner lifecycle. Keys retain the typed String expression identity so
/// dynamic UTF-8 keys have the same evidence shape as literals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnerLifecycleAction {
    pub(crate) profile_version: OrdinaryMapOwnerLifecycleProfileVersion,
    pub(crate) kind: OrdinaryMapOwnerLifecycleActionKind,
    pub(crate) point: NodeId,
    pub(crate) local: ResolvedLocalId,
    pub(crate) root: ResourceId,
    pub(crate) key: Option<NodeId>,
    pub(crate) key_type: Option<ResolvedTypeId>,
    /// Direct local backing a borrowed String key, if the source key is an
    /// unprojected `Load`. The expression node above remains the semantic
    /// origin; this local is the exact storage identity used by MIR.
    pub(crate) key_local: Option<ResolvedLocalId>,
    pub(crate) value: Option<NodeId>,
    pub(crate) value_type: Option<ResolvedTypeId>,
    pub(crate) source_local: Option<ResolvedLocalId>,
    pub(crate) source: Option<ResourceId>,
}

/// Full operation-attempt vocabulary for ordinary Map expressions. Only a
/// subset can currently produce profile actions; unsupported kinds stay in
/// the independent attempt ledger and therefore cannot be hidden by a valid
/// local lifecycle elsewhere in the callable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnerLifecycleAttemptKind {
    New,
    Clone,
    Get,
    Set,
    Remove,
    Size,
    HasKey,
    Drop,
    ScopeDrop,
    Keys,
    Values,
    FromList,
    MapLiteral,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnerLifecycleAttempt {
    pub(crate) kind: OrdinaryMapOwnerLifecycleAttemptKind,
    pub(crate) point: NodeId,
    pub(crate) arguments: Vec<NodeId>,
    /// Direct unprojected receiver/root local for Map calls, when present.
    /// The argument NodeIds remain the exact operand identities; this local
    /// records the root edge needed to join an attempt to a MapAny receipt.
    pub(crate) receiver_local: Option<ResolvedLocalId>,
    /// Bound result local for a direct Map root-producing call, when present.
    /// It ties New/Set/Remove receipts to the exact destination root.
    pub(crate) result_local: Option<ResolvedLocalId>,
}

/// Schema version for the narrow direct `to_json(Map)` / `println(Map)`
/// observation evidence attached to ordinary Map lifecycle v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnerObserverProfileVersion {
    V1,
}

impl OrdinaryMapOwnerObserverProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "ordinary-map-owner-observer-v1",
        }
    }
}

/// A checked direct builtin call whose argument types include the opaque
/// builtin Map root type. Unsupported shapes remain in this census and cannot
/// be hidden by an otherwise complete lifecycle receipt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnerObserverAttempt {
    pub(crate) owner: NodeId,
    pub(crate) callsite: NodeId,
    pub(crate) builtin: BuiltinId,
    pub(crate) arguments: Vec<NodeId>,
    pub(crate) argument_types: Vec<ResolvedTypeId>,
    /// Direct unprojected locals at the corresponding argument positions.
    /// Keeping this separate from the type/node census lets the receipt
    /// checker tie its selected Map root to the exact loaded argument.
    pub(crate) argument_locals: Vec<Option<ResolvedLocalId>>,
    pub(crate) result_type: ResolvedTypeId,
    /// Canonical direct effect from the builtin effect table, including an
    /// empty vector for pure `to_json`.
    pub(crate) direct_effects: Vec<EffectId>,
    /// Effect summary currently carried by the resolved builtin call node.
    /// Some builtin lowering paths leave this empty; keep it distinct from
    /// the canonical direct effect above.
    pub(crate) typed_call_effects: Vec<EffectId>,
}

/// One Checker-owned proof that a specific direct builtin observes one live
/// lifecycle-v1 Map root without consuming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnerObserverReceipt {
    pub(crate) profile_version: OrdinaryMapOwnerObserverProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) callsite: NodeId,
    pub(crate) builtin: BuiltinId,
    pub(crate) argument: NodeId,
    pub(crate) root_local: ResolvedLocalId,
    pub(crate) root: ResourceId,
    pub(crate) root_type: ResolvedTypeId,
    /// The exact checked type passed through the observer's shared read.
    /// For v1 this must equal both the root type and the argument conversion.
    pub(crate) borrow_type: ResolvedTypeId,
    pub(crate) result_type: ResolvedTypeId,
    pub(crate) direct_effects: Vec<EffectId>,
    pub(crate) typed_call_effects: Vec<EffectId>,
}

/// Schema/profile version for the first ordinary Map/Any owner-v2 slice.
/// Unlike the older i32/String profile, this profile admits one concrete
/// nominal record payload (for example `Label { text: string }`) and records
/// the payload as a persistent clone on `map_set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ProfileVersion {
    V2,
}

impl OrdinaryMapOwnedAnyV2ProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V2 => "ordinary-map-owned-any-v2",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ActionKind {
    New,
    /// Create an independently owned Map root by cloning a live direct local
    /// root. The source remains live and the action keeps its exact type/root
    /// edge so payload-selection lineage can cross the clone.
    Clone,
    Set,
    Remove,
    Size,
    HasKey,
    IsEmpty,
    /// Borrow a live Map root and produce the exact `(bool, DynamicAny)`
    /// result for one key. The result has its own owner/settlement receipt;
    /// this action only proves the source Map and key edge.
    GetAny,
    /// Borrow a live Map root and produce the exact `List<DynamicAny>`
    /// snapshot. The Map root remains live and the result is settled
    /// independently from the root lifecycle.
    Values,
    /// Borrow a live Map root and produce an independent, owned `List<String>`
    /// snapshot of its keys. The root remains live and is settled separately.
    Keys,
    Drop,
}

impl OrdinaryMapOwnedAnyV2ActionKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Clone => "clone",
            Self::Set => "set",
            Self::Remove => "remove",
            Self::Size => "size",
            Self::HasKey => "has_key",
            Self::IsEmpty => "is_empty",
            Self::GetAny => "get_any",
            Self::Values => "values",
            Self::Keys => "keys",
            Self::Drop => "drop",
        }
    }
}

/// One exact source operation admitted by the v2 checker receipt. `source`
/// and `source_local` retain the persistent root chain (`empty -> one -> two`
/// in the record fixture), including an independent Clone edge.
/// `payload_clone` is true for every Set: the
/// persistent update copies the payload and therefore never consumes a user
/// binding such as `label`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2Action {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) kind: OrdinaryMapOwnedAnyV2ActionKind,
    pub(crate) point: NodeId,
    pub(crate) local: ResolvedLocalId,
    pub(crate) root: ResourceId,
    pub(crate) key: Option<NodeId>,
    pub(crate) key_type: Option<ResolvedTypeId>,
    pub(crate) key_local: Option<ResolvedLocalId>,
    /// Checker mutability of the direct local key binding, when the key is an
    /// unprojected local load.  The MIR copy keeps this fact beside the local
    /// identity so a narrow route can require an immutable, non-rebindable
    /// computed key without consulting surface syntax.
    pub(crate) key_local_mutable: Option<bool>,
    /// Checker classification of the exact String key expression. A local
    /// key is borrowed for the operation; literals and computed expressions
    /// are owned temporaries whose MIR lifetime must be settled at this use.
    pub(crate) key_provenance: Option<OrdinaryMapOwnedAnyV2KeyProvenance>,
    /// Digest of the exact string bytes when Checker can evaluate this key
    /// from literals and materialized string constants. This commits a narrow
    /// route receipt to the key value, not only to its expression identity.
    pub(crate) key_static_digest: Option<String>,
    pub(crate) value: Option<NodeId>,
    pub(crate) value_type: Option<ResolvedTypeId>,
    pub(crate) payload_clone: bool,
    pub(crate) source_local: Option<ResolvedLocalId>,
    pub(crate) source: Option<ResourceId>,
    /// Exact source operand node and type for Clone/GetAny/Values actions.
    /// For Clone, this is the source expression and its resolved Map-root
    /// type; `local`/`root` identify the destination obligation while
    /// `source_local`/`source` identify the still-live source obligation.
    /// For GetAny/Values/Keys, `local` and `root` identify the observed root.
    pub(crate) source_node: Option<NodeId>,
    pub(crate) source_type: Option<ResolvedTypeId>,
    /// Exact result type and direct binding for GetAny/Values/Keys. These fields
    /// remain empty on root lifecycle actions, including Clone.
    pub(crate) result_type: Option<ResolvedTypeId>,
    pub(crate) result_local: Option<ResolvedLocalId>,
}

/// Exact source contract for a local `map_new() -> Result<Record, i32>`
/// followed by an exhaustive, unguarded Result match. A successful New owns
/// one root exclusively in `ok_block`; Err owns only its Copy i32 status.
/// The receipt deliberately cannot authorize the older unconditional New
/// action, whose destination was a root before any variant was selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2NewResultReceipt {
    pub(crate) owner: NodeId,
    pub(crate) operation: NodeId,
    pub(crate) result_binding_point: NodeId,
    pub(crate) result_local: ResolvedLocalId,
    pub(crate) source_result_type: ResolvedTypeId,
    pub(crate) root_type: ResolvedTypeId,
    pub(crate) status_type: ResolvedTypeId,
    pub(crate) match_node: NodeId,
    pub(crate) scrutinee: NodeId,
    pub(crate) ok_arm: NodeId,
    pub(crate) ok_pattern: NodeId,
    pub(crate) ok_binding_pattern: NodeId,
    pub(crate) root_local: ResolvedLocalId,
    pub(crate) root: ResourceId,
    pub(crate) size_operation: NodeId,
    pub(crate) size_receiver: NodeId,
    pub(crate) size_binding_point: NodeId,
    pub(crate) size_local: ResolvedLocalId,
    pub(crate) ok_result: NodeId,
    pub(crate) root_drop: NodeId,
    pub(crate) err_arm: NodeId,
    pub(crate) err_pattern: NodeId,
    pub(crate) err_binding_pattern: NodeId,
    pub(crate) status_local: ResolvedLocalId,
    pub(crate) err_result: NodeId,
    pub(crate) entry: BasicBlockId,
    pub(crate) ok_block: BasicBlockId,
    pub(crate) err_block: BasicBlockId,
    pub(crate) join: BasicBlockId,
    pub(crate) ok_edge: EdgeId,
    pub(crate) err_edge: EdgeId,
    pub(crate) ok_join_edge: EdgeId,
    pub(crate) err_join_edge: EdgeId,
    /// Full source-CFG commitment. This is descriptive evidence until a
    /// Result consumer validates the exact New/match/payload identities.
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

/// Versioned Checker source proof for `map_new() -> Result<Record, i32>`
/// followed by `map_set(root, key, value) -> Result<Record, i32>`. It binds
/// both Result dispatches, both roots, every branch-local drop, returned
/// status/value, and the complete source CFG snapshot. A NewResult receipt
/// cannot substitute for this richer lineage proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2SetLineageReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2SetLineageProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) new_operation: NodeId,
    pub(crate) new_result_binding: NodeId,
    pub(crate) new_result_local: ResolvedLocalId,
    pub(crate) new_result_type: ResolvedTypeId,
    pub(crate) initial_root_type: ResolvedTypeId,
    pub(crate) initial_root: ResourceId,
    pub(crate) outer_match: NodeId,
    pub(crate) outer_scrutinee: NodeId,
    pub(crate) outer_ok_arm: NodeId,
    pub(crate) outer_err_arm: NodeId,
    pub(crate) initial_root_pattern: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) set_operation: NodeId,
    pub(crate) set_receiver: NodeId,
    pub(crate) set_result_binding: NodeId,
    pub(crate) set_result_local: ResolvedLocalId,
    pub(crate) set_result_type: ResolvedTypeId,
    pub(crate) updated_root_type: ResolvedTypeId,
    pub(crate) updated_root: ResourceId,
    pub(crate) inner_match: NodeId,
    pub(crate) inner_scrutinee: NodeId,
    pub(crate) inner_ok_arm: NodeId,
    pub(crate) inner_err_arm: NodeId,
    pub(crate) updated_root_pattern: NodeId,
    pub(crate) updated_root_local: ResolvedLocalId,
    pub(crate) inner_status_pattern: NodeId,
    pub(crate) inner_status_local: ResolvedLocalId,
    pub(crate) outer_status_pattern: NodeId,
    pub(crate) outer_status_local: ResolvedLocalId,
    pub(crate) old_root_drop: NodeId,
    pub(crate) inner_error_root_drop: NodeId,
    pub(crate) size_operation: NodeId,
    pub(crate) size_receiver: NodeId,
    pub(crate) size_binding: NodeId,
    pub(crate) size_local: ResolvedLocalId,
    pub(crate) updated_root_drop: NodeId,
    pub(crate) inner_ok_result: NodeId,
    pub(crate) inner_err_result: NodeId,
    pub(crate) outer_err_result: NodeId,
    pub(crate) entry: BasicBlockId,
    pub(crate) outer_ok_block: BasicBlockId,
    pub(crate) outer_err_block: BasicBlockId,
    pub(crate) inner_dispatch_block: BasicBlockId,
    pub(crate) inner_ok_block: BasicBlockId,
    pub(crate) inner_err_block: BasicBlockId,
    pub(crate) inner_join_block: BasicBlockId,
    pub(crate) outer_join_block: BasicBlockId,
    pub(crate) outer_ok_edge: EdgeId,
    pub(crate) outer_err_edge: EdgeId,
    pub(crate) inner_ok_edge: EdgeId,
    pub(crate) inner_err_edge: EdgeId,
    pub(crate) inner_ok_join_edge: EdgeId,
    pub(crate) inner_err_join_edge: EdgeId,
    pub(crate) inner_to_outer_join_edge: EdgeId,
    pub(crate) outer_err_join_edge: EdgeId,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2SetLineageProfileVersion {
    V1,
}

/// Versioned Checker proof for the exact
/// `map_new -> Result match -> map_set("answer", 42) -> Result match ->
/// map_get_i32(updated, "answer") -> Result match` source slice. This is a
/// separate profile from SetLineage so the existing SetResult admission
/// remains closed over its original `map_size` tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2SetGetI32LineageReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2SetGetI32LineageProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) new_operation: NodeId,
    pub(crate) new_result_binding: NodeId,
    pub(crate) new_result_local: ResolvedLocalId,
    pub(crate) outer_match: NodeId,
    pub(crate) outer_scrutinee: NodeId,
    pub(crate) outer_ok_arm: NodeId,
    pub(crate) outer_err_arm: NodeId,
    pub(crate) initial_root_pattern: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) initial_root_type: ResolvedTypeId,
    pub(crate) initial_root: ResourceId,
    pub(crate) outer_status_pattern: NodeId,
    pub(crate) outer_status_local: ResolvedLocalId,
    pub(crate) outer_err_result: NodeId,
    pub(crate) set_operation: NodeId,
    pub(crate) set_receiver: NodeId,
    pub(crate) set_result_binding: NodeId,
    pub(crate) set_result_local: ResolvedLocalId,
    pub(crate) set_result_type: ResolvedTypeId,
    pub(crate) set_ok_arm: NodeId,
    pub(crate) set_err_arm: NodeId,
    pub(crate) updated_root_pattern: NodeId,
    pub(crate) updated_root_local: ResolvedLocalId,
    pub(crate) updated_root_type: ResolvedTypeId,
    pub(crate) updated_root: ResourceId,
    pub(crate) set_error_pattern: NodeId,
    pub(crate) set_error_local: ResolvedLocalId,
    pub(crate) set_error_result: NodeId,
    pub(crate) old_root_set_ok_drop: NodeId,
    pub(crate) old_root_set_err_drop: NodeId,
    pub(crate) get_operation: NodeId,
    pub(crate) get_receiver: NodeId,
    pub(crate) get_receiver_local: ResolvedLocalId,
    pub(crate) get_key: NodeId,
    pub(crate) get_key_type: ResolvedTypeId,
    pub(crate) get_key_value: String,
    pub(crate) get_result_binding: NodeId,
    pub(crate) get_result_local: ResolvedLocalId,
    pub(crate) get_result_type: ResolvedTypeId,
    pub(crate) get_match: NodeId,
    pub(crate) get_scrutinee: NodeId,
    pub(crate) get_ok_arm: NodeId,
    pub(crate) get_err_arm: NodeId,
    pub(crate) get_ok_pattern: NodeId,
    pub(crate) get_ok_local: ResolvedLocalId,
    pub(crate) get_ok_result: NodeId,
    pub(crate) get_err_pattern: NodeId,
    pub(crate) get_err_local: ResolvedLocalId,
    pub(crate) get_err_result: NodeId,
    pub(crate) updated_root_get_ok_drop: NodeId,
    pub(crate) updated_root_get_err_drop: NodeId,
    pub(crate) entry: BasicBlockId,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2SetGetI32LineageProfileVersion {
    V1,
}

/// Exact fallible New/Set followed by one ordinary `map_get` and direct i32
/// unpack of its Any payload. This profile is distinct from the Result-valued
/// `map_get_i32` profile: the generic Get returns `(bool, Any)`, the pair is
/// explicitly dropped, and the boolean selects either the unpacked hit value
/// or the canonical miss value zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2SetGetAnyI32LineageReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2SetGetAnyI32LineageProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) new_operation: NodeId,
    pub(crate) new_result_binding: NodeId,
    pub(crate) new_result_local: ResolvedLocalId,
    pub(crate) new_result_type: ResolvedTypeId,
    pub(crate) outer_match: NodeId,
    pub(crate) outer_scrutinee: NodeId,
    pub(crate) outer_ok_arm: NodeId,
    pub(crate) outer_err_arm: NodeId,
    pub(crate) initial_root_pattern: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) initial_root_type: ResolvedTypeId,
    pub(crate) initial_root: ResourceId,
    pub(crate) outer_status_pattern: NodeId,
    pub(crate) outer_status_local: ResolvedLocalId,
    pub(crate) outer_err_result: NodeId,
    pub(crate) set_operation: NodeId,
    pub(crate) set_receiver: NodeId,
    pub(crate) set_key: NodeId,
    pub(crate) set_key_value: String,
    pub(crate) set_value: NodeId,
    pub(crate) set_i32_value: i64,
    pub(crate) set_result_binding: NodeId,
    pub(crate) set_result_local: ResolvedLocalId,
    pub(crate) set_result_type: ResolvedTypeId,
    pub(crate) set_match: NodeId,
    pub(crate) set_scrutinee: NodeId,
    pub(crate) set_ok_arm: NodeId,
    pub(crate) set_err_arm: NodeId,
    pub(crate) updated_root_pattern: NodeId,
    pub(crate) updated_root_local: ResolvedLocalId,
    pub(crate) updated_root_type: ResolvedTypeId,
    pub(crate) updated_root: ResourceId,
    pub(crate) set_error_pattern: NodeId,
    pub(crate) set_error_local: ResolvedLocalId,
    pub(crate) set_error_result: NodeId,
    pub(crate) old_root_set_ok_drop: NodeId,
    pub(crate) old_root_set_err_drop: NodeId,
    pub(crate) get_operation: NodeId,
    pub(crate) get_receiver: NodeId,
    pub(crate) get_receiver_local: ResolvedLocalId,
    pub(crate) get_key: NodeId,
    pub(crate) get_key_type: ResolvedTypeId,
    pub(crate) get_key_value: String,
    pub(crate) get_result_binding: NodeId,
    pub(crate) get_result_local: ResolvedLocalId,
    pub(crate) get_result_type: ResolvedTypeId,
    pub(crate) pair_found_projection: NodeId,
    pub(crate) pair_payload_projection: NodeId,
    pub(crate) found_binding: NodeId,
    pub(crate) found_local: ResolvedLocalId,
    pub(crate) unpack_operation: NodeId,
    pub(crate) unpack_argument: NodeId,
    pub(crate) unpack_conversion_kind: CheckedConversionKind,
    pub(crate) unpack_source_type: ResolvedTypeId,
    pub(crate) unpack_target_type: ResolvedTypeId,
    pub(crate) unpack_result_local: ResolvedLocalId,
    pub(crate) pair_drop: NodeId,
    pub(crate) updated_root_get_drop: NodeId,
    pub(crate) found_result_match: NodeId,
    pub(crate) found_result: NodeId,
    pub(crate) found_true_arm: NodeId,
    pub(crate) found_false_arm: NodeId,
    pub(crate) entry: BasicBlockId,
    /// Full source operation-attempt census. It must contain exactly the
    /// checked `map_new`, `map_set`, and generic `map_get` calls bound above.
    pub(crate) operations: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    /// Complete reachable Checker CFG commitment, including both Result
    /// dispatches, branch-local cleanup, and the Get-hit/miss result branch.
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2SetGetAnyStringLineageProfileVersion {
    V1,
}

/// Checker source authority for the narrow fallible New/Set lineage whose
/// legacy tuple-valued `map_get` returns a directly unpacked String owner.
/// This is deliberately separate from both the i32 execution receipt and the
/// Result-valued `map_get_any` profile. It binds the selected String payload
/// to the exact winning Set key, the pair/payload/root settlements, every
/// Map call attempt, the full source-body commitment, and the reachable CFG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2SetGetAnyStringLineageReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2SetGetAnyStringLineageProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) source_body_commitment: String,
    pub(crate) new_operation: NodeId,
    pub(crate) new_match: NodeId,
    pub(crate) new_scrutinee: NodeId,
    pub(crate) new_ok_arm: NodeId,
    pub(crate) new_err_arm: NodeId,
    pub(crate) empty_root_pattern: NodeId,
    pub(crate) empty_root_local: ResolvedLocalId,
    pub(crate) empty_root_type: ResolvedTypeId,
    pub(crate) new_status_pattern: NodeId,
    pub(crate) new_status_local: ResolvedLocalId,
    pub(crate) set_operation: NodeId,
    pub(crate) set_match: NodeId,
    pub(crate) set_scrutinee: NodeId,
    pub(crate) set_ok_arm: NodeId,
    pub(crate) set_err_arm: NodeId,
    pub(crate) set_receiver: NodeId,
    pub(crate) set_key: NodeId,
    pub(crate) set_key_value: String,
    pub(crate) set_payload: NodeId,
    pub(crate) set_payload_text: String,
    pub(crate) updated_root_pattern: NodeId,
    pub(crate) updated_root_local: ResolvedLocalId,
    pub(crate) updated_root_type: ResolvedTypeId,
    pub(crate) set_status_pattern: NodeId,
    pub(crate) set_status_local: ResolvedLocalId,
    pub(crate) empty_root_set_ok_drop: NodeId,
    pub(crate) empty_root_set_err_drop: NodeId,
    pub(crate) get_operation: NodeId,
    pub(crate) get_receiver: NodeId,
    pub(crate) get_key: NodeId,
    pub(crate) get_key_value: String,
    pub(crate) pair_binding: NodeId,
    pub(crate) pair_local: ResolvedLocalId,
    pub(crate) pair_type: ResolvedTypeId,
    pub(crate) pair_payload_projection: NodeId,
    pub(crate) unpack_operation: NodeId,
    pub(crate) unpack_conversion_kind: CheckedConversionKind,
    pub(crate) unpack_source_type: ResolvedTypeId,
    pub(crate) unpack_target_type: ResolvedTypeId,
    pub(crate) unpack_binding: NodeId,
    pub(crate) unpack_local: ResolvedLocalId,
    pub(crate) pair_drop: NodeId,
    pub(crate) unpack_drop: NodeId,
    pub(crate) updated_root_drop: NodeId,
    pub(crate) operations: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2DirectTupleGetUnpackProfileVersion {
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2DirectTupleGetUnpackPayload {
    I32(i64),
    String(String),
}

/// One Result-aware persistent Set edge in the direct-`match map_new`
/// source/CFG profile. The branch-local drops settle the incoming root on
/// both success and failure; the Ok binding becomes the next root owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2DirectTupleGetUnpackSetReceipt {
    pub(crate) operation: NodeId,
    pub(crate) arguments: Vec<NodeId>,
    pub(crate) input_root_local: ResolvedLocalId,
    pub(crate) key_node: NodeId,
    pub(crate) key_value: String,
    pub(crate) payload_node: NodeId,
    pub(crate) payload_type: ResolvedTypeId,
    pub(crate) payload: OrdinaryMapOwnedAnyV2DirectTupleGetUnpackPayload,
    pub(crate) result_type: ResolvedTypeId,
    pub(crate) match_node: NodeId,
    pub(crate) scrutinee: NodeId,
    pub(crate) ok_arm: NodeId,
    pub(crate) ok_pattern: NodeId,
    pub(crate) output_root_local: ResolvedLocalId,
    pub(crate) error_arm: NodeId,
    pub(crate) error_pattern: NodeId,
    pub(crate) error_local: ResolvedLocalId,
    pub(crate) ok_input_drop: NodeId,
    pub(crate) error_input_drop: NodeId,
}

/// Checker source and CFG authority for a narrow public legacy-shaped
/// `map_get -> (bool, Any)` unpack lineage. It requires a direct Result match
/// on `map_new()`, one or more nested Result matches on `map_set`, exact
/// branch cleanup, a static Get key, an owned tuple binding, and a
/// type-compatible last-writer Set (or the canonical i32 miss). It is L1
/// source evidence only: later MIR, verifier, bytecode, and native consumers
/// must independently bind and execute this same identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2DirectTupleGetUnpackReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2DirectTupleGetUnpackProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) source_body_commitment: String,
    pub(crate) new_operation: NodeId,
    pub(crate) new_result_type: ResolvedTypeId,
    pub(crate) new_match: NodeId,
    pub(crate) new_dispatch: BasicBlockId,
    pub(crate) new_scrutinee: NodeId,
    pub(crate) new_ok_arm: NodeId,
    pub(crate) new_ok_pattern: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) initial_root_type: ResolvedTypeId,
    pub(crate) new_error_arm: NodeId,
    pub(crate) new_error_pattern: NodeId,
    pub(crate) new_error_local: ResolvedLocalId,
    pub(crate) sets: Vec<OrdinaryMapOwnedAnyV2DirectTupleGetUnpackSetReceipt>,
    pub(crate) get_operation: NodeId,
    pub(crate) get_receiver_local: ResolvedLocalId,
    pub(crate) get_key_node: NodeId,
    pub(crate) get_key_value: String,
    pub(crate) pair_binding: NodeId,
    pub(crate) pair_local: ResolvedLocalId,
    pub(crate) pair_type: ResolvedTypeId,
    pub(crate) pair_payload_projection: NodeId,
    pub(crate) root_get_drop: NodeId,
    pub(crate) unpack_operation: NodeId,
    pub(crate) unpack_conversion_kind: CheckedConversionKind,
    pub(crate) unpack_source_type: ResolvedTypeId,
    pub(crate) unpack_target_type: ResolvedTypeId,
    pub(crate) unpack_local: ResolvedLocalId,
    pub(crate) pair_drop: NodeId,
    pub(crate) unpack_drop: NodeId,
    pub(crate) selected_winning_set: Option<NodeId>,
    pub(crate) operations: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2DirectPrintResultProfileVersion {
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2DirectPrintResultShape {
    HitI32,
    HitString,
    MissI32,
    SameRootMapSizeI32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2DirectPrintSetPayload {
    I32(i64),
    String(String),
}

/// Narrow Checker-owned source/CFG authority for the nested Result-match
/// direct-print shape. The proof binds every variant payload, persistent-root
/// transition, tuple projection, print, explicit Drop, and match edge. It is
/// deliberately independent of the ordinary V2 action ledger: that ledger's
/// current straight-line collector cannot model branch-local roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2DirectPrintResultReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2DirectPrintResultProfileVersion,
    pub(crate) shape: OrdinaryMapOwnedAnyV2DirectPrintResultShape,
    pub(crate) owner: NodeId,
    pub(crate) source_body_commitment: String,
    pub(crate) new_operation: NodeId,
    pub(crate) new_result_type: ResolvedTypeId,
    pub(crate) new_match: NodeId,
    pub(crate) new_dispatch: BasicBlockId,
    pub(crate) new_ok_arm: NodeId,
    pub(crate) new_ok_edge: EdgeId,
    pub(crate) new_ok_target: BasicBlockId,
    pub(crate) new_ok_pattern: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) initial_root_type: ResolvedTypeId,
    pub(crate) new_error_arm: NodeId,
    pub(crate) new_error_edge: EdgeId,
    pub(crate) new_error_target: BasicBlockId,
    pub(crate) new_error_pattern: NodeId,
    pub(crate) new_error_local: ResolvedLocalId,
    pub(crate) new_error_return: NodeId,
    pub(crate) set_operation: NodeId,
    pub(crate) set_result_type: ResolvedTypeId,
    pub(crate) set_key_value: String,
    pub(crate) set_payload_type: ResolvedTypeId,
    pub(crate) set_payload: OrdinaryMapOwnedAnyV2DirectPrintSetPayload,
    pub(crate) set_match: NodeId,
    pub(crate) set_dispatch: BasicBlockId,
    pub(crate) set_ok_arm: NodeId,
    pub(crate) set_ok_edge: EdgeId,
    pub(crate) set_ok_target: BasicBlockId,
    pub(crate) set_ok_pattern: NodeId,
    pub(crate) updated_root_local: ResolvedLocalId,
    pub(crate) set_error_arm: NodeId,
    pub(crate) set_error_edge: EdgeId,
    pub(crate) set_error_target: BasicBlockId,
    pub(crate) set_error_pattern: NodeId,
    pub(crate) set_error_local: ResolvedLocalId,
    pub(crate) set_error_return: NodeId,
    pub(crate) set_ok_input_drop: NodeId,
    pub(crate) set_error_input_drop: NodeId,
    pub(crate) get_operation: NodeId,
    pub(crate) get_receiver: NodeId,
    pub(crate) get_key: NodeId,
    pub(crate) get_key_value: String,
    pub(crate) pair_binding: NodeId,
    pub(crate) pair_local: ResolvedLocalId,
    pub(crate) pair_type: ResolvedTypeId,
    /// Lossless projection of the exact GetAny result into the established
    /// result receipt contract. The current production MIR consumers still
    /// require this shape inside the ordinary V2 CFG receipt; carrying it
    /// here makes the source identity mapping explicit for a later adapter.
    pub(crate) result_receipt: OrdinaryMapOwnedAnyV2ResultReceipt,
    pub(crate) root_get_drop: NodeId,
    pub(crate) payload_projection: NodeId,
    pub(crate) print_operation: NodeId,
    pub(crate) pair_drop: NodeId,
    pub(crate) size_operation: Option<NodeId>,
    pub(crate) size_receiver: Option<NodeId>,
    pub(crate) size_binding: Option<NodeId>,
    pub(crate) size_local: Option<ResolvedLocalId>,
    pub(crate) success_return: NodeId,
    pub(crate) operations: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedGetAnyTupleResultV1ProfileVersion {
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedGetAnyTupleResultV1MapActionKind {
    NewResult,
    SetResult,
    GetTuple,
    RootDrop,
    PairDrop,
    PayloadDrop,
    CarriedDrop,
}

/// One operation or owner-settlement event in the fixed Result-aware legacy
/// tuple Get lineage. The local and related-local identities keep persistent
/// roots, tuple owners, detached Any payloads, and carried values distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetAnyTupleResultV1Action {
    pub(crate) kind: OrdinaryMapOwnedGetAnyTupleResultV1MapActionKind,
    pub(crate) point: NodeId,
    pub(crate) operation: Option<NodeId>,
    pub(crate) local: Option<ResolvedLocalId>,
    pub(crate) related_local: Option<ResolvedLocalId>,
    pub(crate) value: Option<NodeId>,
    pub(crate) value_type: Option<ResolvedTypeId>,
}

/// Exact source arm census for one inline `match map_new()` or
/// `match map_set(...)`. Set arms carry both old-root settlement points so
/// the Err arm cannot be accepted while leaving its still-live source root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetAnyTupleResultV1ResultBranch {
    pub(crate) operation_kind: OrdinaryMapOwnedGetAnyTupleResultV1MapActionKind,
    pub(crate) operation: NodeId,
    pub(crate) result_type: ResolvedTypeId,
    pub(crate) match_node: NodeId,
    pub(crate) scrutinee: NodeId,
    pub(crate) ok_arm: NodeId,
    pub(crate) err_arm: NodeId,
    pub(crate) ok_local: ResolvedLocalId,
    pub(crate) err_local: ResolvedLocalId,
    pub(crate) source_root: Option<ResolvedLocalId>,
    /// The New Err variant owns no root; the Set Err variant never publishes
    /// its hypothetical updated root. These are source facts, not backend
    /// status-code interpretations.
    pub(crate) err_creates_root: bool,
    pub(crate) err_publishes_updated_root: bool,
    pub(crate) err_consumes_source_root: bool,
    pub(crate) ok_root_drop: Option<NodeId>,
    pub(crate) err_root_drop: Option<NodeId>,
    pub(crate) ok_result: NodeId,
    pub(crate) err_result: NodeId,
}

/// Exact owner chain for one old tuple-valued `map_get` call. This receipt
/// keeps tuple projection, generic identity call, observation, and explicit
/// exactly-once Drop sites separate, including the payload's Any type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetAnyTupleResultV1OwnerChain {
    pub(crate) get_operation: NodeId,
    pub(crate) root_local: ResolvedLocalId,
    pub(crate) key_node: NodeId,
    pub(crate) key_value: String,
    pub(crate) pair_local: ResolvedLocalId,
    pub(crate) pair_type: ResolvedTypeId,
    pub(crate) found_projection: NodeId,
    pub(crate) found_local: ResolvedLocalId,
    pub(crate) payload_projection: NodeId,
    pub(crate) payload_local: ResolvedLocalId,
    pub(crate) payload_type: ResolvedTypeId,
    pub(crate) carry_operation: NodeId,
    pub(crate) carry_callee: NodeId,
    pub(crate) carried_local: ResolvedLocalId,
    pub(crate) observation_points: Vec<NodeId>,
    /// Updated Map root produced by the matching Set Ok arm, settled after
    /// the tuple read. The Set input root is recorded on the Result branch.
    pub(crate) root_drop: NodeId,
    pub(crate) pair_drop: NodeId,
    pub(crate) payload_drop: NodeId,
    pub(crate) carried_drop: NodeId,
}

/// Checker-only authority for the single fixed SOURCE shape exercised by
/// the original pure-i32/Any escape test. It binds both Result dispatches per
/// root lineage, both old tuple GetAny owner chains, every Map attempt and
/// action observed by the independent scanners, a source-body commitment,
/// and the complete reachable source CFG. It deliberately does not reuse
/// `OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetAnyTupleResultV1Receipt {
    pub(crate) profile_version: OrdinaryMapOwnedGetAnyTupleResultV1ProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) source_body_commitment: String,
    pub(crate) result_branches: Vec<OrdinaryMapOwnedGetAnyTupleResultV1ResultBranch>,
    pub(crate) owner_chains: Vec<OrdinaryMapOwnedGetAnyTupleResultV1OwnerChain>,
    pub(crate) map_actions: Vec<OrdinaryMapOwnedGetAnyTupleResultV1Action>,
    pub(crate) owned_get_attempts: BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
    pub(crate) owned_get_actions: Vec<OrdinaryMapOwnedGetI32StringV1Action>,
    pub(crate) any_v2_attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    pub(crate) any_v2_actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

/// The Get API is part of this receipt's authority. The older tuple-valued
/// `map_get` operation is deliberately represented by a different receipt
/// type and cannot be substituted for the Result-valued `map_get_any` API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2GetApi {
    /// Result-valued builtin: returns `Result<(bool, Any), i32>`.
    MapGetAnyResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageProfileVersion {
    V1,
}

/// Exact fallible New/Set followed by Result-valued `map_get_any` and one
/// checked DynamicAny-to-i32 unpack. The Get Err arm propagates its exact i32
/// status after settling the updated root. The Get Ok arm settles the pair
/// (which owns the Any payload) and the updated root before choosing the hit
/// value or canonical miss result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageProfileVersion,
    pub(crate) get_api: OrdinaryMapOwnedAnyV2GetApi,
    pub(crate) owner: NodeId,
    /// Optional exact direct `ensures` prelude admitted for the L2 source
    /// proof. Both identities are committed by the Checker receipt; the MIR
    /// route still validates the predicate's meaning against this callable.
    pub(crate) proof_ensures: Option<NodeId>,
    pub(crate) proof_ensures_expression: Option<NodeId>,
    pub(crate) new_operation: NodeId,
    pub(crate) new_result_binding: NodeId,
    pub(crate) new_result_local: ResolvedLocalId,
    pub(crate) new_result_type: ResolvedTypeId,
    pub(crate) new_match: NodeId,
    pub(crate) new_scrutinee: NodeId,
    pub(crate) new_ok_arm: NodeId,
    pub(crate) new_err_arm: NodeId,
    pub(crate) initial_root_pattern: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) initial_root_type: ResolvedTypeId,
    pub(crate) initial_root: ResourceId,
    pub(crate) new_error_pattern: NodeId,
    pub(crate) new_error_local: ResolvedLocalId,
    pub(crate) new_error_result: NodeId,
    pub(crate) set_operation: NodeId,
    pub(crate) set_receiver: NodeId,
    pub(crate) set_key: NodeId,
    pub(crate) set_key_value: String,
    pub(crate) set_value: NodeId,
    pub(crate) set_i32_value: i64,
    pub(crate) set_result_binding: NodeId,
    pub(crate) set_result_local: ResolvedLocalId,
    pub(crate) set_result_type: ResolvedTypeId,
    pub(crate) set_match: NodeId,
    pub(crate) set_scrutinee: NodeId,
    pub(crate) set_ok_arm: NodeId,
    pub(crate) set_err_arm: NodeId,
    pub(crate) updated_root_pattern: NodeId,
    pub(crate) updated_root_local: ResolvedLocalId,
    pub(crate) updated_root_type: ResolvedTypeId,
    pub(crate) updated_root: ResourceId,
    pub(crate) set_error_pattern: NodeId,
    pub(crate) set_error_local: ResolvedLocalId,
    pub(crate) set_error_result: NodeId,
    pub(crate) old_root_set_ok_drop: NodeId,
    pub(crate) old_root_set_err_drop: NodeId,
    pub(crate) get_operation: NodeId,
    pub(crate) get_receiver: NodeId,
    pub(crate) get_receiver_local: ResolvedLocalId,
    pub(crate) get_key: NodeId,
    pub(crate) get_key_type: ResolvedTypeId,
    pub(crate) get_key_value: String,
    pub(crate) get_result_binding: NodeId,
    pub(crate) get_result_local: ResolvedLocalId,
    pub(crate) get_result_type: ResolvedTypeId,
    pub(crate) get_match: NodeId,
    pub(crate) get_scrutinee: NodeId,
    pub(crate) get_ok_arm: NodeId,
    pub(crate) get_err_arm: NodeId,
    pub(crate) get_ok_pair_pattern: NodeId,
    pub(crate) get_ok_pair_local: ResolvedLocalId,
    pub(crate) get_ok_pair_type: ResolvedTypeId,
    pub(crate) get_err_pattern: NodeId,
    pub(crate) get_err_local: ResolvedLocalId,
    pub(crate) get_err_result: NodeId,
    pub(crate) get_pair_found_projection: NodeId,
    pub(crate) get_found_binding: NodeId,
    pub(crate) get_found_local: ResolvedLocalId,
    pub(crate) get_pair_payload_projection: NodeId,
    pub(crate) get_unpack_operation: NodeId,
    pub(crate) get_unpack_argument: NodeId,
    pub(crate) get_unpack_conversion_kind: CheckedConversionKind,
    pub(crate) get_unpack_source_type: ResolvedTypeId,
    pub(crate) get_unpack_target_type: ResolvedTypeId,
    pub(crate) get_unpack_result_local: ResolvedLocalId,
    pub(crate) get_ok_pair_payload_settlement: NodeId,
    pub(crate) get_ok_updated_root_drop: NodeId,
    pub(crate) get_err_updated_root_drop: NodeId,
    pub(crate) get_found_result: NodeId,
    pub(crate) get_found_condition: NodeId,
    pub(crate) get_found_true_arm: NodeId,
    pub(crate) get_found_false_arm: NodeId,
    pub(crate) entry: BasicBlockId,
    /// Full source operation-attempt census. `map_get_any` is checked here as
    /// its own builtin identity, separate from `map_get`'s tuple receipt.
    pub(crate) operations: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2SetGetAnyI32LineageProfileVersion {
    V1,
}

/// Versioned Checker proof for one exact `map_new -> map_set(String) ->
/// map_get_string -> Ok(String)` owner path. The Get Result's String payload
/// is moved out while the updated Map root is live, compared by content with
/// the Set payload, then the updated Map root and detached String are settled
/// before the boolean selects the observed result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2SetGetStringLineageReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2SetGetStringLineageProfileVersion,
    pub(crate) owner: NodeId,
    /// Optional, exact direct L2 Ensures root. L1 profiles carry `None`; a
    /// verifier projection requires one of the two Checker-bound direct
    /// result-domain predicates: the positive `result == 42 || result == 2 ||
    /// result == 3` claim or the intentional false claim `result == 42 ||
    /// result == 3` used to require a Disproven verifier result.
    pub(crate) proof_ensures: Option<NodeId>,
    pub(crate) proof_ensures_expression: Option<NodeId>,
    pub(crate) new_operation: NodeId,
    pub(crate) new_result_binding: NodeId,
    pub(crate) new_result_local: ResolvedLocalId,
    pub(crate) outer_match: NodeId,
    pub(crate) outer_scrutinee: NodeId,
    pub(crate) outer_ok_arm: NodeId,
    pub(crate) outer_err_arm: NodeId,
    pub(crate) initial_root_pattern: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) initial_root_type: ResolvedTypeId,
    pub(crate) initial_root: ResourceId,
    pub(crate) outer_status_pattern: NodeId,
    pub(crate) outer_status_local: ResolvedLocalId,
    pub(crate) outer_err_result: NodeId,
    pub(crate) set_payload_binding: NodeId,
    pub(crate) set_payload_local: ResolvedLocalId,
    pub(crate) set_payload_literal: NodeId,
    pub(crate) set_payload_text: String,
    pub(crate) set_operation: NodeId,
    pub(crate) set_receiver: NodeId,
    pub(crate) set_key: NodeId,
    pub(crate) set_key_value: String,
    pub(crate) set_result_binding: NodeId,
    pub(crate) set_result_local: ResolvedLocalId,
    pub(crate) set_result_type: ResolvedTypeId,
    pub(crate) set_ok_arm: NodeId,
    pub(crate) set_err_arm: NodeId,
    pub(crate) updated_root_pattern: NodeId,
    pub(crate) updated_root_local: ResolvedLocalId,
    pub(crate) updated_root_type: ResolvedTypeId,
    pub(crate) updated_root: ResourceId,
    pub(crate) set_error_pattern: NodeId,
    pub(crate) set_error_local: ResolvedLocalId,
    pub(crate) set_error_result: NodeId,
    pub(crate) set_payload_drop: NodeId,
    pub(crate) old_root_set_ok_drop: NodeId,
    pub(crate) old_root_set_err_drop: NodeId,
    pub(crate) get_operation: NodeId,
    pub(crate) get_receiver: NodeId,
    pub(crate) get_receiver_local: ResolvedLocalId,
    pub(crate) get_key: NodeId,
    pub(crate) get_key_type: ResolvedTypeId,
    pub(crate) get_key_value: String,
    pub(crate) get_result_binding: NodeId,
    pub(crate) get_result_local: ResolvedLocalId,
    pub(crate) get_result_type: ResolvedTypeId,
    pub(crate) get_match: NodeId,
    pub(crate) get_scrutinee: NodeId,
    pub(crate) get_ok_arm: NodeId,
    pub(crate) get_err_arm: NodeId,
    pub(crate) get_ok_pattern: NodeId,
    pub(crate) get_ok_local: ResolvedLocalId,
    pub(crate) get_ok_compare_binding: NodeId,
    pub(crate) get_ok_compare_local: ResolvedLocalId,
    pub(crate) get_ok_compare_expression: NodeId,
    pub(crate) get_ok_compare_literal: NodeId,
    pub(crate) get_ok_compare_text: String,
    pub(crate) get_ok_if: NodeId,
    pub(crate) get_ok_if_condition: NodeId,
    pub(crate) get_ok_if_then_block: NodeId,
    pub(crate) get_ok_if_else_block: NodeId,
    pub(crate) get_ok_if_then_edge: EdgeId,
    pub(crate) get_ok_if_else_edge: EdgeId,
    pub(crate) get_ok_if_then_cfg_block: BasicBlockId,
    pub(crate) get_ok_if_else_cfg_block: BasicBlockId,
    pub(crate) get_ok_if_then_join_edge: EdgeId,
    pub(crate) get_ok_if_else_join_edge: EdgeId,
    pub(crate) get_ok_if_join_cfg_block: BasicBlockId,
    pub(crate) get_ok_true_result: NodeId,
    pub(crate) get_ok_false_result: NodeId,
    pub(crate) get_ok_drop: NodeId,
    pub(crate) get_ok_result: NodeId,
    pub(crate) get_err_pattern: NodeId,
    pub(crate) get_err_local: ResolvedLocalId,
    pub(crate) get_err_result: NodeId,
    pub(crate) updated_root_get_ok_drop: NodeId,
    pub(crate) updated_root_get_err_drop: NodeId,
    pub(crate) entry: BasicBlockId,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2SetGetStringLineageProfileVersion {
    V1,
}

/// Source-certified origin class for an ordinary Map/Any V2 String key.
/// The action's `key` NodeId and `key_type` remain the exact expression and
/// type identities; this small tag tells consumers how to treat its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2KeyProvenance {
    DirectLocal,
    StringLiteral,
    StaticConstant,
    StaticConcat,
    ComputedString,
}

/// The attempt vocabulary is intentionally wider than the admitted v2
/// actions. Unsupported operations remain in this set and poison the receipt
/// instead of disappearing behind a valid-looking subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2AttemptKind {
    New,
    Clone,
    Set,
    Remove,
    Size,
    HasKey,
    Drop,
    /// `is_empty` has a dedicated narrow owner-v2 route and Checker action;
    /// it remains a distinct attempt so profiles without that exact receipt
    /// fail closed instead of treating the call as an unrelated scalar
    /// builtin or silently routing to legacy.
    IsEmpty,
    GetAny,
    /// Provisional direct `get_or_default<i32>` function-call attempt. This
    /// never grants authority by itself: a separate whole-program source
    /// receipt must bind the canonical stdlib target and helper body.
    GetOrDefaultI32,
    /// Provisional direct `get_or_default<String>` function-call attempt.
    /// This has no route authority without its own whole-program receipt.
    GetOrDefaultString,
    Keys,
    Values,
    /// The tuple-valued `to_list` API is deliberately distinct from the
    /// owned `values` snapshot admitted by this slice.
    ToList,
    FromList,
    MapLiteral,
    ScopeDrop,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2Attempt {
    pub(crate) kind: OrdinaryMapOwnedAnyV2AttemptKind,
    pub(crate) point: NodeId,
    pub(crate) arguments: Vec<NodeId>,
    pub(crate) receiver_local: Option<ResolvedLocalId>,
    pub(crate) result_local: Option<ResolvedLocalId>,
}

/// One exact source-level `Label.text == "..."` observation reached through a
/// checked GetAny pair and its DynamicAny-to-Label unpack. The tuple
/// projection/cast and the later Label field projection are deliberately
/// separate identities: only `field_projection` can authorize the matching
/// MIR `MoveProject`. That move settles the one-field Label owner by moving
/// its String out; it must not be followed by a second Drop of the Label.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2PersistentRootAliasFieldReadReceipt {
    pub(crate) operation: NodeId,
    pub(crate) root_local: ResolvedLocalId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    pub(crate) key_node: NodeId,
    pub(crate) key_type: ResolvedTypeId,
    pub(crate) pair_binding_point: NodeId,
    pub(crate) pair_binding: ResolvedLocalId,
    pub(crate) pair_result_type: ResolvedTypeId,
    pub(crate) pair_projection: NodeId,
    pub(crate) cast: NodeId,
    pub(crate) conversion_kind: crate::core::ir::CheckedConversionKind,
    pub(crate) conversion_from: ResolvedTypeId,
    pub(crate) conversion_to: ResolvedTypeId,
    pub(crate) unpack_binding_point: NodeId,
    pub(crate) unpack_binding: ResolvedLocalId,
    pub(crate) pair_settlement_point: NodeId,
    pub(crate) pair_settlement_place_index: usize,
    pub(crate) field_projection: NodeId,
    pub(crate) field: NodeId,
    pub(crate) field_name: String,
    pub(crate) field_type: ResolvedTypeId,
    pub(crate) equality: NodeId,
    pub(crate) equality_statement: NodeId,
    pub(crate) print_call: NodeId,
    pub(crate) equality_statement_order: usize,
    pub(crate) field_is_left_operand: bool,
    pub(crate) peer_literal: NodeId,
    pub(crate) peer_text: String,
    pub(crate) payload_move_point: NodeId,
}

/// Exact Checker node identities for the one admitted postcondition. MIR
/// consumers bind `contract_statement` to `MirContract.id` and independently
/// check the canonical `result == 0 : i32 -> bool` expression.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2PersistentRootAliasContractReceipt {
    pub(crate) contract_statement: NodeId,
    pub(crate) condition: NodeId,
    pub(crate) result_load: NodeId,
    pub(crate) result_local: ResolvedLocalId,
    pub(crate) zero_literal: NodeId,
}

/// Checker source-node commitment used only by the persistent-root alias /
/// replacement profile. It is populated by the typed CFG source collector
/// only when both direct field reads and the exact postcondition are present.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2PersistentRootAliasReplacementReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) field_reads: Vec<OrdinaryMapOwnedAnyV2PersistentRootAliasFieldReadReceipt>,
    pub(crate) ensures: OrdinaryMapOwnedAnyV2PersistentRootAliasContractReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2CfgReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    pub(crate) attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    /// Independent Checker L1 evidence for the narrow runtime String-payload
    /// ComputedKeyGet route: a direct-local dynamic String key selects an
    /// owned String through GetAny and DynamicAnyUnpack, followed by one
    /// observed-use and explicit result Drop. This does not widen the global
    /// ordinary-Map readiness predicate.
    pub(crate) computed_key_string_get_receipts:
        Vec<OrdinaryMapOwnedAnyV2ComputedKeyStringGetReceipt>,
    /// Independent static-key lineage evidence for direct `unpack_i32` and
    /// `unpack_string` calls over a directly-bound GetAny pair. This remains
    /// narrower than the general unpack receipts: every entry replays the
    /// exact persistent root chain and carries the newest matching Set, or a
    /// canonical-i32 miss at New/Remove.
    pub(crate) static_key_get_receipts: Vec<OrdinaryMapOwnedAnyV2StaticKeyGetReceipt>,
    /// Exact source-level settlement for each owned GetAny/Values result.
    /// This is disjoint from the Map-root actions, so a synthetic scope drop
    /// cannot discharge an aggregate result owner.
    pub(crate) result_receipts: Vec<OrdinaryMapOwnedAnyV2ResultReceipt>,
    /// Exact no-argument Values producer whose owned List<DynamicAny> result
    /// is returned once after every Map root and direct String source payload
    /// has been explicitly settled. This narrow source receipt does not make
    /// a generic result Transfer/Escape receipted.
    pub(crate) values_producer_return_receipt:
        Option<OrdinaryMapOwnedAnyV2ValuesProducerReturnReceipt>,
    /// Result-branch producer evidence for the closed Values producer
    /// profiles. This is independent from the legacy straight-line return
    /// receipt above: it binds every Result arm and every branch-local owner
    /// settlement in the producer CFG.
    pub(crate) values_result_producer_receipt:
        Option<OrdinaryMapOwnedAnyV2ValuesResultProducerReceipt>,
    /// Exact three-path proof for a nested fallible `map_new` then
    /// `map_set` chain. New-Err creates no root, Set-Err settles the input
    /// root, and Set-Ok settles both persistent roots plus the source payload.
    /// This is an additive Checker receipt and does not broaden ordinary
    /// Map/Any readiness by itself.
    pub(crate) map_set_drop_control_result_branch_receipt:
        Option<OrdinaryMapOwnedAnyV2MapSetDropControlResultBranchReceipt>,
    /// Exact New-Err/Set-Err/Set-Success owner paths for the local-i32
    /// Values snapshot route. This profile has its own payload and snapshot
    /// settlement contract and does not extend MapSetDropControl.
    pub(crate) values_local_index_i32_result_branch_receipt:
        Option<OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchReceipt>,
    /// Closed source/CFG evidence for the broad heterogeneous Values-unpack
    /// fixture used by the readiness boundary. This is diagnostic MIR
    /// materialization authority only; it is not an executable route profile.
    pub(crate) values_unpack_result_branch_receipt:
        Option<OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchReceipt>,
    /// Exact one-caller owner settlement for the narrow direct Values-producer
    /// call slice. This lives on the producer receipt because the Checker
    /// proves the producer and its unique caller together after all callable
    /// analyses have been assembled.
    pub(crate) values_producer_caller_receipt:
        Option<OrdinaryMapOwnedAnyV2ValuesProducerCallerReceipt>,
    /// Exact independent caller proof for a Result-branch Values producer.
    /// Its two length observations and scalar sum form a new profile; it does
    /// not reuse the straight-line Values caller or the indexed String caller.
    pub(crate) values_result_producer_caller_receipt:
        Option<OrdinaryMapOwnedAnyV2ValuesResultProducerCallerReceipt>,
    /// Exact cross-function String-unpack caller profile. This is kept
    /// separate from the V1 Values length/drop caller receipt and from
    /// same-function `values_unpack_receipts`: its producer snapshot owner
    /// crosses a call boundary before the caller extracts one String.
    pub(crate) values_crosscall_string_caller_receipt:
        Option<OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerReceipt>,
    /// Exact Map-root ownership transfer for the narrow no-argument producer
    /// and its unique direct caller. This carries the final root lineage
    /// across the call boundary independently from Values snapshot receipts.
    pub(crate) map_root_producer_return_receipt:
        Option<OrdinaryMapOwnedAnyV2MapRootProducerReturnReceipt>,
    /// Exact settlement of the root returned by the producer above. The
    /// caller binding is tied to the producer's logical root identity, then
    /// observed once with map_size and explicitly dropped.
    pub(crate) map_root_producer_caller_receipt:
        Option<OrdinaryMapOwnedAnyV2MapRootProducerCallerReceipt>,
    /// Exact whole-program Map-root producer bridge whose unique direct
    /// caller performs one checked String GetAny unpack, drops the returned
    /// Map root, settles the GetAny tuple, and explicitly drops the copied
    /// String. This remains an independently versioned narrow route.
    pub(crate) map_root_get_any_caller_receipt:
        Option<OrdinaryMapOwnedAnyV2MapRootGetAnyCallerReceipt>,
    /// One exact canonical `std::maps.get_or_default<i32>` callsite whose
    /// caller root/key/default and generic helper body have been joined by
    /// Checker replay. This independent profile does not widen CheckedGet
    /// or ordinary Map/Any production readiness.
    pub(crate) get_or_default_i32_receipt: Option<OrdinaryMapOwnedAnyV2GetOrDefaultI32Receipt>,
    /// One exact canonical `std::maps.get_or_default<String>` callsite whose
    /// caller's String Set/default and generic helper body have been joined by
    /// Checker replay. It remains independent from the i32 profile and broad
    /// ordinary-Map readiness.
    pub(crate) get_or_default_string_receipt:
        Option<OrdinaryMapOwnedAnyV2GetOrDefaultStringReceipt>,
    /// Exact Checker evidence that a directly bound GetAny tuple's field 0
    /// was copied into an immutable Boolean local and that every read of the
    /// local belongs to the callable's unique Boolean result expression.
    /// This does not certify the Boolean formula itself.
    pub(crate) found_observation_receipts: Vec<OrdinaryMapOwnedAnyV2FoundObservationReceipt>,
    /// Exact source evidence for the one supported DynamicAny unpack shape:
    /// the field-1 projection of a directly-bound GetAny tuple, unpacked by
    /// a checked conversion while the original tuple remains explicitly
    /// settled as a whole.
    pub(crate) unpack_receipts: Vec<OrdinaryMapOwnedAnyV2UnpackReceipt>,
    /// Exact source evidence for direct indexed casts from a directly-bound
    /// Values snapshot. This remains separate from GetAny's tuple receipt:
    /// each indexed cast observes the List<DynamicAny>, while the snapshot
    /// itself is settled once by its exact explicit Drop.
    pub(crate) values_unpack_receipts: Vec<OrdinaryMapOwnedAnyV2ValuesUnpackReceipt>,
    /// Exact Checker evidence for the independently admitted immutable
    /// `let index = 0; snapshot[index] as i32` route. The generic
    /// `index_local` field above intentionally does not authorize this shape.
    pub(crate) values_local_index_i32_receipts:
        Vec<OrdinaryMapOwnedAnyV2ValuesLocalIndexI32Receipt>,
    /// Exact Checker L1 source evidence for an owned keys snapshot, one
    /// literal index-zero String copy, its observation and owner settlement.
    /// This receipt is intentionally independent of the DynamicAny Values
    /// unpack contract.
    pub(crate) keys_snapshot_receipts: Vec<OrdinaryMapOwnedAnyV2KeysSnapshotReceipt>,
    /// Exact source-level runtime-input, dynamic-key, Keys snapshot and
    /// same-key GetAny profile. This does not widen KeysSnapshot or broad
    /// owner-v2 readiness.
    pub(crate) dynamic_keys_get_receipts: Vec<OrdinaryMapOwnedAnyV2DynamicKeysGetReceipt>,
    /// Narrow proof that two distinct statically keyed Label payloads are
    /// selected by a Values snapshot, then survive a later same-key
    /// persistent replacement and explicit settlement of every Map root.
    /// This is an additive Checker receipt; it does not widen the existing
    /// Values unpack profile by itself.
    pub(crate) values_snapshot_replacement_receipts:
        Vec<OrdinaryMapOwnedAnyV2ValuesSnapshotReplacementReceipt>,
    /// Exact heterogeneous Values snapshot with one Copy, one String, and
    /// one canonical Label payload. This is separate from the all-Label
    /// replacement profile and does not widen owner-v2 readiness.
    pub(crate) values_mixed_snapshot_receipts: Vec<OrdinaryMapOwnedAnyV2ValuesMixedSnapshotReceipt>,
    /// Exact heterogeneous Values snapshot followed by one persistent
    /// removal of `b-text` from its source root. The nested snapshot is
    /// evidence only for this additive candidate and does not make the
    /// strict ValuesMixedSnapshot profile admit a Remove action.
    pub(crate) values_mixed_snapshot_remove_receipts:
        Vec<OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemoveReceipt>,
    /// Compiler-owned root identities for the one admitted temporary shape:
    /// `map_get(map_new(), string_key)`. The root is settled by a dedicated
    /// Drop immediately after GetAny, never by scope cleanup.
    pub(crate) temporary_root_receipts: Vec<OrdinaryMapOwnedAnyV2TemporaryRootReceipt>,
    /// Independent exact source authority for MapEmptyRootRootQueriesV1.
    pub(crate) empty_root_root_queries_v1: Option<MapEmptyRootRootQueriesV1CheckerReceipt>,
    /// Exact Checker evidence for the narrow ZeroMissI32 short-circuit tail.
    /// It binds the two actual GetAny `pair.0` observations to the three
    /// source CFG branch selectors after every Map/Any owner has been settled.
    pub(crate) zero_miss_i32_observation_receipt:
        Option<OrdinaryMapOwnedAnyV2ZeroMissI32ObservationReceipt>,
    /// Exact Checker proof for the single-key String-payload Set -> persistent
    /// Remove -> same-key GetAny miss slice. This is deliberately distinct
    /// from the broad CheckedGet and ZeroMissI32 observations.
    pub(crate) removed_key_checked_miss_receipt:
        Option<OrdinaryMapOwnedAnyV2RemovedKeyCheckedMissReceipt>,
    /// Exact Checker source nodes for two typed Label reads from distinct
    /// GetAny roots plus the single `ensures result == 0` contract. MIR-only
    /// consumers can bind their MoveProject and contract IDs without
    /// rescanning surface or Resolved AST.
    pub(crate) persistent_root_alias_replacement_receipt:
        Option<OrdinaryMapOwnedAnyV2PersistentRootAliasReplacementReceipt>,
    /// A V2-owned source-CFG and live-owner proof for either the carried-owner
    /// two-arm diamond or the settled-owner control-flow tail. It is
    /// intentionally independent of the V1 Map receipt: every live root and
    /// GetAny/Values result owner is recorded on every edge and replayed by
    /// MIR validation. In the settled-owner form, `arm_edges` names every
    /// branch edge in the acyclic tail, and every owner set is empty.
    pub(crate) flow: Option<OrdinaryMapOwnedAnyV2CfgFlowReceipt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2MapSetDropControlResultBranchProfileVersion {
    V1,
}

impl OrdinaryMapOwnedAnyV2MapSetDropControlResultBranchProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "ordinary-map-owned-any-v2-map-set-drop-control-result-branch-v1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2MapSetDropControlResultPathKind {
    MapNewErr,
    MapSetErr,
    Success,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2MapSetDropControlBranchDrop {
    pub(crate) local: ResolvedLocalId,
    pub(crate) binding_point: NodeId,
    pub(crate) binding_type: ResolvedTypeId,
    pub(crate) settlement_point: NodeId,
    pub(crate) settlement_place_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2MapSetDropControlResultPath {
    pub(crate) arm_path: Vec<NodeId>,
    pub(crate) result: NodeId,
    pub(crate) kind: OrdinaryMapOwnedAnyV2MapSetDropControlResultPathKind,
    /// Only Map-root Drops are listed here. Source payload cleanup is a
    /// separate owner obligation bound to this exact path.
    pub(crate) drops: Vec<OrdinaryMapOwnedAnyV2MapSetDropControlBranchDrop>,
    pub(crate) payload_drop: OrdinaryMapOwnedAnyV2MapSetDropControlBranchDrop,
}

/// Checker-side proof for the exact three-way Result control flow exercised
/// by `map_set_drop_control`. It commits the status bindings, payload Drop,
/// root path settlements, and the complete reachable CFG snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2MapSetDropControlResultBranchReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2MapSetDropControlResultBranchProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) map_new_operation: NodeId,
    pub(crate) map_new_match: NodeId,
    pub(crate) map_new_ok_arm: NodeId,
    pub(crate) map_new_err_arm: NodeId,
    pub(crate) map_new_error_status: ResolvedLocalId,
    pub(crate) initial_root: ResolvedLocalId,
    pub(crate) map_set_operation: NodeId,
    pub(crate) map_set_match: NodeId,
    pub(crate) map_set_ok_arm: NodeId,
    pub(crate) map_set_err_arm: NodeId,
    pub(crate) map_set_error_status: ResolvedLocalId,
    pub(crate) populated_root: ResolvedLocalId,
    pub(crate) key: NodeId,
    pub(crate) payload: ResolvedLocalId,
    pub(crate) payload_drop: NodeId,
    pub(crate) paths: Vec<OrdinaryMapOwnedAnyV2MapSetDropControlResultPath>,
    pub(crate) ordered_action_points: Vec<NodeId>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

/// Version for the exact fallible New/Set path used by the local-i32 Values
/// profile. This is independent of MapSetDropControl: its payload and success
/// branch have a different owner graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchProfileVersion {
    V2,
}

impl OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V2 => "ordinary-map-owned-any-v2-values-local-index-i32-result-branch-v2",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultPathKind {
    MapNewErr,
    MapSetErr,
    Success,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesLocalIndexI32BranchDropOwner {
    MapRoot,
    ValuesSnapshot,
}

/// One exact path-local owner settlement. The same Map root can occur in
/// mutually exclusive Set-Err and Success paths, so each Drop remains tied
/// to its arm path instead of being flattened into a callable-wide count.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesLocalIndexI32BranchDrop {
    pub(crate) owner: OrdinaryMapOwnedAnyV2ValuesLocalIndexI32BranchDropOwner,
    pub(crate) local: ResolvedLocalId,
    pub(crate) binding_point: NodeId,
    pub(crate) binding_type: ResolvedTypeId,
    pub(crate) settlement_point: NodeId,
    pub(crate) settlement_place_index: usize,
}

/// One terminal branch of the exact nested New/Set Result graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultPath {
    pub(crate) arm_path: Vec<NodeId>,
    pub(crate) result: NodeId,
    pub(crate) kind: OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultPathKind,
    pub(crate) drops: Vec<OrdinaryMapOwnedAnyV2ValuesLocalIndexI32BranchDrop>,
}

/// Checker proof for the closed fallible Map New/Set plus Values local-index
/// route. It binds the persistent root edge, exact key and i32 Set payload,
/// Values snapshot and zero-index unpack, the three independent Result paths,
/// every explicit owner settlement, and the complete reachable source CFG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranchProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) map_new_operation: NodeId,
    pub(crate) map_new_match: NodeId,
    pub(crate) map_new_ok_arm: NodeId,
    pub(crate) map_new_err_arm: NodeId,
    pub(crate) map_new_ok_pattern: NodeId,
    pub(crate) map_new_err_pattern: NodeId,
    pub(crate) map_new_ok_body: NodeId,
    pub(crate) map_new_err_body: NodeId,
    pub(crate) map_new_error_status: ResolvedLocalId,
    pub(crate) initial_root: ResolvedLocalId,
    pub(crate) map_set_operation: NodeId,
    pub(crate) map_set_match: NodeId,
    pub(crate) map_set_ok_arm: NodeId,
    pub(crate) map_set_err_arm: NodeId,
    pub(crate) map_set_ok_pattern: NodeId,
    pub(crate) map_set_err_pattern: NodeId,
    pub(crate) map_set_ok_body: NodeId,
    pub(crate) map_set_err_body: NodeId,
    pub(crate) map_set_error_status: ResolvedLocalId,
    pub(crate) populated_root: ResolvedLocalId,
    pub(crate) key_node: NodeId,
    pub(crate) key_literal: String,
    pub(crate) payload_node: NodeId,
    pub(crate) payload_type: ResolvedTypeId,
    pub(crate) payload_literal: i64,
    pub(crate) values_operation: NodeId,
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) snapshot_local: ResolvedLocalId,
    pub(crate) snapshot_type: ResolvedTypeId,
    pub(crate) index_binding_point: NodeId,
    pub(crate) index_local: ResolvedLocalId,
    pub(crate) index_initializer: NodeId,
    pub(crate) index_expression: NodeId,
    pub(crate) snapshot_index_node: NodeId,
    pub(crate) unpack_binding_point: NodeId,
    pub(crate) unpack_local: ResolvedLocalId,
    pub(crate) unpack_cast: NodeId,
    pub(crate) snapshot_drop: NodeId,
    pub(crate) result: NodeId,
    pub(crate) paths: Vec<OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultPath>,
    pub(crate) ordered_action_points: Vec<NodeId>,
    pub(crate) root_loads: Vec<(NodeId, ResolvedLocalId)>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

/// Version tag for a diagnostic-only, exact four-unpack Values Result chain.
/// This receipt lets Checker produce read-only canonical MIR for the broad
/// Values candidate while keeping run/build/verify behind readiness gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchProfileVersion {
    V1,
}

impl OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "ordinary-map-owned-any-v2-values-unpack-result-branch-inspection-v1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchSet {
    pub(crate) operation: NodeId,
    pub(crate) match_node: NodeId,
    pub(crate) ok_arm: NodeId,
    pub(crate) err_arm: NodeId,
    pub(crate) ok_pattern: NodeId,
    pub(crate) err_pattern: NodeId,
    pub(crate) input_root: ResolvedLocalId,
    pub(crate) output_root: ResolvedLocalId,
    pub(crate) error_status: ResolvedLocalId,
    pub(crate) key_node: NodeId,
    pub(crate) key_literal: String,
    pub(crate) payload_node: NodeId,
    pub(crate) payload_type: ResolvedTypeId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchDrop {
    pub(crate) local: ResolvedLocalId,
    pub(crate) binding_point: NodeId,
    pub(crate) binding_type: ResolvedTypeId,
    pub(crate) settlement_point: NodeId,
    pub(crate) settlement_place_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchPathKind {
    MapNewErr,
    MapSetErr0,
    MapSetErr1,
    MapSetErr2,
    Success,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchPath {
    pub(crate) arm_path: Vec<NodeId>,
    pub(crate) result: NodeId,
    pub(crate) kind: OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchPathKind,
    pub(crate) drops: Vec<OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchDrop>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) map_new_operation: NodeId,
    pub(crate) map_new_match: NodeId,
    pub(crate) map_new_ok_arm: NodeId,
    pub(crate) map_new_err_arm: NodeId,
    pub(crate) initial_root: ResolvedLocalId,
    pub(crate) map_new_error_status: ResolvedLocalId,
    pub(crate) sets: Vec<OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchSet>,
    pub(crate) values_operation: NodeId,
    pub(crate) values_root: ResolvedLocalId,
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) snapshot_local: ResolvedLocalId,
    pub(crate) snapshot_type: ResolvedTypeId,
    pub(crate) index_binding_point: NodeId,
    pub(crate) index_local: ResolvedLocalId,
    pub(crate) index_initializer: NodeId,
    pub(crate) index_expression: NodeId,
    pub(crate) unpack_casts: Vec<NodeId>,
    pub(crate) label_binding_point: NodeId,
    pub(crate) label_local: ResolvedLocalId,
    pub(crate) label_type: ResolvedTypeId,
    pub(crate) label_unpack_local: ResolvedLocalId,
    pub(crate) label_unpack_binding_point: NodeId,
    pub(crate) label_unpack_drop: NodeId,
    pub(crate) snapshot_drop: NodeId,
    pub(crate) result: NodeId,
    pub(crate) paths: Vec<OrdinaryMapOwnedAnyV2ValuesUnpackResultBranchPath>,
    pub(crate) ordered_action_points: Vec<NodeId>,
    pub(crate) root_loads: Vec<(NodeId, ResolvedLocalId)>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainProfileVersion {
    V1,
}

impl OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "ordinary-map-owned-any-v2-computed-string-key-owner-chain-v1",
        }
    }
}

/// Exact source binding identity used by a static String-key DAG. The
/// initializer and type are retained with the immutable local ID so byte
/// equality can be recomputed from Checker-owned typed source evidence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2StringKeySourceBinding {
    pub(crate) local: ResolvedLocalId,
    pub(crate) binding_point: NodeId,
    pub(crate) initializer: NodeId,
    pub(crate) ty: ResolvedTypeId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2StringKeyValueNodeKind {
    Literal {
        bytes: Vec<u8>,
    },
    Constant {
        constant: NodeId,
        bytes: Vec<u8>,
    },
    ImmutableLocal {
        binding: OrdinaryMapOwnedAnyV2StringKeySourceBinding,
    },
    Add {
        left: NodeId,
        right: NodeId,
    },
}

/// One node in the recursively re-evaluable String expression DAG. Every
/// node keeps its exact checked type and computed bytes; `digest` is only an
/// index/checksum and never substitutes for comparing `bytes`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2StringKeyValueNode {
    pub(crate) node: NodeId,
    pub(crate) ty: ResolvedTypeId,
    pub(crate) kind: OrdinaryMapOwnedAnyV2StringKeyValueNodeKind,
    pub(crate) bytes: Vec<u8>,
    pub(crate) digest: String,
}

/// Recomputed static bytes and the complete typed expression DAG for a key
/// use. The DAG includes immutable local binding identities and initializer
/// roots; a digest alone is deliberately insufficient to establish equality.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2StringKeyValueProof {
    pub(crate) expression: NodeId,
    pub(crate) expression_type: ResolvedTypeId,
    pub(crate) bytes: Vec<u8>,
    pub(crate) digest: String,
    pub(crate) source_bindings: Vec<OrdinaryMapOwnedAnyV2StringKeySourceBinding>,
    pub(crate) nodes: Vec<OrdinaryMapOwnedAnyV2StringKeyValueNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2StringKeyIdentity {
    DirectImmutableLocal {
        local: ResolvedLocalId,
        binding_point: NodeId,
        initializer: NodeId,
    },
    ComputedExpression,
}

/// Complete per-operation Checker source evidence for a String-key use.
/// `action` preserves the full Action (point/root/source/key node/type/local,
/// provenance, and static digest); the remaining fields make operand position
/// and immutable source identity explicit to a downstream exact MIR validator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ComputedStringKeyUseReceipt {
    pub(crate) action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) action_kind: OrdinaryMapOwnedAnyV2ActionKind,
    pub(crate) map_source_local: ResolvedLocalId,
    pub(crate) key_node: NodeId,
    pub(crate) key_type: ResolvedTypeId,
    pub(crate) key_identity: OrdinaryMapOwnedAnyV2StringKeyIdentity,
    pub(crate) value_proof: OrdinaryMapOwnedAnyV2StringKeyValueProof,
    pub(crate) expression_cfg_index: usize,
    pub(crate) consumer_cfg_index: usize,
    pub(crate) key_argument_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2OwnerDropReceipt {
    pub(crate) local: ResolvedLocalId,
    pub(crate) binding_point: NodeId,
    pub(crate) binding_type: ResolvedTypeId,
    pub(crate) settlement_point: NodeId,
    pub(crate) settlement_place_index: usize,
    pub(crate) source_uses: BTreeSet<NodeId>,
}

/// The payload remains an independent user owner while Set clones its value
/// into the persistent Map. This receipt ties that clone to the Checker
/// record construction and the unique explicit source Drop.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ComputedStringPayloadReceipt {
    pub(crate) source: OrdinaryMapOwnedAnyV2OwnerDropReceipt,
    pub(crate) construction: NodeId,
    pub(crate) set_value: NodeId,
    pub(crate) set_value_type: ResolvedTypeId,
    pub(crate) set_action: OrdinaryMapOwnedAnyV2Action,
}

/// Exact source readout of a GetAny `(bool, DynamicAny)` pair after every
/// source Map root, key binding, and the original Set payload have been
/// explicitly settled. The first tuple field is observed as the hit flag;
/// the second is checked-unpacked to the Set payload's concrete record,
/// whose string field is compared with the expected bytes and whose owned
/// source local is explicitly dropped. L2 must bind each node to its MIR
/// projection, unpack, and owner action; L3 verifies runtime survival.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2PairSnapshotObservationReceipt {
    pub(crate) found_tuple_index: usize,
    pub(crate) found_projection: NodeId,
    pub(crate) found_base_load: NodeId,
    pub(crate) found_binding_point: NodeId,
    pub(crate) found_local: ResolvedLocalId,
    pub(crate) found_type: ResolvedTypeId,
    pub(crate) snapshot_tuple_index: usize,
    pub(crate) snapshot_projection: NodeId,
    pub(crate) snapshot_base_load: NodeId,
    pub(crate) snapshot_unpack: NodeId,
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) snapshot_local: ResolvedLocalId,
    pub(crate) snapshot_source_type: ResolvedTypeId,
    pub(crate) snapshot_type: ResolvedTypeId,
    pub(crate) snapshot_text_field: NodeId,
    pub(crate) snapshot_text_projection: NodeId,
    pub(crate) snapshot_text_base_load: NodeId,
    pub(crate) snapshot_text_type: ResolvedTypeId,
    pub(crate) snapshot_drop: OrdinaryMapOwnedAnyV2OwnerDropReceipt,
    pub(crate) observation_binding_point: NodeId,
    pub(crate) observation_local: ResolvedLocalId,
    pub(crate) observation_expression: NodeId,
    pub(crate) observation_equal: NodeId,
    pub(crate) observation_expected_literal: NodeId,
    pub(crate) observation_found_load: NodeId,
    pub(crate) observation_print_call: NodeId,
    pub(crate) observation_print_argument: NodeId,
}

/// Source-only L1 proof for a reusable ordinary owner-v2 String-key chain.
/// It proves New -> Set -> HasKey/GetAny -> Remove/Size lineage, exact static
/// key-byte equality, Map-root settlement, pair field observations after all
/// owner settlements, and payload settlement. L3 separately proves the
/// selected snapshot survives those runtime frees.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) map_new: OrdinaryMapOwnedAnyV2Action,
    pub(crate) map_set: OrdinaryMapOwnedAnyV2Action,
    pub(crate) has_key_actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    pub(crate) get_any: OrdinaryMapOwnedAnyV2Action,
    pub(crate) map_remove: OrdinaryMapOwnedAnyV2Action,
    pub(crate) map_size: OrdinaryMapOwnedAnyV2Action,
    pub(crate) key_uses: Vec<OrdinaryMapOwnedAnyV2ComputedStringKeyUseReceipt>,
    /// Explicit source Drops for immutable local bindings referenced by the
    /// key DAGs (including intermediate computed locals such as `key`).
    /// Compiler-generated expression temporaries have no linear-ledger Drop
    /// here and are intentionally left to exact L2 MIR ownership validation.
    pub(crate) key_source_drops: Vec<OrdinaryMapOwnedAnyV2OwnerDropReceipt>,
    pub(crate) map_root_drops: Vec<OrdinaryMapOwnedAnyV2OwnerDropReceipt>,
    pub(crate) pair: OrdinaryMapOwnedAnyV2ResultReceipt,
    pub(crate) pair_drop: OrdinaryMapOwnedAnyV2OwnerDropReceipt,
    pub(crate) pair_snapshot_observation: OrdinaryMapOwnedAnyV2PairSnapshotObservationReceipt,
    pub(crate) payload: OrdinaryMapOwnedAnyV2ComputedStringPayloadReceipt,
    pub(crate) ordered_action_points: Vec<NodeId>,
    pub(crate) result_branch:
        Option<Box<OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchReceipt>>,
}

/// Separately versioned path authority for the dynamic String-key owner-chain
/// shape whose `map_new` and `map_set` results are consumed by explicit
/// `Result` matches. The straight-line chain receipt above remains unchanged;
/// this evidence records each mutually exclusive terminal path and the root
/// Drops that settle that path's live owners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchProfileVersion {
    V1,
}

#[cfg(test)]
impl OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => {
                "ordinary-map-owned-any-v2-computed-string-key-owner-chain-result-branch-v1"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultPathKind {
    MapNewErr,
    MapSetErr,
    Success,
}

/// One exact branch-local settlement, retaining the binding and Drop source
/// identities so MIR can prove that the event belongs to this path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainBranchDrop {
    pub(crate) local: ResolvedLocalId,
    pub(crate) binding_point: NodeId,
    pub(crate) binding_type: ResolvedTypeId,
    pub(crate) settlement_point: NodeId,
    pub(crate) settlement_place_index: usize,
}

/// One terminal Result path in the branch-aware owner-chain receipt. The
/// ordered arm IDs identify the outer `map_new` and optional nested
/// `map_set` constructor arms; drops are path-local events, so the same root
/// may be settled at different source points on mutually exclusive paths.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultPath {
    pub(crate) arm_path: Vec<NodeId>,
    pub(crate) result: NodeId,
    pub(crate) kind: OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultPathKind,
    pub(crate) drops: Vec<OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainBranchDrop>,
}

/// Strict Checker L1 proof for the explicit-Result branch form of the
/// computed String-key owner chain. `cfg` snapshots every reachable block
/// and edge; `paths` partition those edges into New-Err, Set-Err, and Set-Ok
/// terminals with equal root ownership at the relevant joins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchReceipt {
    pub(crate) profile_version:
        OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) map_new_operation: NodeId,
    pub(crate) map_new_match: NodeId,
    pub(crate) map_new_ok_arm: NodeId,
    pub(crate) map_new_err_arm: NodeId,
    pub(crate) initial_root: ResolvedLocalId,
    pub(crate) map_set_operation: NodeId,
    pub(crate) map_set_match: NodeId,
    pub(crate) map_set_ok_arm: NodeId,
    pub(crate) map_set_err_arm: NodeId,
    pub(crate) populated_root: ResolvedLocalId,
    pub(crate) paths: Vec<OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultPath>,
    pub(crate) ordered_action_points: Vec<NodeId>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

impl PartialOrd for OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchReceipt {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchReceipt {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.profile_version
            .cmp(&other.profile_version)
            .then_with(|| self.owner.cmp(&other.owner))
            .then_with(|| self.map_new_operation.cmp(&other.map_new_operation))
            .then_with(|| self.map_new_match.cmp(&other.map_new_match))
            .then_with(|| self.map_new_ok_arm.cmp(&other.map_new_ok_arm))
            .then_with(|| self.map_new_err_arm.cmp(&other.map_new_err_arm))
            .then_with(|| self.initial_root.cmp(&other.initial_root))
            .then_with(|| self.map_set_operation.cmp(&other.map_set_operation))
            .then_with(|| self.map_set_match.cmp(&other.map_set_match))
            .then_with(|| self.map_set_ok_arm.cmp(&other.map_set_ok_arm))
            .then_with(|| self.map_set_err_arm.cmp(&other.map_set_err_arm))
            .then_with(|| self.populated_root.cmp(&other.populated_root))
            .then_with(|| self.paths.cmp(&other.paths))
            .then_with(|| self.ordered_action_points.cmp(&other.ordered_action_points))
            // The CFG snapshot contains resolved IR enums without an Ord
            // contract. Its derived Debug form includes all fields and has
            // deterministic BTreeMap ordering, so it supplies the final
            // structural tie-breaker without weakening Eq.
            .then_with(|| format!("{:?}", self.cfg).cmp(&format!("{:?}", other.cfg)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2KeysSnapshotProfileVersion {
    V1,
}

/// Checker L1 proof that `keys(root)` creates a fresh owned `List<String>`
/// whose first element remains independently usable after both the source Map
/// and the list snapshot have been dropped. The one admitted source key is a
/// statically known literal so the selected Map lineage has a unique source
/// value; runtime ordering and allocation are verified by downstream gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2KeysSnapshotReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2KeysSnapshotProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) keys_operation: NodeId,
    pub(crate) source_local: ResolvedLocalId,
    pub(crate) source: ResourceId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) snapshot_local: ResolvedLocalId,
    pub(crate) snapshot_type: ResolvedTypeId,
    pub(crate) snapshot_drop_point: NodeId,
    pub(crate) snapshot_drop_place_index: usize,
    pub(crate) index_binding_point: NodeId,
    pub(crate) index_projection: NodeId,
    pub(crate) index_expression: NodeId,
    pub(crate) index_type: ResolvedTypeId,
    pub(crate) key_local: ResolvedLocalId,
    pub(crate) key_type: ResolvedTypeId,
    pub(crate) observation_call: NodeId,
    pub(crate) observation_load: NodeId,
    pub(crate) key_drop_point: NodeId,
    pub(crate) key_drop_place_index: usize,
    pub(crate) root_drop_point: NodeId,
    pub(crate) root_drop_place_index: usize,
    pub(crate) static_set_operation: NodeId,
    pub(crate) static_key_node: NodeId,
    pub(crate) static_key_utf8: Vec<u8>,
    pub(crate) static_key_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2DynamicKeysGetProfileVersion {
    V1,
}

/// Checker L1 evidence for one source-owned runtime argument key route. The
/// exact `args()[0]` projection is materialized as a dedicated MIR node; its
/// owned String result is concatenated with the source literal `"\0雪"`
/// before that one immutable key local is used for the miss GetAny, Set,
/// Keys snapshot, and hit GetAny. This receipt is independent from the
/// literal-only KeysSnapshot and the ComputedKeyGet source profiles.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2DynamicKeysGetReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2DynamicKeysGetProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) args_projection: NodeId,
    pub(crate) args_call: NodeId,
    pub(crate) args_index: NodeId,
    pub(crate) prefix_binding_point: NodeId,
    pub(crate) prefix_local: ResolvedLocalId,
    pub(crate) prefix_drop_point: NodeId,
    pub(crate) suffix_binding_point: NodeId,
    pub(crate) suffix_local: ResolvedLocalId,
    pub(crate) suffix_literal_node: NodeId,
    pub(crate) suffix_drop_point: NodeId,
    pub(crate) key_binding_point: NodeId,
    pub(crate) key_local: ResolvedLocalId,
    pub(crate) key_concat: NodeId,
    pub(crate) key_concat_prefix: NodeId,
    pub(crate) key_concat_suffix: NodeId,
    pub(crate) key_drop_point: NodeId,
    pub(crate) key_suffix_utf8: Vec<u8>,
    pub(crate) new_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) set_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) miss_get_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) keys_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) hit_get_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) root_drops: [OrdinaryMapOwnedAnyV2Action; 2],
    pub(crate) miss_result: OrdinaryMapOwnedAnyV2ResultReceipt,
    pub(crate) miss_found: OrdinaryMapOwnedAnyV2FoundObservationReceipt,
    pub(crate) miss_unpack: OrdinaryMapOwnedAnyV2UnpackReceipt,
    pub(crate) hit_result: OrdinaryMapOwnedAnyV2ResultReceipt,
    pub(crate) hit_found: OrdinaryMapOwnedAnyV2FoundObservationReceipt,
    pub(crate) hit_unpack: OrdinaryMapOwnedAnyV2UnpackReceipt,
    pub(crate) miss_pair_drop_point: NodeId,
    pub(crate) hit_pair_drop_point: NodeId,
    pub(crate) outcome_local: ResolvedLocalId,
    pub(crate) outcome_observation_call: NodeId,
    pub(crate) outcome_observation_load: NodeId,
    pub(crate) return_load: NodeId,
    /// Four ordered Boolean outcomes consumed by the source expression. The
    /// checker receipt records each root and its exact operand/literal nodes,
    /// so MIR replay can bind the comparisons instead of trusting counts.
    pub(crate) outcome_terms: [OrdinaryMapOwnedAnyV2DynamicKeysGetOutcomeTerm; 4],
    /// Short-circuit conjunction nodes in inner-to-outer evaluation order.
    /// The profile is deliberately left-associated: (((a && b) && c) && d).
    pub(crate) outcome_and_nodes: [OrdinaryMapOwnedAnyV2DynamicKeysGetAndNode; 3],
    pub(crate) keys_snapshot: OrdinaryMapOwnedAnyV2KeysSnapshotOwnerReceipt,
    /// Complete ordered source statement identity census. MIR replay checks
    /// this against the one-block expression/action/drop identity census;
    /// keeping statement IDs separate makes extra or repeated AST identities
    /// observable even though `source_identity_nodes` is a set.
    pub(crate) source_statement_nodes: Vec<NodeId>,
    /// Every allowed observation and source-identity node. MIR replay uses
    /// this complete census to reject unrelated operations that happen to
    /// preserve the same action counts.
    pub(crate) source_identity_nodes: BTreeSet<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2DynamicKeysGetOutcomeTerm {
    pub(crate) expression: NodeId,
    /// The direct local load, either the whole term (`hit_found`) or the
    /// operand below `!`/`== 0`.
    pub(crate) operand: NodeId,
    pub(crate) local: ResolvedLocalId,
    /// Present only for the exact integer-zero comparison terms.
    pub(crate) zero_literal: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2DynamicKeysGetAndNode {
    pub(crate) expression: NodeId,
    pub(crate) left: NodeId,
    pub(crate) right: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2KeysSnapshotOwnerReceipt {
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) snapshot_local: ResolvedLocalId,
    pub(crate) snapshot_type: ResolvedTypeId,
    pub(crate) snapshot_drop_point: NodeId,
    pub(crate) copied_key_projection: NodeId,
    pub(crate) snapshot_source_load: NodeId,
    pub(crate) copied_key_index: NodeId,
    pub(crate) copied_key_binding_point: NodeId,
    pub(crate) copied_key_local: ResolvedLocalId,
    pub(crate) copied_key_drop_point: NodeId,
    pub(crate) copied_key_type: ResolvedTypeId,
    pub(crate) observation_call: NodeId,
    pub(crate) observation_load: NodeId,
}

/// Checker-only whole-callable proof. No public route label can construct it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MapEmptyRootRootQueriesV1CheckerReceipt {
    pub(crate) owner: NodeId,
    pub(crate) bindings: [MapEmptyRootRootQueriesV1Binding; 4],
    pub(crate) drops: [NodeId; 4],
    pub(crate) key: NodeId,
    pub(crate) key_text: String,
    pub(crate) return_value: NodeId,
    pub(crate) actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    pub(crate) attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MapEmptyRootRootQueriesV1Binding {
    pub(crate) local: ResolvedLocalId,
    pub(crate) statement: NodeId,
    pub(crate) operation: NodeId,
    pub(crate) ty: ResolvedTypeId,
    pub(crate) arguments: Vec<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ComputedKeyStringGetProfileVersion {
    V1,
}

/// A source-owned runtime String local, with its exact two runtime String
/// inputs, complete value-use census, and one explicit whole-value Drop.
/// The source local is used for either the immutable computed key or the
/// String value copied into the persistent Map by Set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2RuntimeStringLocalReceipt {
    pub(crate) local: ResolvedLocalId,
    pub(crate) binding_point: NodeId,
    pub(crate) initializer: NodeId,
    pub(crate) runtime_inputs: [ResolvedLocalId; 2],
    pub(crate) input_loads: [NodeId; 2],
    pub(crate) source_uses: BTreeSet<NodeId>,
    pub(crate) settlement_point: NodeId,
    pub(crate) settlement_place_index: usize,
}

/// Exact observable borrow of the unpacked owned String, followed by its
/// unique explicit release. The complete use set excludes aliases, escapes,
/// extra reads, and hidden closure captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2StringObservationReceipt {
    pub(crate) local: ResolvedLocalId,
    pub(crate) observation_binding_point: NodeId,
    pub(crate) observation_binding: ResolvedLocalId,
    pub(crate) observation: NodeId,
    pub(crate) observation_load: NodeId,
    pub(crate) observation_is_not_equal: bool,
    pub(crate) peer_literal: String,
    pub(crate) peer_literal_node: NodeId,
    /// Exact direct result load which exposes the equality result to the
    /// caller, closing the observation path after the owned String is dropped.
    pub(crate) observation_result_load: NodeId,
    pub(crate) source_uses: BTreeSet<NodeId>,
    pub(crate) settlement_point: NodeId,
    pub(crate) settlement_place_index: usize,
}

/// Checker L1 proof for the narrow runtime-String-payload ComputedKeyGet
/// slice. It binds persistent root lineage and both root settlements to the
/// exact Set/GetAny/Unpack source operations; the GetAny pair and the owned
/// unpacked String have independent, complete use and Drop receipts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ComputedKeyStringGetReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ComputedKeyStringGetProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) map_new: OrdinaryMapOwnedAnyV2Action,
    pub(crate) map_set: OrdinaryMapOwnedAnyV2Action,
    pub(crate) get_any: OrdinaryMapOwnedAnyV2Action,
    /// Ordered by source root lineage: the `map_new` root, then the Set result.
    pub(crate) map_root_drops: [OrdinaryMapOwnedAnyV2Action; 2],
    /// One-place source Drop indices for the two roots, in the same order.
    pub(crate) map_root_drop_place_indices: [usize; 2],
    pub(crate) key: OrdinaryMapOwnedAnyV2RuntimeStringLocalReceipt,
    pub(crate) payload: OrdinaryMapOwnedAnyV2RuntimeStringLocalReceipt,
    pub(crate) result: OrdinaryMapOwnedAnyV2ResultReceipt,
    pub(crate) unpack: OrdinaryMapOwnedAnyV2UnpackReceipt,
    pub(crate) pair_source_uses: BTreeSet<NodeId>,
    pub(crate) pair_settlement_point: NodeId,
    pub(crate) pair_settlement_place_index: usize,
    pub(crate) observed_string: OrdinaryMapOwnedAnyV2StringObservationReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2LiveOwner {
    Root {
        local: ResolvedLocalId,
        resource: ResourceId,
    },
    Result {
        operation: NodeId,
        kind: OrdinaryMapOwnedAnyV2ActionKind,
        binding: ResolvedLocalId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2CfgBlock {
    pub(crate) source: crate::core::cfg::CfgSource,
    pub(crate) points: Vec<(NodeId, crate::core::cfg::CfgPointKind)>,
    pub(crate) terminator: crate::core::cfg::Terminator,
}

/// One exact source-CFG short-circuit selector. The complete source block and
/// edge snapshots remain in `OrdinaryMapOwnedAnyV2CfgFlowReceipt`; these
/// references identify the three selectors authorized by ZeroMissI32.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2BranchSelectorReceipt {
    pub(crate) block: crate::core::cfg::BasicBlockId,
    pub(crate) condition: NodeId,
    pub(crate) then_edge: crate::core::cfg::EdgeId,
    pub(crate) then_target: crate::core::cfg::BasicBlockId,
    pub(crate) else_edge: crate::core::cfg::EdgeId,
    pub(crate) else_target: crate::core::cfg::BasicBlockId,
}

/// Checker-only ZeroMissI32 source receipt. The two observations are copied
/// from the independent GetAny found-observation collector, so the pair and
/// `pair.0` provenance are explicit; `branch_selectors` bind those observations
/// into the exact three-branch short-circuit CFG tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ZeroMissI32ObservationReceipt {
    pub(crate) owner: NodeId,
    pub(crate) formula_node: NodeId,
    pub(crate) outcome_node: NodeId,
    /// Exact tail expression that returns the immutable formula local. This
    /// is distinct from `formula_node`: the source tail is a direct Load of
    /// the formula binding and therefore has its own stable AST identity.
    pub(crate) return_node: NodeId,
    pub(crate) hit_observation: OrdinaryMapOwnedAnyV2FoundObservationReceipt,
    pub(crate) miss_observation: OrdinaryMapOwnedAnyV2FoundObservationReceipt,
    pub(crate) branch_selectors: [OrdinaryMapOwnedAnyV2BranchSelectorReceipt; 3],
}

/// Exact topology and owner-state receipt for the V2 single-diamond slice.
/// The carried-owner profile keeps root/result owners live across both arms;
/// its arms may contain read-only scalar Map observations, with creation and
/// settlement at the entry/join. A second profile permits a direct-New query
/// whose result and root are explicitly settled before the split. That profile
/// requires every split, arm, join, and edge owner set to be empty and admits
/// no Map V2 actions inside or after the arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2CfgFlowReceipt {
    /// Source authority selects the proof algorithm. Empty endpoint facts
    /// alone never authorize a settled scalar tail.
    pub(crate) proof: OrdinaryMapOwnedAnyV2CfgFlowProof,
    pub(crate) entry: crate::core::cfg::BasicBlockId,
    pub(crate) split: crate::core::cfg::BasicBlockId,
    pub(crate) join: crate::core::cfg::BasicBlockId,
    pub(crate) arm_edges: BTreeSet<crate::core::cfg::EdgeId>,
    pub(crate) reachable_blocks:
        BTreeMap<crate::core::cfg::BasicBlockId, OrdinaryMapOwnedAnyV2CfgBlock>,
    pub(crate) reachable_edges: BTreeMap<crate::core::cfg::EdgeId, crate::core::cfg::CfgEdge>,
    pub(crate) live_owners_in:
        BTreeMap<crate::core::cfg::BasicBlockId, BTreeSet<OrdinaryMapOwnedAnyV2LiveOwner>>,
    pub(crate) live_owners_out:
        BTreeMap<crate::core::cfg::BasicBlockId, BTreeSet<OrdinaryMapOwnedAnyV2LiveOwner>>,
    pub(crate) live_owners_on_edge:
        BTreeMap<crate::core::cfg::EdgeId, BTreeSet<OrdinaryMapOwnedAnyV2LiveOwner>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2CfgFlowProof {
    OwnerBearingDiamondV1,
    SettledScalarBranchTailV1(OrdinaryMapOwnedAnyV2ScalarTailReceipt),
    BranchLocalPersistentSetDiamondV1(OrdinaryMapOwnedAnyV2PersistentSetDiamondReceipt),
}

/// Exact Checker proof for one branch-local persistent Map Set lifecycle in
/// each arm of a four-block `if` diamond. Each arm transforms the carried
/// base root into a distinct child, observes that child once with `map_size`,
/// then explicitly drops it before the edge. The base root alone crosses the
/// split and is explicitly dropped in the common join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2PersistentSetDiamondReceipt {
    pub(crate) owner: NodeId,
    pub(crate) split: crate::core::cfg::BasicBlockId,
    pub(crate) join: crate::core::cfg::BasicBlockId,
    pub(crate) base_local: ResolvedLocalId,
    pub(crate) base_new: OrdinaryMapOwnedAnyV2Action,
    pub(crate) base_drop: OrdinaryMapOwnedAnyV2Action,
    /// Both Set calls clone this exact concrete record local. The per-arm
    /// payload load node is retained in each branch receipt as well.
    pub(crate) payload_local: ResolvedLocalId,
    pub(crate) payload_type: ResolvedTypeId,
    pub(crate) arms:
        BTreeMap<crate::core::cfg::EdgeId, OrdinaryMapOwnedAnyV2PersistentSetArmReceipt>,
}

/// The three source actions and exact edge/block identities for one arm.
/// Keeping full action records binds the arm's Set/size/Drop operation ids,
/// Map locals, root identities, payload/key operands, and persistent source
/// edge into this proof instead of relying only on owner-set equality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2PersistentSetArmReceipt {
    pub(crate) edge: crate::core::cfg::EdgeId,
    pub(crate) block: crate::core::cfg::BasicBlockId,
    pub(crate) join_edge: crate::core::cfg::EdgeId,
    pub(crate) set: OrdinaryMapOwnedAnyV2Action,
    pub(crate) size: OrdinaryMapOwnedAnyV2Action,
    pub(crate) drop: OrdinaryMapOwnedAnyV2Action,
    pub(crate) payload_load: NodeId,
}

/// Typed source commitment for a finite, owner-free scalar suffix. The CFG
/// snapshot binds topology; these nodes additionally bind the actual scalar
/// operations, selector operands, edge arguments, and unique return value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ScalarTailReceipt {
    pub(crate) owner: NodeId,
    pub(crate) first_condition: NodeId,
    pub(crate) first_condition_rank: usize,
    pub(crate) nodes: BTreeMap<NodeId, OrdinaryMapOwnedAnyV2ScalarTailNode>,
    pub(crate) bindings: BTreeMap<ResolvedLocalId, (NodeId, NodeId)>,
    pub(crate) selectors: BTreeMap<NodeId, OrdinaryMapOwnedAnyV2ScalarTailSelector>,
    /// Complete non-Copy local use census; every recorded point is in the
    /// entry prefix, and every local has a source-visible settlement there.
    pub(crate) owned_uses: BTreeMap<ResolvedLocalId, BTreeSet<NodeId>>,
    pub(crate) string_drops: BTreeMap<ResolvedLocalId, OrdinaryMapOwnedAnyV2ScalarTailStringDrop>,
    /// Only the independent exact ZeroMissI32 observation may authorize its
    /// existing final Boolean stdout observation. Generic scalar tails have
    /// no calls or effects.
    pub(crate) zero_miss_stdout: Option<(NodeId, NodeId)>,
    pub(crate) return_node: NodeId,
    pub(crate) return_type: ResolvedTypeId,
    pub(crate) return_block: crate::core::cfg::BasicBlockId,
}

/// Source-only last-use authority. This is minted exclusively from one
/// unprojected AST Drop; a synthesized scope cleanup can never create it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ScalarTailStringDrop {
    pub(crate) ty: ResolvedTypeId,
    pub(crate) point: NodeId,
    pub(crate) place_index: usize,
    pub(crate) point_rank: usize,
    /// Complete Load census, with exact CFG order retained independently of
    /// the set used for the general non-Copy source commitment.
    pub(crate) loads: Vec<(NodeId, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ScalarTailNode {
    pub(crate) ty: ResolvedTypeId,
    pub(crate) kind: OrdinaryMapOwnedAnyV2ScalarTailNodeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2ScalarTailNodeKind {
    Literal(crate::core::ir::ResolvedLiteral),
    Load(ResolvedLocalId),
    Unary(crate::core::ir::ResolvedUnaryOp, NodeId),
    Binary(crate::core::ir::ResolvedBinaryOp, NodeId, NodeId),
    Join,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ScalarTailSelector {
    pub(crate) selector: OrdinaryMapOwnedAnyV2BranchSelectorReceipt,
    pub(crate) then_value: NodeId,
    pub(crate) else_value: NodeId,
    pub(crate) then_source: NodeId,
    pub(crate) else_source: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OrdinaryMapOwnedAnyV2RootSettlementKind {
    ImmediateGetAnyDrop,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2TemporaryRootReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    /// Synthetic Checker local used only as an opaque MIR identity. It is
    /// deterministically derived from `construction` and is not source-visible.
    pub(crate) local: ResolvedLocalId,
    pub(crate) root: ResourceId,
    pub(crate) construction: NodeId,
    pub(crate) root_type: ResolvedTypeId,
    pub(crate) get_any: NodeId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    pub(crate) settlement: OrdinaryMapOwnedAnyV2RootSettlementKind,
    pub(crate) settlement_point: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OrdinaryMapOwnedAnyV2ResultSettlementKind {
    ExplicitDrop,
    Transfer,
    /// A result local was transferred through the callable's direct return
    /// expression. This outcome is authorized only by a profile-specific
    /// source receipt; it is not a generally receipted settlement.
    Return,
    Escape,
    ScopeDrop,
    Unsettled,
    DoubleSettlement,
}

impl OrdinaryMapOwnedAnyV2ResultSettlementKind {
    pub(crate) const fn is_receipted(self) -> bool {
        matches!(self, Self::ExplicitDrop)
    }
}

/// V2-specific result ownership evidence for GetAny and Values. The source
/// Map/key edge is duplicated here intentionally: consumers can validate the
/// result owner without borrowing a V1 receipt or reconstructing the call.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ResultReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) operation: NodeId,
    pub(crate) kind: OrdinaryMapOwnedAnyV2ActionKind,
    pub(crate) source_local: ResolvedLocalId,
    pub(crate) source: ResourceId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    pub(crate) key: Option<NodeId>,
    pub(crate) key_type: Option<ResolvedTypeId>,
    pub(crate) key_local: Option<ResolvedLocalId>,
    pub(crate) result: NodeId,
    pub(crate) result_type: ResolvedTypeId,
    pub(crate) binding: ResolvedLocalId,
    /// CFG point for the direct result binding statement. Keeping this point
    /// beside the operation and settlement lets MIR materialization verify
    /// the complete source ownership sequence rather than trusting a local
    /// name alone.
    pub(crate) binding_point: NodeId,
    /// Exact, direct borrowed payload observation. This source evidence does
    /// not authorize reinterpreting the V2 token as a ManagedAny-v1 owner.
    pub(crate) payload_println: Option<OrdinaryMapOwnedAnyV2PrintlnObservation>,
    pub(crate) settlement: OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: Option<NodeId>,
    /// Optional reusable L1 owner-chain source proof attached to the exact
    /// GetAny result whose independent snapshot is settled by this receipt.
    /// L2 must still prove computed String temporary ownership and cleanup.
    pub(crate) computed_string_key_owner_chain:
        Option<Box<OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainReceipt>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesProducerReturnProfileVersion {
    V1,
}

/// Separately versioned source shapes for the exact Result-wrapped Values
/// producer. The version names the one-entry and sorted two-entry fixtures;
/// neither extends the straight-line V1 contract above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesResultProducerProfileVersion {
    V1,
    V2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesResultPathKind {
    ValuesSnapshot,
    EmptyList,
}

/// One exact source Drop in a particular mutually exclusive Result path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesResultBranchDrop {
    pub(crate) local: ResolvedLocalId,
    pub(crate) point: NodeId,
    pub(crate) place_index: usize,
}

/// Complete terminal arm evidence for one fallible New/Set path. `arm_path`
/// is ordered from the outer `map_new` match through each nested `map_set`
/// match. The result kind and every source Drop are checked as a unit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesResultBranchPath {
    pub(crate) arm_path: Vec<NodeId>,
    pub(crate) result: NodeId,
    pub(crate) kind: OrdinaryMapOwnedAnyV2ValuesResultPathKind,
    pub(crate) drops: Vec<OrdinaryMapOwnedAnyV2ValuesResultBranchDrop>,
}

/// Source identities for one nested Result-wrapped `map_set`. The current
/// branch producer profiles require exactly two constructor arms, direct
/// local roots, no guards, and one arm-local successor/terminal shape.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesResultSetBranch {
    pub(crate) operation: NodeId,
    pub(crate) match_node: NodeId,
    pub(crate) input_root: ResolvedLocalId,
    /// Exact static String key expression and bytes consumed by this Set.
    pub(crate) key_node: NodeId,
    pub(crate) key_literal: String,
    /// Exact direct-local payload operand and the literal-backed owner that
    /// supplies it. The binding point and bytes are retained so MIR lowering
    /// can emit a checked String Const without reopening surface AST.
    pub(crate) payload_node: NodeId,
    pub(crate) payload_local: ResolvedLocalId,
    pub(crate) payload_binding_point: NodeId,
    pub(crate) payload_initializer_node: NodeId,
    pub(crate) payload_literal: String,
    pub(crate) ok_arm: NodeId,
    pub(crate) ok_pattern: NodeId,
    pub(crate) output_root: ResolvedLocalId,
    pub(crate) err_arm: NodeId,
    pub(crate) err_pattern: NodeId,
    pub(crate) error_local: ResolvedLocalId,
}

/// Exact Checker L1 receipt for a Result-wrapped Values producer. Besides
/// the operation chain, it carries the full reachable source CFG snapshot,
/// each map_new/map_set match arm, the success snapshot return, every empty
/// error return, and each path's explicit root/payload settlements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesResultProducerReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ValuesResultProducerProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) map_new_operation: NodeId,
    pub(crate) map_new_match: NodeId,
    pub(crate) map_new_ok_arm: NodeId,
    pub(crate) map_new_ok_pattern: NodeId,
    pub(crate) initial_root: ResolvedLocalId,
    pub(crate) map_new_err_arm: NodeId,
    pub(crate) map_new_err_pattern: NodeId,
    pub(crate) map_new_error_local: ResolvedLocalId,
    pub(crate) set_branches: Vec<OrdinaryMapOwnedAnyV2ValuesResultSetBranch>,
    pub(crate) values_operation: NodeId,
    pub(crate) values_source_local: ResolvedLocalId,
    pub(crate) values_source_node: NodeId,
    pub(crate) values_source_type: ResolvedTypeId,
    pub(crate) values_binding_point: NodeId,
    pub(crate) snapshot_local: ResolvedLocalId,
    pub(crate) snapshot_type: ResolvedTypeId,
    pub(crate) surface_return_type: ResolvedTypeId,
    pub(crate) success_result: NodeId,
    pub(crate) paths: Vec<OrdinaryMapOwnedAnyV2ValuesResultBranchPath>,
    pub(crate) root_settlements: Vec<OrdinaryMapOwnedAnyV2ValuesResultBranchDrop>,
    pub(crate) payload_settlements: Vec<OrdinaryMapOwnedAnyV2ValuesResultBranchDrop>,
    pub(crate) cfg: ManagedAnyCfgReceipt,
}

impl OrdinaryMapOwnedAnyV2ValuesResultProducerReceipt {
    /// Match only a Values result whose caller-visible Return settlement is
    /// the exact successful branch's ContainerErase expression. Other branch
    /// results and any unbound/aliased owner remain unreceipted.
    pub(crate) fn matches_returned_result(
        &self,
        result: &OrdinaryMapOwnedAnyV2ResultReceipt,
    ) -> bool {
        matches!(
            self.profile_version,
            OrdinaryMapOwnedAnyV2ValuesResultProducerProfileVersion::V1
                | OrdinaryMapOwnedAnyV2ValuesResultProducerProfileVersion::V2
        ) && result.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
            && result.kind == OrdinaryMapOwnedAnyV2ActionKind::Values
            && result.operation == self.values_operation
            && result.source_local == self.values_source_local
            && result.source_node == self.values_source_node
            && result.source_type == self.values_source_type
            && result.result == self.values_operation
            && result.result_type == self.snapshot_type
            && result.binding == self.snapshot_local
            && result.binding_point == self.values_binding_point
            && result.settlement == OrdinaryMapOwnedAnyV2ResultSettlementKind::Return
            && result.settlement_point.as_ref() == Some(&self.success_result)
    }
}

/// One exact Map-owned Values producer return. The receipt binds the source
/// Map root and actual List<DynamicAny> owner type to the direct binding and
/// unique tail ContainerErase into the surface bare List return, then records
/// every explicit root and direct String-payload Drop. It is narrower than
/// `OrdinaryMapOwnedAnyV2ResultReceipt`: the latter keeps Return as an
/// unreceipted outcome unless this complete profile is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesProducerReturnReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ValuesProducerReturnProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) values_operation: NodeId,
    pub(crate) source_local: ResolvedLocalId,
    pub(crate) source: ResourceId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) snapshot_local: ResolvedLocalId,
    /// Actual Values result type whose DynamicAny element ownership is tracked.
    pub(crate) snapshot_type: ResolvedTypeId,
    /// Callable's surface result type after the one admitted ContainerErase.
    pub(crate) surface_return_type: ResolvedTypeId,
    pub(crate) return_value: NodeId,
    pub(crate) return_container_erase: OrdinaryMapOwnedAnyV2ValuesProducerReturnEraseReceipt,
    pub(crate) settlement: OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) root_settlements: Vec<OrdinaryMapOwnedAnyV2ValuesProducerRootSettlement>,
    pub(crate) payload_settlements: Vec<OrdinaryMapOwnedAnyV2ValuesProducerPayloadSettlement>,
}

/// Exact typed edge for the producer's one admitted `List<DynamicAny> -> List`
/// return conversion. Its cast node is also `return_value` on the parent
/// receipt; the extra source identity lets downstream consumers verify the
/// direct owner load without reconstructing Checker evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesProducerReturnEraseReceipt {
    pub(crate) cast_node: NodeId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    pub(crate) target_type: ResolvedTypeId,
    pub(crate) conversion_kind: crate::core::ir::CheckedConversionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesProducerCallerProfileVersion {
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion {
    V1,
    /// Two statically keyed String entries (`a`, `b`) are returned in sorted
    /// Values order and the caller unpacks index one. V1 remains the
    /// single-entry, literal-zero profile.
    V2,
    /// One-entry Values producer whose Map construction is wrapped in
    /// exhaustive `Result` branches. The source branch receipt is carried
    /// separately from straight-line V1/V2 so it cannot widen their route.
    V3,
    /// Two-entry sorted Values producer whose Map construction is wrapped in
    /// exhaustive `Result` branches; the caller selects index one.
    V4,
}

impl OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "ordinary-map-owned-any-v2-values-crosscall-string-caller-v1",
            Self::V2 => "ordinary-map-owned-any-v2-values-crosscall-string-caller-v2",
            Self::V3 => "ordinary-map-owned-any-v2-values-crosscall-string-caller-v3-result-branch-one-entry",
            Self::V4 => "ordinary-map-owned-any-v2-values-crosscall-string-caller-v4-result-branch-two-entry",
        }
    }
}

/// Exact Checker ownership receipt for the separate cross-function Values
/// String extraction route. The producer's independently checked straightline
/// return receipt or Result-branch receipt owns source/root/payload
/// destruction; this record binds the unique caller's fresh snapshot, exact
/// literal-index checked String extraction, comparison, copied-String Drop,
/// snapshot Drop, and Boolean tail value. It is deliberately not inserted
/// into `values_unpack_receipts`, whose contract requires a same-function
/// Values source action and owner receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
    pub(crate) producer_owner: NodeId,
    pub(crate) producer_values_operation: NodeId,
    pub(crate) producer_return: NodeId,
    /// Present only for the additive Result-branch caller profiles V3/V4.
    /// V1/V2 retain their original straight-line producer receipt contract.
    pub(crate) producer_result_receipt: Option<OrdinaryMapOwnedAnyV2ValuesResultProducerReceipt>,
    pub(crate) caller_owner: NodeId,
    pub(crate) call_node: NodeId,
    pub(crate) call_target: NodeId,
    pub(crate) snapshot_local: ResolvedLocalId,
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) call_result_type: ResolvedTypeId,
    pub(crate) snapshot_type: ResolvedTypeId,
    pub(crate) snapshot_load: NodeId,
    pub(crate) index_projection: NodeId,
    pub(crate) index_expression: NodeId,
    pub(crate) index_type: ResolvedTypeId,
    /// Number of statically keyed String entries in the producer's sorted
    /// Values result. V1/V3 are exactly one; V2/V4 are exactly two.
    pub(crate) values_element_count: usize,
    /// The literal index selected by this profile: zero for V1/V3, one for V2/V4.
    pub(crate) index_value: i32,
    /// Producer's statically verified String keys in Values sort order.
    pub(crate) producer_sorted_keys: Vec<String>,
    /// Index1's four owned Set-expression temporaries, independently bound
    /// to their exact source Set action, source expression, String type, and
    /// semantic role. V1 intentionally keeps this empty so its exact profile
    /// identity remains independent.
    pub(crate) producer_set_temporary_settlements: Vec<OrdinaryMapOwnedAnyV2SetTemporarySettlement>,
    pub(crate) unpack_cast: NodeId,
    pub(crate) unpack_binding: ResolvedLocalId,
    pub(crate) unpack_binding_point: NodeId,
    pub(crate) string_type: ResolvedTypeId,
    pub(crate) comparison: NodeId,
    pub(crate) comparison_load: NodeId,
    pub(crate) expected_string: String,
    pub(crate) string_drop_point: NodeId,
    pub(crate) string_drop_place_index: usize,
    pub(crate) string_source_uses: BTreeSet<NodeId>,
    pub(crate) snapshot_drop_point: NodeId,
    pub(crate) snapshot_drop_place_index: usize,
    pub(crate) snapshot_source_uses: BTreeSet<NodeId>,
    pub(crate) boolean_local: ResolvedLocalId,
    pub(crate) boolean_binding_point: NodeId,
    pub(crate) boolean_type: ResolvedTypeId,
    pub(crate) caller_return: NodeId,
    pub(crate) caller_return_type: ResolvedTypeId,
}

/// Distinguishes the two temporary String owners materialized by one
/// receipted Map Set. `source_node` is the exact key expression for `Key`,
/// or the exact source payload Load that is cloned for `PayloadClone`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2SetTemporaryRole {
    Key,
    PayloadClone,
}

/// Checker-side source binding for a MIR-only post-Set owner settlement.
/// The Set initializer/action is the cleanup anchor because the generated
/// drops do not correspond to source Drop statements.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2SetTemporarySettlement {
    pub(crate) set_operation: NodeId,
    pub(crate) role: OrdinaryMapOwnedAnyV2SetTemporaryRole,
    pub(crate) source_node: NodeId,
    pub(crate) owner_type: ResolvedTypeId,
}

/// One direct callsite that receives the producer's surface bare List into a
/// fresh owner, optionally observes `snapshot.len()` once, and explicitly
/// drops that same owner before returning a scalar. `result_type` records the
/// surface call result; `owner_type` retains the producer's actual
/// List<DynamicAny> Values ownership obligation. The producer's own return
/// receipt remains separately available and is bound here by exact identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesProducerCallerReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ValuesProducerCallerProfileVersion,
    pub(crate) producer_owner: NodeId,
    pub(crate) producer_values_operation: NodeId,
    pub(crate) producer_implicit_return: NodeId,
    pub(crate) caller_owner: NodeId,
    pub(crate) call_node: NodeId,
    pub(crate) call_target: NodeId,
    pub(crate) result_binding: ResolvedLocalId,
    pub(crate) result_binding_point: NodeId,
    pub(crate) result_type: ResolvedTypeId,
    pub(crate) owner_resource: ResourceId,
    pub(crate) owner_local: ResolvedLocalId,
    pub(crate) owner_type: ResolvedTypeId,
    pub(crate) len_observation: Option<OrdinaryMapOwnedAnyV2ValuesProducerLenObservation>,
    pub(crate) owner_drop_point: NodeId,
    pub(crate) owner_drop_place_index: usize,
    pub(crate) caller_return: NodeId,
    pub(crate) caller_return_type: ResolvedTypeId,
    /// Exact Checker literal for the no-Len tail-return shape. This is `None`
    /// when `len_observation` names the source of the returned scalar.
    pub(crate) caller_return_literal: Option<crate::core::ir::ResolvedLiteral>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesProducerLenObservation {
    pub(crate) call_node: NodeId,
    pub(crate) receiver_node: NodeId,
    pub(crate) result_binding: ResolvedLocalId,
    pub(crate) result_binding_point: NodeId,
    pub(crate) result_type: ResolvedTypeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesResultProducerCallerProfileVersion {
    V1,
}

#[cfg(test)]
impl OrdinaryMapOwnedAnyV2ValuesResultProducerCallerProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => {
                "ordinary-map-owned-any-v2-values-result-producer-caller-v1-one-length-return"
            }
        }
    }
}

/// One exact `List.len()` observation on the returned Values snapshot. Both
/// the receiver conversion and result binding identities are committed so a
/// caller cannot move the observation across the owner Drop or substitute an
/// unrelated scalar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesResultProducerLenObservation {
    pub(crate) call_node: NodeId,
    pub(crate) receiver_node: NodeId,
    pub(crate) receiver_local: ResolvedLocalId,
    pub(crate) receiver_type: ResolvedTypeId,
    pub(crate) conversion_kind: crate::core::ir::CheckedConversionKind,
    pub(crate) conversion_from: ResolvedTypeId,
    pub(crate) conversion_to: ResolvedTypeId,
    pub(crate) result_binding: ResolvedLocalId,
    pub(crate) result_binding_point: NodeId,
    pub(crate) result_type: ResolvedTypeId,
}

/// Whole-program Checker receipt joining one exhaustively settled Result
/// Values producer with its unique direct caller. The caller observes the
/// transferred snapshot length once, drops that same owner, and returns the
/// exact scalar result. It is independent from the straight-line
/// producer-caller profile and the cross-call String index profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesResultProducerCallerReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ValuesResultProducerCallerProfileVersion,
    pub(crate) producer_result: OrdinaryMapOwnedAnyV2ValuesResultProducerReceipt,
    pub(crate) producer_owner: NodeId,
    pub(crate) producer_values_operation: NodeId,
    pub(crate) producer_success_result: NodeId,
    pub(crate) caller_owner: NodeId,
    pub(crate) producer_call: NodeId,
    pub(crate) producer_target: NodeId,
    pub(crate) snapshot_local: ResolvedLocalId,
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) call_result_type: ResolvedTypeId,
    pub(crate) snapshot_owner: ResourceId,
    pub(crate) snapshot_type: ResolvedTypeId,
    pub(crate) len: OrdinaryMapOwnedAnyV2ValuesResultProducerLenObservation,
    pub(crate) owner_drop: NodeId,
    pub(crate) owner_drop_place_index: usize,
    pub(crate) caller_return: NodeId,
    pub(crate) caller_return_type: ResolvedTypeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesProducerRootSettlement {
    pub(crate) local: ResolvedLocalId,
    pub(crate) resource: ResourceId,
    pub(crate) root_type: ResolvedTypeId,
    pub(crate) binding_point: NodeId,
    pub(crate) drop_point: NodeId,
    pub(crate) drop_place_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesProducerPayloadSettlement {
    pub(crate) local: ResolvedLocalId,
    pub(crate) payload_type: ResolvedTypeId,
    pub(crate) binding_point: NodeId,
    pub(crate) set_points: Vec<NodeId>,
    pub(crate) drop_point: NodeId,
    pub(crate) drop_place_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2MapRootProducerProfileVersion {
    V1,
}

/// One exact, straight-line Map-root producer. `initial_root` is created by
/// `map_new`; `final_root` is the distinct persistent root created by the one
/// `map_set`. The source identity and transition point make the lineage
/// explicit instead of treating a Map root as a Values-like snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2MapRootProducerReturnReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2MapRootProducerProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) map_new_operation: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) initial_root: ResourceId,
    pub(crate) initial_root_type: ResolvedTypeId,
    pub(crate) initial_root_binding_point: NodeId,
    pub(crate) map_set_operation: NodeId,
    pub(crate) final_root_local: ResolvedLocalId,
    pub(crate) final_root: ResourceId,
    pub(crate) final_root_type: ResolvedTypeId,
    pub(crate) final_root_binding_point: NodeId,
    pub(crate) set_source_local: ResolvedLocalId,
    pub(crate) set_source_root: ResourceId,
    pub(crate) set_source_node: NodeId,
    pub(crate) set_key_node: NodeId,
    pub(crate) payload_local: ResolvedLocalId,
    pub(crate) payload_type: ResolvedTypeId,
    pub(crate) payload_binding_point: NodeId,
    pub(crate) predecessor_drop_point: NodeId,
    pub(crate) predecessor_drop_place_index: usize,
    pub(crate) payload_drop_point: NodeId,
    pub(crate) payload_drop_place_index: usize,
    pub(crate) return_value: NodeId,
    pub(crate) return_load: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2MapRootCallerProfileVersion {
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2MapRootGetAnyCallerProfileVersion {
    V1,
}

/// One exact straight-line caller of a Map-root producer. The caller binds
/// the returned root, performs a literal-key `map_get`, unpacks field 1 as an
/// owned String, then explicitly drops the GetAny tuple, the Map root, and
/// the copied String before returning an i32 literal. The producer's logical
/// root identity is carried across the direct call boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2MapRootGetAnyCallerReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2MapRootGetAnyCallerProfileVersion,
    pub(crate) producer_owner: NodeId,
    pub(crate) producer_map_set_operation: NodeId,
    pub(crate) producer_return_value: NodeId,
    pub(crate) producer_final_local: ResolvedLocalId,
    pub(crate) producer_final_binding_point: NodeId,
    pub(crate) logical_root: ResourceId,
    pub(crate) caller_owner: NodeId,
    pub(crate) call_node: NodeId,
    pub(crate) call_target: NodeId,
    pub(crate) caller_root_local: ResolvedLocalId,
    pub(crate) caller_root_binding_point: NodeId,
    pub(crate) caller_root_type: ResolvedTypeId,
    pub(crate) get_any_node: NodeId,
    pub(crate) get_any_receiver_node: NodeId,
    pub(crate) get_any_key_node: NodeId,
    pub(crate) pair_local: ResolvedLocalId,
    pub(crate) pair_binding_point: NodeId,
    pub(crate) pair_type: ResolvedTypeId,
    pub(crate) unpack_projection: NodeId,
    pub(crate) unpack_cast: NodeId,
    pub(crate) unpack_local: ResolvedLocalId,
    pub(crate) unpack_binding_point: NodeId,
    pub(crate) string_type: ResolvedTypeId,
    pub(crate) pair_drop_point: NodeId,
    pub(crate) root_drop_point: NodeId,
    pub(crate) string_drop_point: NodeId,
    pub(crate) string_drop_place_index: usize,
    pub(crate) caller_return: NodeId,
    pub(crate) caller_return_type: ResolvedTypeId,
    pub(crate) caller_return_literal: crate::core::ir::ResolvedLiteral,
}

/// Exact cross-call Checker bridge for the first executable
/// `std::maps.get_or_default<i32>` slice.  The caller fields bind the Map
/// root, its sole `i32` Set lineage, the literal query key/default, and the
/// caller-owned root Drop.  The helper fields independently commit the
/// canonical generic helper's formal argument order, GetAny pair, found-only
/// DynamicAny unpack, selected value, and explicit whole-pair Drop.  This
/// receipt grants no authority to other instantiations or callsites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2GetOrDefaultI32Receipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2GetOrDefaultI32ProfileVersion,
    pub(crate) caller_owner: NodeId,
    pub(crate) call_node: NodeId,
    pub(crate) call_target: NodeId,
    pub(crate) call_type_argument: ResolvedTypeId,
    pub(crate) call_arguments: Vec<NodeId>,
    pub(crate) call_parameters: Vec<ResolvedParameterId>,
    pub(crate) call_argument_types: Vec<ResolvedTypeId>,
    pub(crate) call_conversions: Vec<crate::core::ir::CheckedConversion>,
    pub(crate) call_result_type: ResolvedTypeId,
    pub(crate) caller_result_local: ResolvedLocalId,
    pub(crate) caller_call_binding_point: NodeId,
    pub(crate) caller_initial_root_local: ResolvedLocalId,
    pub(crate) caller_root_local: ResolvedLocalId,
    pub(crate) caller_root: ResourceId,
    pub(crate) caller_root_type: ResolvedTypeId,
    pub(crate) caller_new_point: NodeId,
    pub(crate) caller_set_point: NodeId,
    pub(crate) caller_set_key: NodeId,
    pub(crate) caller_set_key_bytes: String,
    pub(crate) caller_set_value: NodeId,
    pub(crate) caller_set_value_i32: i64,
    pub(crate) caller_set_value_type: ResolvedTypeId,
    pub(crate) caller_query_key: NodeId,
    pub(crate) caller_query_key_bytes: String,
    pub(crate) caller_default: NodeId,
    pub(crate) caller_default_i32: i64,
    pub(crate) caller_default_type: ResolvedTypeId,
    pub(crate) caller_root_drop_point: NodeId,
    pub(crate) caller_root_drop_place_index: usize,
    pub(crate) caller_initial_root_drop_point: NodeId,
    pub(crate) caller_initial_root_drop_place_index: usize,
    pub(crate) helper_owner: NodeId,
    pub(crate) helper_map_parameter: ResolvedParameterId,
    pub(crate) helper_map_permission: Permission,
    pub(crate) helper_key_parameter: ResolvedParameterId,
    pub(crate) helper_default_parameter: ResolvedParameterId,
    pub(crate) helper_map_local: ResolvedLocalId,
    pub(crate) helper_key_local: ResolvedLocalId,
    pub(crate) helper_default_local: ResolvedLocalId,
    pub(crate) helper_get_any_call: NodeId,
    pub(crate) helper_pair_local: ResolvedLocalId,
    pub(crate) helper_pair_binding_point: NodeId,
    pub(crate) helper_pair_type: ResolvedTypeId,
    pub(crate) helper_found_condition: NodeId,
    pub(crate) helper_found_projection: NodeId,
    pub(crate) helper_payload_projection: NodeId,
    pub(crate) helper_unpack_cast: NodeId,
    pub(crate) helper_unpack_target: ResolvedTypeId,
    pub(crate) helper_if_node: NodeId,
    pub(crate) helper_found_arm_block: NodeId,
    pub(crate) helper_default_arm_block: NodeId,
    pub(crate) helper_selected_local: ResolvedLocalId,
    pub(crate) helper_selected_binding_point: NodeId,
    pub(crate) helper_default_load: NodeId,
    pub(crate) helper_pair_drop_point: NodeId,
    pub(crate) helper_return_node: NodeId,
    pub(crate) helper_result_type: ResolvedTypeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2GetOrDefaultI32ProfileVersion {
    V1,
}

impl OrdinaryMapOwnedAnyV2GetOrDefaultI32ProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "ordinary-map-owned-any-v2-get-or-default-i32-v1",
        }
    }
}

/// Exact cross-call Checker bridge for the narrow ordinary Map/Any
/// `std::maps.get_or_default<String>` slice. The caller fields bind its Map
/// root, sole String Set lineage, literal query key/default, and root Drops.
/// Helper fields commit the canonical generic helper's argument order,
/// GetAny pair, found-only String unpack, selected value, and pair Drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2GetOrDefaultStringReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2GetOrDefaultStringProfileVersion,
    pub(crate) caller_owner: NodeId,
    pub(crate) call_node: NodeId,
    pub(crate) call_target: NodeId,
    pub(crate) call_type_argument: ResolvedTypeId,
    pub(crate) miss_payload_type: ResolvedTypeId,
    pub(crate) call_arguments: Vec<NodeId>,
    pub(crate) call_parameters: Vec<ResolvedParameterId>,
    pub(crate) call_argument_types: Vec<ResolvedTypeId>,
    pub(crate) call_conversions: Vec<crate::core::ir::CheckedConversion>,
    pub(crate) call_result_type: ResolvedTypeId,
    pub(crate) caller_result_local: ResolvedLocalId,
    pub(crate) caller_call_binding_point: NodeId,
    pub(crate) caller_initial_root_local: ResolvedLocalId,
    pub(crate) caller_root_local: ResolvedLocalId,
    /// Map Set creates this persistent root. When the call receiver is an
    /// immutable alias, this source root has its own identity and Drop.
    pub(crate) caller_set_root_local: ResolvedLocalId,
    pub(crate) caller_root: ResourceId,
    pub(crate) caller_root_type: ResolvedTypeId,
    pub(crate) caller_new_point: NodeId,
    pub(crate) caller_set_point: NodeId,
    pub(crate) caller_set_key: NodeId,
    pub(crate) caller_set_key_bytes: String,
    pub(crate) caller_set_value: NodeId,
    pub(crate) caller_set_value_local: Option<ResolvedLocalId>,
    pub(crate) caller_set_value_binding_point: Option<NodeId>,
    pub(crate) caller_set_value_drop_point: Option<NodeId>,
    pub(crate) caller_set_value_drop_place_index: Option<usize>,
    pub(crate) caller_set_value_string: String,
    pub(crate) caller_set_value_type: ResolvedTypeId,
    pub(crate) caller_query_key: NodeId,
    pub(crate) caller_query_key_bytes: String,
    pub(crate) caller_default: NodeId,
    pub(crate) caller_default_local: Option<ResolvedLocalId>,
    pub(crate) caller_default_binding_point: Option<NodeId>,
    pub(crate) caller_default_string: String,
    pub(crate) caller_default_type: ResolvedTypeId,
    pub(crate) caller_result_settlement: OrdinaryMapOwnedAnyV2GetOrDefaultStringResultSettlement,
    pub(crate) caller_root_drop_point: NodeId,
    pub(crate) caller_root_drop_place_index: usize,
    pub(crate) caller_set_root_drop_point: NodeId,
    pub(crate) caller_set_root_drop_place_index: usize,
    pub(crate) caller_initial_root_drop_point: NodeId,
    pub(crate) caller_initial_root_drop_place_index: usize,
    pub(crate) helper_owner: NodeId,
    pub(crate) helper_map_parameter: ResolvedParameterId,
    pub(crate) helper_map_permission: Permission,
    pub(crate) helper_key_parameter: ResolvedParameterId,
    pub(crate) helper_default_parameter: ResolvedParameterId,
    pub(crate) helper_map_local: ResolvedLocalId,
    pub(crate) helper_key_local: ResolvedLocalId,
    pub(crate) helper_default_local: ResolvedLocalId,
    pub(crate) helper_get_any_call: NodeId,
    pub(crate) helper_pair_local: ResolvedLocalId,
    pub(crate) helper_pair_binding_point: NodeId,
    pub(crate) helper_pair_type: ResolvedTypeId,
    pub(crate) helper_found_condition: NodeId,
    pub(crate) helper_found_projection: NodeId,
    pub(crate) helper_payload_projection: NodeId,
    pub(crate) helper_default_pack: NodeId,
    pub(crate) helper_default_pack_from: ResolvedTypeId,
    pub(crate) helper_default_pack_to: ResolvedTypeId,
    pub(crate) helper_unpack_cast: NodeId,
    pub(crate) helper_unpack_target: ResolvedTypeId,
    pub(crate) helper_if_node: NodeId,
    pub(crate) helper_found_arm_block: NodeId,
    pub(crate) helper_default_arm_block: NodeId,
    pub(crate) helper_selected_local: ResolvedLocalId,
    pub(crate) helper_selected_binding_point: NodeId,
    pub(crate) helper_default_load: NodeId,
    pub(crate) helper_pair_drop_point: NodeId,
    pub(crate) helper_return_node: NodeId,
    pub(crate) helper_result_type: ResolvedTypeId,
}

/// Exactly-once settlement for the owned String returned from the fused
/// operation. The source helper may return the value from its caller directly,
/// while a terminal entrypoint must explicitly drop it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OrdinaryMapOwnedAnyV2GetOrDefaultStringResultSettlement {
    ExplicitDrop { point: NodeId, place_index: usize },
    TailReturn { point: NodeId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2GetOrDefaultStringProfileVersion {
    V1,
}

impl OrdinaryMapOwnedAnyV2GetOrDefaultStringProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "ordinary-map-owned-any-v2-get-or-default-string-v1",
        }
    }
}

/// The unique direct caller receives the producer's final logical root under
/// a fresh local, performs one borrowed `map_size`, then explicitly drops the
/// same local before returning that scalar observation. `logical_root` must
/// equal the producer receipt's `final_root` even though the caller-local
/// spelling is different.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2MapRootProducerCallerReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2MapRootCallerProfileVersion,
    pub(crate) producer_owner: NodeId,
    pub(crate) producer_map_set_operation: NodeId,
    pub(crate) producer_return_value: NodeId,
    pub(crate) producer_final_local: ResolvedLocalId,
    pub(crate) producer_final_binding_point: NodeId,
    pub(crate) logical_root: ResourceId,
    pub(crate) caller_owner: NodeId,
    pub(crate) call_node: NodeId,
    pub(crate) call_target: NodeId,
    pub(crate) result_binding: ResolvedLocalId,
    pub(crate) result_binding_point: NodeId,
    pub(crate) result_type: ResolvedTypeId,
    pub(crate) caller_root_local: ResolvedLocalId,
    pub(crate) caller_root_type: ResolvedTypeId,
    pub(crate) map_size_node: NodeId,
    pub(crate) map_size_receiver_node: NodeId,
    pub(crate) size_binding: ResolvedLocalId,
    pub(crate) size_binding_point: NodeId,
    pub(crate) size_type: ResolvedTypeId,
    pub(crate) root_drop_point: NodeId,
    pub(crate) root_drop_place_index: usize,
    pub(crate) caller_return: NodeId,
    pub(crate) caller_return_type: ResolvedTypeId,
}

impl OrdinaryMapOwnedAnyV2ValuesProducerReturnReceipt {
    pub(crate) fn matches_returned_result(
        &self,
        result: &OrdinaryMapOwnedAnyV2ResultReceipt,
    ) -> bool {
        self.profile_version == OrdinaryMapOwnedAnyV2ValuesProducerReturnProfileVersion::V1
            && result.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
            && result.kind == OrdinaryMapOwnedAnyV2ActionKind::Values
            && result.operation == self.values_operation
            && result.source_local == self.source_local
            && result.source == self.source
            && result.source_node == self.source_node
            && result.source_type == self.source_type
            && result.result == self.values_operation
            && result.result_type == self.snapshot_type
            && result.binding == self.snapshot_local
            && result.binding_point == self.snapshot_binding_point
            && result.settlement == OrdinaryMapOwnedAnyV2ResultSettlementKind::Return
            && result.settlement_point.as_ref() == Some(&self.return_value)
    }
}

/// Checker-owned source evidence for observing `GetAny`'s Boolean found
/// discriminator. The receipt binds one exact operation and result-pair
/// binding to a direct tuple-field-0 load, then commits every direct load of
/// the immutable Boolean local and the unique callable Boolean result that
/// contains those loads. It does not infer found status from the payload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2FoundObservationReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) operation: NodeId,
    pub(crate) pair_binding_point: NodeId,
    pub(crate) pair_binding: ResolvedLocalId,
    /// NodeId of the actual source `pair.0` projection expression.
    pub(crate) projection: NodeId,
    pub(crate) found_binding_point: NodeId,
    pub(crate) found_binding: ResolvedLocalId,
    /// Complete callable census of direct loads from `found_binding`.
    pub(crate) found_source_uses: BTreeSet<NodeId>,
    /// The callable's unique Boolean root result containing every load above.
    pub(crate) outcome_node: NodeId,
}

/// The one admitted `println(pair.1)` source shape. The enclosing result
/// receipt binds the pair, source Map/key, binding and whole-pair Drop.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2PrintlnObservation {
    pub(crate) projection: NodeId,
    pub(crate) call: NodeId,
    pub(crate) checker_payload_type: ResolvedTypeId,
    pub(crate) result_type: ResolvedTypeId,
}

/// A Checker-owned source receipt for one precisely sourced GetAny payload
/// unpack. The projection is observational: the pair binding remains the
/// owner and must still be explicitly dropped after this conversion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2UnpackReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    /// The exact `map_get` call which produced the pair.
    pub(crate) operation: NodeId,
    /// Statement and local for the direct `let pair = map_get(...)` binding.
    pub(crate) pair_binding_point: NodeId,
    pub(crate) pair_binding: ResolvedLocalId,
    /// Statement and local receiving the exact checked cast.
    pub(crate) unpack_binding_point: NodeId,
    pub(crate) unpack_binding: ResolvedLocalId,
    /// Node IDs for `pair.1` and `pair.1 as T`, respectively.
    pub(crate) projection: NodeId,
    pub(crate) cast: NodeId,
    pub(crate) conversion_kind: crate::core::ir::CheckedConversionKind,
    pub(crate) conversion_from: ResolvedTypeId,
    pub(crate) conversion_to: ResolvedTypeId,
    /// Explicit whole-pair settlement point, tied to the result receipt.
    pub(crate) settlement: OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2StaticKeyGetProfileVersion {
    V1,
}

/// Result category proved by replaying the exact static-key lineage from New
/// to this GetAny. Only a positive Set winner can authorize `unpack_string`;
/// absent keys are represented solely for the canonical i32 zero-miss path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource {
    SelectedSet { value_type: ResolvedTypeId },
    I32MissAtNew,
    I32MissAtRemove,
}

/// Versioned Checker credential for one direct builtin `unpack_i32` or
/// `unpack_string` over GetAny's exact field-1 DynamicAny. The GetAny pair is
/// explicitly dropped after the unpack; `selected_winning_set_action` is the
/// exact latest same-key Set on this root lineage, and is absent for a proved
/// canonical-i32 miss.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2StaticKeyGetReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2StaticKeyGetProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) get_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) unpack: OrdinaryMapOwnedAnyV2UnpackReceipt,
    pub(crate) query_digest: String,
    pub(crate) payload_source: OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource,
    pub(crate) selected_winning_set_action: Option<OrdinaryMapOwnedAnyV2Action>,
}

/// Checker-owned source authority for `Set(root, key, String) ->
/// Remove(set_root, key) -> GetAny(remove_root, key)`, followed by the exact
/// `found` and canonical-i32 unpack observations. The nested action and result
/// receipts preserve each source identity; `key_bytes` is the shared literal
/// value, not a digest-only equality claim.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2RemovedKeyCheckedMissReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) new_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) set_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) remove_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) get_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) key_bytes: Vec<u8>,
    pub(crate) get_result: OrdinaryMapOwnedAnyV2ResultReceipt,
    pub(crate) found_observation: OrdinaryMapOwnedAnyV2FoundObservationReceipt,
    pub(crate) unpack: OrdinaryMapOwnedAnyV2UnpackReceipt,
    /// Exact Boolean conjunction: the first conjunct negates this direct
    /// GetAny `found` binding, and the second compares this direct i32 unpack
    /// binding with literal zero. Storing both nodes prevents a truth-table
    /// coincidence such as `!found == (payload == 0)` from sharing authority.
    pub(crate) observation_formula: OrdinaryMapOwnedAnyV2RemovedKeyCheckedMissFormulaReceipt,
    /// Exact explicit root Drops for New, Set, and Remove results.
    pub(crate) root_settlements: Vec<OrdinaryMapOwnedAnyV2Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2RemovedKeyCheckedMissFormulaReceipt {
    pub(crate) shape: OrdinaryMapOwnedAnyV2RemovedKeyCheckedMissFormulaShape,
    pub(crate) outcome_node: NodeId,
    pub(crate) found_false_node: NodeId,
    pub(crate) found_binding: ResolvedLocalId,
    pub(crate) payload_zero_node: NodeId,
    pub(crate) payload_binding: ResolvedLocalId,
    pub(crate) zero_literal_node: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2RemovedKeyCheckedMissFormulaShape {
    NotFoundAndCanonicalI32Zero,
}

/// Checker classification of the owner obligation created by one Values
/// element unpack. Copy payloads need no owner settlement, Strings are
/// settled by compiler-owned binding cleanup, and aggregate payloads require
/// a source-visible whole-binding Drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind {
    NoOwnedPayload,
    CompilerOwnedStringDrop,
    /// A source-visible whole-value Drop settles a Values-unpacked String
    /// after a closed set of direct literal String comparisons.
    ExplicitStringDrop,
    ExplicitWholeDrop,
    /// The unpacked canonical one-field Label is consumed by one direct
    /// `item.text` MoveProject, and that moved String is explicitly dropped.
    DirectStringFieldMove,
}

/// Checker disposition for a statically exact index into a Values snapshot.
/// An out-of-bounds index is a distinct, deterministic trap outcome and never
/// carries payload-selection lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesIndexDisposition {
    SelectedPayload,
    AlwaysOutOfBounds { raw_index: i64, snapshot_len: usize },
}

impl OrdinaryMapOwnedAnyV2ValuesIndexDisposition {
    pub(crate) fn is_canonical(self) -> bool {
        match self {
            Self::SelectedPayload => true,
            Self::AlwaysOutOfBounds {
                raw_index,
                snapshot_len,
            } if raw_index >= 0 => {
                usize::try_from(raw_index).is_ok_and(|index| index >= snapshot_len)
            }
            Self::AlwaysOutOfBounds {
                raw_index,
                snapshot_len,
            } => i64::try_from(snapshot_len)
                .ok()
                .and_then(|length| length.checked_add(raw_index))
                .is_some_and(|normalized| normalized < 0),
        }
    }
}

/// Complete Checker source-use commitment for the one admitted Label field
/// move. The parent-use census is retained verbatim and must contain only the
/// exact `item.text` projection. The child may have direct String equality
/// reads, each tied to its source operation and root statement order, followed
/// by its exact explicit whole-value String Drop. No aliasing use is admitted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesUnpackTextReadReceipt {
    pub(crate) load: NodeId,
    pub(crate) operation: NodeId,
    pub(crate) statement: NodeId,
    pub(crate) statement_order: usize,
    pub(crate) not_equal: bool,
    pub(crate) peer: NodeId,
    pub(crate) peer_literal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesUnpackFieldMoveReceipt {
    pub(crate) field_projection: NodeId,
    pub(crate) field: NodeId,
    pub(crate) field_name: String,
    pub(crate) text_binding_point: NodeId,
    pub(crate) text_binding: ResolvedLocalId,
    pub(crate) text_binding_statement_order: usize,
    pub(crate) text_settlement_point: NodeId,
    pub(crate) text_settlement_index: usize,
    pub(crate) text_settlement_statement_order: usize,
    pub(crate) parent_source_uses: BTreeSet<NodeId>,
    pub(crate) text_source_uses: BTreeSet<NodeId>,
    pub(crate) text_read_uses: Vec<OrdinaryMapOwnedAnyV2ValuesUnpackTextReadReceipt>,
}

/// One old Label payload selected by the Values snapshot's UTF-8-key-sorted
/// position. The nested field-move receipt proves that this exact unpacked
/// owner was read and explicitly settled after its String field moved out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesSnapshotEntryReceipt {
    pub(crate) key_bytes: Vec<u8>,
    pub(crate) key_expression: NodeId,
    pub(crate) set_point: NodeId,
    pub(crate) payload_node: NodeId,
    pub(crate) payload_type: ResolvedTypeId,
    pub(crate) payload_text: String,
    pub(crate) payload_owner_settlement: OrdinaryMapOwnedAnyV2ValuesPayloadOwnerSettlementReceipt,
    pub(crate) index: usize,
    pub(crate) unpack_binding_point: NodeId,
    pub(crate) unpack_binding: ResolvedLocalId,
    pub(crate) cast: NodeId,
    pub(crate) conversion_to: ResolvedTypeId,
    pub(crate) owner_settlement: OrdinaryMapOwnedAnyV2ValuesUnpackFieldMoveReceipt,
}

/// The post-snapshot Set and every root it creates/consumes are tied to their
/// exact source identities. `key_bytes` is computed by the Checker from the
/// source String expression and validated against that Set's source node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesSnapshotReplacementReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    /// Exact root-level `let initial = map_new()` construction which begins
    /// the persistent chain consumed by the two pre-snapshot Sets.
    pub(crate) initial_root_point: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) operation: NodeId,
    pub(crate) source_local: ResolvedLocalId,
    pub(crate) source: ResourceId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) snapshot_binding: ResolvedLocalId,
    pub(crate) snapshot_type: ResolvedTypeId,
    /// Exact explicit Drop which settles the Values snapshot owner after its
    /// two entry reads.
    pub(crate) snapshot_settlement_point: NodeId,
    pub(crate) snapshot_settlement_place_index: usize,
    /// Always exactly two entries, in ascending UTF-8 key-byte order, with
    /// indices zero and one respectively.
    pub(crate) entries: [OrdinaryMapOwnedAnyV2ValuesSnapshotEntryReceipt; 2],
    pub(crate) replacement: OrdinaryMapOwnedAnyV2ValuesReplacementSetReceipt,
    /// Exact explicit Drops for every persistent Map root in this chain,
    /// each located at one unprojected place in a source Drop statement.
    pub(crate) map_root_settlements: Vec<OrdinaryMapOwnedAnyV2ValuesMapRootSettlementReceipt>,
}

/// A sorted-key entry in the narrowly admitted heterogeneous Values
/// snapshot. The complete unpack receipt is copied so the admission binds the
/// exact Checker conversion, result ownership disposition, and source-use
/// census rather than relying on an index/type summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesMixedSnapshotEntryReceipt {
    pub(crate) key_bytes: Vec<u8>,
    pub(crate) key_expression: NodeId,
    pub(crate) set_point: NodeId,
    pub(crate) payload_node: NodeId,
    pub(crate) payload_type: ResolvedTypeId,
    pub(crate) payload_text: Option<String>,
    /// Present for the one direct Label source local; literal i32/String
    /// payloads have no source-local owner to settle here.
    pub(crate) payload_owner_settlement:
        Option<OrdinaryMapOwnedAnyV2ValuesPayloadOwnerSettlementReceipt>,
    pub(crate) index: usize,
    pub(crate) unpack: OrdinaryMapOwnedAnyV2ValuesUnpackReceipt,
}

/// Checker-owned exact proof for the mixed 0/1/2 Values snapshot slice.
/// Four persistent Map roots and the original Label source are released
/// before the first indexed read; the result owners and snapshot are settled
/// by the copied per-unpack Checker receipts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesMixedSnapshotReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) initial_root_point: NodeId,
    pub(crate) initial_root_local: ResolvedLocalId,
    pub(crate) operation: NodeId,
    pub(crate) source_local: ResolvedLocalId,
    pub(crate) source: ResourceId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) snapshot_binding: ResolvedLocalId,
    pub(crate) snapshot_type: ResolvedTypeId,
    pub(crate) snapshot_settlement_point: NodeId,
    pub(crate) snapshot_settlement_place_index: usize,
    pub(crate) entries: [OrdinaryMapOwnedAnyV2ValuesMixedSnapshotEntryReceipt; 3],
    pub(crate) map_root_settlements: Vec<OrdinaryMapOwnedAnyV2ValuesMapRootSettlementReceipt>,
}

/// Checker-owned source proof for the exact heterogeneous Values snapshot
/// plus one later `map_remove(snapshot.source_local, "b-text")`. The nested
/// snapshot receipt preserves the established three-entry ownership proof;
/// this wrapper separately binds the removal result and its explicit root
/// settlement before the first indexed unpack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemoveReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) snapshot: OrdinaryMapOwnedAnyV2ValuesMixedSnapshotReceipt,
    pub(crate) remove_action: OrdinaryMapOwnedAnyV2Action,
    pub(crate) remove_binding_point: NodeId,
    pub(crate) snapshot_statement_order: usize,
    pub(crate) remove_statement_order: usize,
    pub(crate) remove_key_bytes: Vec<u8>,
    pub(crate) remove_result_type: ResolvedTypeId,
    pub(crate) removed_root_settlement: OrdinaryMapOwnedAnyV2ValuesMapRootSettlementReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesReplacementSetReceipt {
    pub(crate) point: NodeId,
    pub(crate) key_bytes: Vec<u8>,
    pub(crate) key_expression: NodeId,
    pub(crate) source_local: ResolvedLocalId,
    pub(crate) source: ResourceId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    pub(crate) result_local: ResolvedLocalId,
    pub(crate) result: ResourceId,
    pub(crate) result_type: ResolvedTypeId,
    pub(crate) payload_node: NodeId,
    pub(crate) payload_type: ResolvedTypeId,
    pub(crate) payload_text: String,
    pub(crate) payload_owner_settlement: OrdinaryMapOwnedAnyV2ValuesPayloadOwnerSettlementReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesPayloadOwnerSettlementReceipt {
    pub(crate) local: ResolvedLocalId,
    pub(crate) drop_point: NodeId,
    pub(crate) drop_place_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesMapRootSettlementReceipt {
    pub(crate) local: ResolvedLocalId,
    pub(crate) resource: ResourceId,
    pub(crate) drop_point: NodeId,
    pub(crate) drop_place_index: usize,
}

impl OrdinaryMapOwnedAnyV2ValuesUnpackFieldMoveReceipt {
    /// The source-use sets are a complete census: the Label may be touched
    /// only by its one field projection, while the moved String may be read
    /// only by the receipted direct equality operands before its one Drop.
    pub(crate) fn has_exact_source_use_sets(&self) -> bool {
        let parent_uses = BTreeSet::from([self.field_projection.clone()]);
        let mut text_uses = self
            .text_read_uses
            .iter()
            .map(|read| read.load.clone())
            .collect::<BTreeSet<_>>();
        text_uses.insert(self.text_settlement_point.clone());
        let mut loads = BTreeSet::new();
        let mut operations = BTreeSet::new();
        let ordered_reads = self
            .text_read_uses
            .windows(2)
            .all(|pair| pair[0].statement_order < pair[1].statement_order);
        self.parent_source_uses == parent_uses
            && self.text_source_uses == text_uses
            && self.text_read_uses.iter().all(|read| {
                !read.load.0.trim().is_empty()
                    && !read.operation.0.trim().is_empty()
                    && !read.statement.0.trim().is_empty()
                    && !read.peer.0.trim().is_empty()
                    && loads.insert(read.load.clone())
                    && operations.insert(read.operation.clone())
            })
            && ordered_reads
    }
}

/// A Checker-owned receipt for one directly-bound cast from a Values
/// snapshot element. The projection and cast are observations; the exact
/// Values snapshot remains the owner and must be explicitly dropped after
/// every receipted unpack. `payload_selection` records the same-root action
/// lineage through the Set which selected `conversion_to`. The result-owner
/// fields separately describe how the unpacked value itself is settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesUnpackReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    /// The exact `values(root)` action and its Map-root authority.
    pub(crate) operation: NodeId,
    pub(crate) source_local: ResolvedLocalId,
    pub(crate) source: ResourceId,
    pub(crate) source_node: NodeId,
    pub(crate) source_type: ResolvedTypeId,
    /// The direct `let snapshot = values(root)` binding and its exact type.
    pub(crate) snapshot_binding_point: NodeId,
    pub(crate) snapshot_binding: ResolvedLocalId,
    pub(crate) snapshot_type: ResolvedTypeId,
    /// The direct `let item = snapshot[index] as T` binding.
    pub(crate) unpack_binding_point: NodeId,
    pub(crate) unpack_binding: ResolvedLocalId,
    /// Exact index projection, index expression, and resolved index type.
    pub(crate) index_projection: NodeId,
    pub(crate) index_expression: NodeId,
    pub(crate) index_type: ResolvedTypeId,
    /// Present only when the exact index expression is a direct local Load.
    pub(crate) index_local: Option<ResolvedLocalId>,
    /// The exact checked cast from the Values list's DynamicAny element.
    pub(crate) cast: NodeId,
    pub(crate) conversion_kind: crate::core::ir::CheckedConversionKind,
    pub(crate) conversion_from: ResolvedTypeId,
    pub(crate) conversion_to: ResolvedTypeId,
    /// Ordered same-root Set/Remove/Clone lineage ending at the exact Set
    /// selected by sorted Values index replay.
    /// Empty only for an independently proven AlwaysOutOfBounds index.
    pub(crate) payload_selection: Vec<NodeId>,
    /// Static index disposition, independently rechecked by MIR lowering.
    pub(crate) index_disposition: OrdinaryMapOwnedAnyV2ValuesIndexDisposition,
    /// Owner disposition of the unpacked payload, independent of the Values
    /// snapshot settlement below.
    pub(crate) result_owner_settlement: OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind,
    /// Exact source point for `ExplicitWholeDrop`, or the root tail-expression
    /// anchor for compiler-owned String cleanup; absent for Copy payloads.
    pub(crate) result_owner_settlement_point: Option<NodeId>,
    /// Index of the exact root-level place in the Drop statement's places
    /// array. Present for explicit String, whole-value, and direct field-move
    /// settlements; absent for compiler cleanup and Copy payloads.
    pub(crate) result_owner_settlement_index: Option<usize>,
    /// Source-use commitment for the isolated whole-record Drop profile.
    /// `Some(empty)` proves that the Checker found no Load, assignment,
    /// capture, or non-root Drop touching this owner beyond its exact root
    /// whole-value Drop, and no opaque nested callable in this body.
    /// Nonempty sets are retained as
    /// rejection evidence; lowering must not erase an unsupported source use
    /// and thereby manufacture an isolated MIR owner. Copy/String results use
    /// `None` because they have different observation contracts.
    pub(crate) result_owner_whole_drop_unproved_uses: Option<BTreeSet<NodeId>>,
    /// Complete direct-local use census for the restricted explicit String
    /// Drop profile. It contains the exact comparison operand Loads and the
    /// exact Drop statement point, with no uses after that Drop.
    pub(crate) result_owner_string_drop_source_uses: Option<BTreeSet<NodeId>>,
    /// Root statement index of that exact explicit Drop, used to bind every
    /// admitted comparison to the pre-Drop lifetime.
    pub(crate) result_owner_string_drop_statement_order: Option<usize>,
    /// Exact Eq/Ne literal reads admitted before the explicit String Drop.
    pub(crate) result_owner_string_drop_reads:
        Vec<OrdinaryMapOwnedAnyV2ValuesUnpackTextReadReceipt>,
    /// Present only for the exact canonical Label `text: string` MoveProject
    /// profile. Both source-use sets are complete Checker census entries; the
    /// allowed projection/drop sites remain in the sets and are compared
    /// against the exact source identities by every downstream consumer.
    pub(crate) result_owner_field_move: Option<OrdinaryMapOwnedAnyV2ValuesUnpackFieldMoveReceipt>,
    /// Exact explicit settlement of the entire Values snapshot.
    pub(crate) settlement: OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: NodeId,
}

/// Checker proof that one Values i32 unpack uses the unique direct immutable
/// local binding initialized from integer literal zero. The use census is
/// complete across the resolved callable, so aliases, writes, captures, and
/// any other read of the index local invalidate the receipt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedAnyV2ValuesLocalIndexI32Receipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) binding_point: NodeId,
    pub(crate) binding: ResolvedLocalId,
    pub(crate) binding_mutable: bool,
    pub(crate) initializer: NodeId,
    pub(crate) literal_value: i64,
    pub(crate) index_type: ResolvedTypeId,
    pub(crate) index_expression: NodeId,
    pub(crate) unpack_cast: NodeId,
    pub(crate) unpack_binding_point: NodeId,
    pub(crate) unpack_binding: ResolvedLocalId,
    pub(crate) snapshot_binding: ResolvedLocalId,
    /// Complete direct-local source-load census; the only legal entry is the
    /// one index Load consumed by the exact Values projection.
    pub(crate) source_uses: BTreeSet<NodeId>,
}

impl PartialOrd for OrdinaryMapOwnedAnyV2ValuesUnpackReceipt {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdinaryMapOwnedAnyV2ValuesUnpackReceipt {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // CheckedConversionKind is intentionally not Ord in the public IR
        // schema. Its fieldless declaration order supplies a deterministic
        // local sort key while all receipt identities remain compared too.
        (
            (
                &self.profile_version,
                &self.operation,
                &self.source_local,
                &self.source,
                &self.source_node,
                &self.source_type,
            ),
            (
                &self.snapshot_binding_point,
                &self.snapshot_binding,
                &self.snapshot_type,
                &self.unpack_binding_point,
                &self.unpack_binding,
                &self.index_projection,
                &self.index_expression,
                &self.index_type,
            ),
            (
                &self.index_local,
                &self.cast,
                self.conversion_kind as u8,
                &self.conversion_from,
                &self.conversion_to,
                &self.payload_selection,
                self.index_disposition,
            ),
            (
                self.result_owner_settlement,
                &self.result_owner_settlement_point,
                &self.result_owner_settlement_index,
                &self.result_owner_whole_drop_unproved_uses,
                &self.result_owner_string_drop_source_uses,
                &self.result_owner_string_drop_statement_order,
                &self.result_owner_string_drop_reads,
                &self.result_owner_field_move,
                &self.settlement,
                &self.settlement_point,
            ),
        )
            .cmp(&(
                (
                    &other.profile_version,
                    &other.operation,
                    &other.source_local,
                    &other.source,
                    &other.source_node,
                    &other.source_type,
                ),
                (
                    &other.snapshot_binding_point,
                    &other.snapshot_binding,
                    &other.snapshot_type,
                    &other.unpack_binding_point,
                    &other.unpack_binding,
                    &other.index_projection,
                    &other.index_expression,
                    &other.index_type,
                ),
                (
                    &other.index_local,
                    &other.cast,
                    other.conversion_kind as u8,
                    &other.conversion_from,
                    &other.conversion_to,
                    &other.payload_selection,
                    other.index_disposition,
                ),
                (
                    other.result_owner_settlement,
                    &other.result_owner_settlement_point,
                    &other.result_owner_settlement_index,
                    &other.result_owner_whole_drop_unproved_uses,
                    &other.result_owner_string_drop_source_uses,
                    &other.result_owner_string_drop_statement_order,
                    &other.result_owner_string_drop_reads,
                    &other.result_owner_field_move,
                    &other.settlement,
                    &other.settlement_point,
                ),
            ))
    }
}

/// Schema/profile version for the owned-result typed getters over ordinary
/// Map roots. This version is intentionally disjoint from the owner lifecycle
/// v1 profile, MapRoot, and MapAnyOwner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedGetI32StringV1ProfileVersion {
    V1,
}

impl OrdinaryMapOwnedGetI32StringV1ProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::V1 => "ordinary-map-owned-get-i32-string-v1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedGetI32StringV1ActionKind {
    New,
    Clone,
    Set,
    Remove,
    Size,
    HasKey,
    GetI32,
    GetString,
    GetAny,
    /// Borrow the live Map root and publish an owned, deterministic snapshot
    /// of all heterogeneous values as `List<Any>`. The root remains live;
    /// every list element owns an independent ManagedAny token.
    Values,
    Drop,
}

impl OrdinaryMapOwnedGetI32StringV1ActionKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Clone => "clone",
            Self::Set => "set",
            Self::Remove => "remove",
            Self::Size => "size",
            Self::HasKey => "has_key",
            Self::GetI32 => "get_i32",
            Self::GetString => "get_string",
            Self::GetAny => "get_any",
            Self::Values => "values",
            Self::Drop => "drop",
        }
    }
}

/// One Checker-recognized operation in the independently sealed typed owned
/// Map getter lifecycle. `key` is the full typed expression identity; the
/// action never reduces a dynamic key to a literal or C-string prefix.
/// Getter result types are exactly Result<i32, i32> and Result<string, i32>;
/// the profile's runtime contract assigns Missing=1, TypeMismatch=2, and
/// Capacity=3 to the error payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetI32StringV1Action {
    pub(crate) profile_version: OrdinaryMapOwnedGetI32StringV1ProfileVersion,
    pub(crate) kind: OrdinaryMapOwnedGetI32StringV1ActionKind,
    pub(crate) point: NodeId,
    /// The local that owns the root for New/Set/Remove, or observes/drops it
    /// for the remaining actions.
    pub(crate) local: ResolvedLocalId,
    pub(crate) root: ResourceId,
    /// Full key expression identity for Set/Remove/HasKey/GetI32/GetString.
    pub(crate) key: Option<NodeId>,
    pub(crate) key_type: Option<ResolvedTypeId>,
    /// Direct local backing a borrowed String key, if the source key is an
    /// unprojected `Load`. The expression node above remains the semantic
    /// origin; this local is the exact storage identity used by MIR.
    pub(crate) key_local: Option<ResolvedLocalId>,
    /// Payload expression consumed by Set and its exact Checker type.
    pub(crate) value: Option<NodeId>,
    pub(crate) value_type: Option<ResolvedTypeId>,
    /// Exact Checker result type for observations, including the typed
    /// Result<i32, i32> / Result<string, i32> getter result.
    pub(crate) result_type: Option<ResolvedTypeId>,
    /// Persistent update source local, present only for Set/Remove.
    pub(crate) source_local: Option<ResolvedLocalId>,
    pub(crate) source: Option<ResourceId>,
}

/// Attempt vocabulary is deliberately wider than the admitted action set.
/// In particular, old tuple-valued `map_get` is distinct from both typed
/// getters and blocks this profile's receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedGetI32StringV1AttemptKind {
    New,
    Clone,
    UntypedGet,
    GetI32,
    GetString,
    Set,
    Remove,
    Size,
    HasKey,
    Drop,
    ScopeDrop,
    Keys,
    Values,
    FromList,
    MapLiteral,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedGetI32StringV1Attempt {
    pub(crate) kind: OrdinaryMapOwnedGetI32StringV1AttemptKind,
    pub(crate) point: NodeId,
    pub(crate) arguments: Vec<NodeId>,
    /// Direct unprojected Map root receiver, when the source operation has
    /// one.  Keeping the source local alongside the argument node ids lets a
    /// result receipt join the operation to the exact root edge without
    /// re-reading surface syntax in a consumer.
    pub(crate) receiver_local: Option<ResolvedLocalId>,
    /// Direct binding that receives a `map_get`/`values` result (or a new
    /// persistent root).  `None` is intentional for a return, a nested
    /// expression, or an expression statement; those forms are classified
    /// separately by the source result receipt collector.
    pub(crate) result_local: Option<ResolvedLocalId>,
}

/// Source-level settlement classification for an owned `map_get`/`values`
/// result.  This is deliberately separate from the Map root's synthetic
/// scope-drop action: a scope destructor is only scheduling evidence and does
/// not prove that the aggregate result owner was discharged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedGetResultSettlementKind {
    /// An explicit `drop(result)` statement consumed the result owner.
    ExplicitDrop,
    /// The result was moved into the callable's return/fault envelope.
    Return,
    /// The result was moved into another value by assignment or a direct
    /// consuming call boundary.
    Transfer,
    /// The result was captured by an aggregate/closure/task escape.
    Escape,
    /// The result had no source-level discharge; any cleanup is synthetic
    /// scope cleanup and is not a receipt.
    ScopeDrop,
    /// A source use was observed but no exact consuming edge was proved.
    Unsettled,
}

impl OrdinaryMapOwnedGetResultSettlementKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitDrop => "explicit-drop",
            Self::Return => "return",
            Self::Transfer => "transfer",
            Self::Escape => "escape",
            Self::ScopeDrop => "scope-drop",
            Self::Unsettled => "unsettled",
        }
    }

    pub(crate) const fn is_receipted(self) -> bool {
        matches!(
            self,
            Self::ExplicitDrop | Self::Return | Self::Transfer | Self::Escape
        )
    }
}

/// One Checker-owned source result receipt for a `GetAny` or `Values` action.
/// The result node remains distinct from its Map root and from any bound local
/// so a later MIR consumer cannot mistake root scope cleanup for result
/// ownership settlement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrdinaryMapOwnedGetResultReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedGetI32StringV1ProfileVersion,
    pub(crate) operation: NodeId,
    pub(crate) kind: OrdinaryMapOwnedGetI32StringV1ActionKind,
    pub(crate) result: NodeId,
    pub(crate) binding: Option<ResolvedLocalId>,
    pub(crate) settlement: OrdinaryMapOwnedGetResultSettlementKind,
    pub(crate) settlement_point: Option<NodeId>,
}

/// Checker-owned proof for the one typed `map_get_string` result match that
/// may outlive every ordinary Map root. This is deliberately more specific
/// than a generic Result consumer receipt: it binds a direct local result,
/// exhaustive `Ok(string)` / `Err(i32)` arms, observation and Drop of the Ok
/// payload, and scalar settlement of the Err payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetStringMatchReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedGetI32StringV1ProfileVersion,
    pub(crate) operation: NodeId,
    pub(crate) result_binding: ResolvedLocalId,
    pub(crate) match_node: NodeId,
    pub(crate) scrutinee: NodeId,
    pub(crate) ok_arm: NodeId,
    pub(crate) ok_variant: NodeId,
    pub(crate) ok_binding: ResolvedLocalId,
    pub(crate) ok_observation: NodeId,
    pub(crate) ok_observed_value: NodeId,
    pub(crate) ok_drop: NodeId,
    pub(crate) ok_result: NodeId,
    pub(crate) ok_body: NodeId,
    pub(crate) err_arm: NodeId,
    pub(crate) err_variant: NodeId,
    pub(crate) err_binding: ResolvedLocalId,
    pub(crate) err_value: NodeId,
    pub(crate) err_body: NodeId,
}

/// Version one keeps all owned Map operations in the entry block and closes
/// every root before control flow branches. Version two carries exact live
/// root facts on every edge of a supported acyclic CFG. Cycles still require
/// a separate fixed-point proof and remain rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedGetI32StringV1CfgProfileVersion {
    EntryClosedTailV1,
    AcyclicPerEdgeRootsV2,
    ResultBranchV3,
}

impl OrdinaryMapOwnedGetI32StringV1CfgProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::EntryClosedTailV1 => "ordinary-map-owned-get-entry-closed-tail-cfg-v1",
            Self::AcyclicPerEdgeRootsV2 => "ordinary-map-owned-get-acyclic-per-edge-roots-cfg-v2",
            Self::ResultBranchV3 => "ordinary-map-owned-get-result-branch-cfg-v3",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetI32StringV1CfgBlock {
    pub(crate) points: Vec<(NodeId, CfgPointKind)>,
    pub(crate) terminator: Terminator,
}

/// Checker authority for one typed `Result<_, i32>` Map operation match.
/// Root creation is attached to the Ok edge; Err carries only the source
/// roots that remain live on that path. The path records bind each arm to its
/// exact edge region, join endpoint, carried root set, and explicit drops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetResultBranchPathReceipt {
    pub(crate) arm_entry_edge: EdgeId,
    pub(crate) arm_entry: BasicBlockId,
    pub(crate) join: BasicBlockId,
    pub(crate) path_edges: BTreeSet<EdgeId>,
    pub(crate) exit_edges: BTreeSet<EdgeId>,
    pub(crate) roots_at_entry: BTreeSet<String>,
    pub(crate) roots_at_exit: BTreeSet<String>,
    pub(crate) explicit_root_drops: Vec<NodeId>,
}

/// Exact source and CFG binding for a New, Set, typed GetI32, or typed
/// GetString Result match. `output_root` is introduced only on `ok.path` for
/// New/Set; Get operations preserve `input_root`, while owned String payloads
/// must be explicitly dropped on Ok.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetI32StringV1ResultBranchReceipt {
    pub(crate) kind: OrdinaryMapOwnedGetI32StringV1ActionKind,
    pub(crate) operation_point: NodeId,
    pub(crate) result_node: NodeId,
    pub(crate) result_binding: Option<ResolvedLocalId>,
    pub(crate) result_type: ResolvedTypeId,
    pub(crate) match_node: NodeId,
    pub(crate) scrutinee_node: NodeId,
    pub(crate) ok_arm: NodeId,
    pub(crate) ok_variant: NodeId,
    pub(crate) ok_pattern: NodeId,
    pub(crate) ok_binding: Option<ResolvedLocalId>,
    pub(crate) ok_payload_type: ResolvedTypeId,
    pub(crate) err_arm: NodeId,
    pub(crate) err_variant: NodeId,
    pub(crate) err_pattern: NodeId,
    pub(crate) err_binding: Option<ResolvedLocalId>,
    pub(crate) err_payload_type: ResolvedTypeId,
    pub(crate) input_root: Option<ResolvedLocalId>,
    pub(crate) output_root: Option<ResolvedLocalId>,
    pub(crate) ok_owned_payload_drop: Option<NodeId>,
    pub(crate) ok: OrdinaryMapOwnedGetResultBranchPathReceipt,
    pub(crate) err: OrdinaryMapOwnedGetResultBranchPathReceipt,
}

/// Checker-owned exact control-flow commitment for a typed owned-get Map
/// candidate. `reachable_blocks` and `reachable_edges` are the graph the
/// validator later projects onto MIR; attempts/actions close the source Map
/// operation inventory and v2 root facts prove path-local ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetI32StringV1CfgReceipt {
    /// The callable whose checker-derived body and CFG were scanned. This is
    /// explicit so a closure receipt cannot be replayed as its enclosing
    /// function's authority (or vice versa).
    pub(crate) owner: NodeId,
    pub(crate) profile_version: OrdinaryMapOwnedGetI32StringV1CfgProfileVersion,
    pub(crate) entry: BasicBlockId,
    pub(crate) map_operation_block: BasicBlockId,
    pub(crate) reachable_blocks: BTreeMap<BasicBlockId, OrdinaryMapOwnedGetI32StringV1CfgBlock>,
    pub(crate) reachable_edges: BTreeMap<EdgeId, CfgEdge>,
    pub(crate) attempts: BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
    pub(crate) action_points: BTreeSet<NodeId>,
    /// Independent source-level owner receipts for `GetAny` and `Values`
    /// aggregate results.  A `ScopeDrop` entry is retained as evidence of a
    /// missing explicit consumer and must never be upgraded by a backend.
    pub(crate) result_receipts: Vec<OrdinaryMapOwnedGetResultReceipt>,
    /// Exact source ownership receipt for direct String-get Result matching.
    /// This never authorizes arbitrary Result matches.
    pub(crate) string_match_receipts: Vec<OrdinaryMapOwnedGetStringMatchReceipt>,
    /// Edge-local Result producer and typed getter obligations. This is
    /// populated only by `ResultBranchV3`; all older profiles keep it empty.
    pub(crate) result_branches: Vec<OrdinaryMapOwnedGetI32StringV1ResultBranchReceipt>,
    /// Exact path-sensitive Map-root facts at each reachable source block.
    /// The v2 profile requires these maps to cover all blocks/edges and the
    /// consumer independently replays every receipted action.
    pub(crate) live_roots_in: BTreeMap<BasicBlockId, BTreeSet<String>>,
    pub(crate) live_roots_out: BTreeMap<BasicBlockId, BTreeSet<String>>,
    pub(crate) live_roots_on_edge: BTreeMap<EdgeId, BTreeSet<String>>,
}

impl CanonicalActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Introduce => "introduce",
            Self::Move => "move",
            Self::Drop => "drop",
            Self::Return => "return",
            Self::TransferSession => "transfer_session",
            Self::TransferChild => "transfer_child",
            Self::BorrowShared => "borrow_shared",
            Self::BorrowMut => "borrow_mut",
            Self::BorrowEnd => "borrow_end",
        }
    }
}

impl CanonicalResourceAction {
    /// Display name for the resource this action operates on.
    pub fn resource_display(&self) -> String {
        self.source
            .as_ref()
            .or(self.target.as_ref())
            .map(Place::display)
            .unwrap_or_else(|| self.resource.0 .0.clone())
    }
}

/// A branch-merge observation computed from `ResourceAnalysis` in/out states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMerge {
    pub resource: String,
    pub then_state: Availability,
    pub else_state: Availability,
    pub merged_state: Availability,
    pub span: Span,
}

fn observer_attempt_covers_receipt(
    attempt: &OrdinaryMapOwnerObserverAttempt,
    receipt: &OrdinaryMapOwnerObserverReceipt,
) -> bool {
    receipt.profile_version == OrdinaryMapOwnerObserverProfileVersion::V1
        && attempt.owner == receipt.owner
        && attempt.callsite == receipt.callsite
        && attempt.builtin == receipt.builtin
        && attempt.arguments.as_slice() == [receipt.argument.clone()]
        && attempt.argument_types.as_slice() == [receipt.borrow_type.clone()]
        && attempt.argument_locals.as_slice() == [Some(receipt.root_local.clone())]
        && attempt.result_type == receipt.result_type
        && attempt.direct_effects == receipt.direct_effects
        && attempt.typed_call_effects == receipt.typed_call_effects
}

pub(crate) fn ordinary_map_owner_observer_effects(builtin: &str) -> Option<Vec<EffectId>> {
    if !matches!(builtin, "to_json" | "println") {
        return None;
    }
    crate::core::effects::builtin_effects(builtin)
        .to_vec()
        .into_iter()
        .map(|effect| EffectId::new(effect).ok())
        .collect()
}

fn ordinary_map_owner_observer_contract_is_valid(
    receipt: &OrdinaryMapOwnerObserverReceipt,
) -> bool {
    let Some(direct_effects) = ordinary_map_owner_observer_effects(receipt.builtin.as_str()) else {
        return false;
    };
    let expected_result = match receipt.builtin.as_str() {
        "to_json" => crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String),
        "println" => crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Unit),
        _ => return false,
    };
    receipt.result_type == expected_result
        && receipt.direct_effects == direct_effects
        && (receipt.typed_call_effects.is_empty() || receipt.typed_call_effects == direct_effects)
}

fn map_any_v1_action_covers_ordinary_attempt(
    analysis: &ResourceAnalysis,
    action: &MapAnyOwnerAction,
    attempt: &OrdinaryMapOwnerLifecycleAttempt,
) -> bool {
    use MapAnyOwnerActionKind as MapAny;
    use OrdinaryMapOwnerLifecycleActionKind as Ordinary;
    use OrdinaryMapOwnerLifecycleAttemptKind as Attempt;

    if action.profile_version != MapAnyOwnerProfileVersion::V1 || action.point != attempt.point {
        return false;
    }
    let (map_any_kind, ordinary_kind) = match attempt.kind {
        Attempt::New => (MapAny::New, Ordinary::New),
        Attempt::Set => (MapAny::Set, Ordinary::Set),
        Attempt::Remove => (MapAny::Remove, Ordinary::Remove),
        Attempt::Size => (MapAny::Size, Ordinary::Size),
        Attempt::HasKey => (MapAny::Contains, Ordinary::HasKey),
        Attempt::Drop => (MapAny::Drop, Ordinary::Drop),
        Attempt::Clone
        | Attempt::ScopeDrop
        | Attempt::Get
        | Attempt::Keys
        | Attempt::Values
        | Attempt::FromList
        | Attempt::MapLiteral => return false,
    };
    if action.kind != map_any_kind {
        return false;
    }
    let map_any_root_matches_local =
        action.root.0 .0 == format!("map-any-owner-v1:{}", action.local.0 .0);
    let map_any_source_matches_local = match (&action.source_local, &action.source) {
        (Some(local), Some(source)) => source.0 .0 == format!("map-any-owner-v1:{}", local.0 .0),
        (None, None) => true,
        _ => false,
    };
    if !map_any_root_matches_local || !map_any_source_matches_local {
        return false;
    }

    let arguments = &attempt.arguments;
    let is_temporary_local_for_source = |local: &ResolvedLocalId, source: &NodeId| {
        local.0 .0 == format!("map-root-temporary:{}", source.0)
    };
    let attempt_source_matches = |local: &ResolvedLocalId| {
        attempt.receiver_local.as_ref() == Some(local)
            || (attempt.receiver_local.is_none()
                && arguments
                    .first()
                    .is_some_and(|source| is_temporary_local_for_source(local, source)))
    };
    let map_any_operands_match = match attempt.kind {
        Attempt::New => {
            arguments.is_empty()
                && attempt.receiver_local.is_none()
                && (attempt.result_local.as_ref() == Some(&action.local)
                    || (attempt.result_local.is_none()
                        && is_temporary_local_for_source(&action.local, &attempt.point)))
                && action.key.is_none()
                && action.key_expression.is_none()
                && action.value.is_none()
                && action.value_type.is_none()
                && action.source_local.is_none()
                && action.source.is_none()
        }
        Attempt::Set => {
            arguments.len() == 3
                && action
                    .source_local
                    .as_ref()
                    .is_some_and(attempt_source_matches)
                && attempt.result_local.as_ref() == Some(&action.local)
                && action.key.is_some()
                && action.key_expression.as_ref() == arguments.get(1)
                && action.value.as_ref() == arguments.get(2)
                && action.value.is_some()
                && action.value_type.is_some()
                && action.source.is_some()
        }
        Attempt::Remove => {
            arguments.len() == 2
                && action
                    .source_local
                    .as_ref()
                    .is_some_and(attempt_source_matches)
                && attempt.result_local.as_ref() == Some(&action.local)
                && action.key.is_some()
                && action.key_expression.as_ref() == arguments.get(1)
                && action.value.is_none()
                && action.value_type.is_none()
                && action.source.is_some()
        }
        Attempt::Size => {
            arguments.len() == 1
                && attempt_source_matches(&action.local)
                && action.key.is_none()
                && action.key_expression.is_none()
                && action.value.is_none()
                && action.value_type.is_none()
                && action.source_local.is_none()
                && action.source.is_none()
        }
        Attempt::HasKey => {
            arguments.len() == 2
                && attempt.receiver_local.as_ref() == Some(&action.local)
                && action.key.is_some()
                && action.key_expression.as_ref() == arguments.get(1)
                && action.value.is_none()
                && action.value_type.is_none()
                && action.source_local.is_none()
                && action.source.is_none()
        }
        Attempt::Drop => {
            arguments.len() == 1
                && action.local.0 == arguments[0]
                && attempt.receiver_local.as_ref() == Some(&action.local)
                && action.key.is_none()
                && action.key_expression.is_none()
                && action.value.is_none()
                && action.value_type.is_none()
                && action.source_local.is_none()
                && action.source.is_none()
        }
        _ => false,
    };
    if !map_any_operands_match {
        return false;
    }

    let mut ordinary_actions = analysis
        .ordinary_map_owner_lifecycle_v1_actions
        .iter()
        .filter(|ordinary_action| {
            ordinary_action.profile_version == OrdinaryMapOwnerLifecycleProfileVersion::V1
                && ordinary_action.point == attempt.point
                && ordinary_action.kind == ordinary_kind
        });
    let Some(ordinary_action) = ordinary_actions.next() else {
        // Values such as an inline scalar FFI result are valid MapAnyOwner
        // payloads while deliberately outside the ordinary lifecycle action
        // profile. The independent receipt and exact source attempt still
        // form a complete join.
        return true;
    };
    let ordinary_root_matches_local = ordinary_action.root.0 .0
        == format!(
            "ordinary-map-owner-lifecycle-i32-string-v1:{}",
            ordinary_action.local.0 .0
        );
    let ordinary_source_matches_local =
        match (&ordinary_action.source_local, &ordinary_action.source) {
            (Some(local), Some(source)) => {
                source.0 .0 == format!("ordinary-map-owner-lifecycle-i32-string-v1:{}", local.0 .0)
            }
            (None, None) => true,
            _ => false,
        };
    let ordinary_operands_match = match attempt.kind {
        Attempt::New => {
            arguments.is_empty()
                && ordinary_action.key.is_none()
                && ordinary_action.key_type.is_none()
                && ordinary_action.value.is_none()
                && ordinary_action.value_type.is_none()
                && ordinary_action.source_local.is_none()
                && ordinary_action.source.is_none()
        }
        Attempt::Set => {
            arguments.len() == 3
                && ordinary_action.key.as_ref() == arguments.get(1)
                && ordinary_action.key_type.is_some()
                && ordinary_action.value.as_ref() == arguments.get(2)
                && ordinary_action.value_type.is_some()
                && ordinary_action.source_local == attempt.receiver_local
                && ordinary_action.source.is_some()
        }
        Attempt::Remove => {
            arguments.len() == 2
                && ordinary_action.key.as_ref() == arguments.get(1)
                && ordinary_action.key_type.is_some()
                && ordinary_action.value.is_none()
                && ordinary_action.value_type.is_none()
                && ordinary_action.source_local == attempt.receiver_local
                && ordinary_action.source.is_some()
        }
        Attempt::Size => {
            arguments.len() == 1
                && ordinary_action.key.is_none()
                && ordinary_action.key_type.is_none()
                && ordinary_action.value.is_none()
                && ordinary_action.value_type.is_none()
                && ordinary_action.source_local.is_none()
                && ordinary_action.source.is_none()
        }
        Attempt::HasKey => {
            arguments.len() == 2
                && ordinary_action.key.as_ref() == arguments.get(1)
                && ordinary_action.key_type.is_some()
                && ordinary_action.value.is_none()
                && ordinary_action.value_type.is_none()
                && ordinary_action.source_local.is_none()
                && ordinary_action.source.is_none()
        }
        Attempt::Drop => {
            arguments.len() == 1
                && ordinary_action.local.0 == arguments[0]
                && ordinary_action.key.is_none()
                && ordinary_action.key_type.is_none()
                && ordinary_action.value.is_none()
                && ordinary_action.value_type.is_none()
                && ordinary_action.source_local.is_none()
                && ordinary_action.source.is_none()
        }
        _ => false,
    };
    ordinary_actions.next().is_none()
        && action.local == ordinary_action.local
        && action.source_local == ordinary_action.source_local
        && action.key_expression == ordinary_action.key
        && action.value == ordinary_action.value
        && action.value_type == ordinary_action.value_type
        && ordinary_root_matches_local
        && ordinary_source_matches_local
        && ordinary_operands_match
}

impl ResourceAnalysis {
    /// TypeDesc registration asks this helper instead of consulting the
    /// legacy MapRoot ledger. Only a sealed MapAnyOwner v1 `New` receipt can
    /// authorize its synthetic descriptor.
    pub(crate) fn has_map_any_owner_v1_receipt(&self) -> bool {
        self.map_any_owner_actions.iter().any(|action| {
            action.profile_version == MapAnyOwnerProfileVersion::V1
                && action.kind == MapAnyOwnerActionKind::New
        })
    }

    /// Ordinary Map route admission must consult this profile's own sealed
    /// Checker receipt, never a MapRoot or MapAnyOwner action.
    pub(crate) fn has_ordinary_map_owner_lifecycle_v1_receipt(&self) -> bool {
        self.ordinary_map_owner_lifecycle_v1_actions
            .iter()
            .any(|action| {
                action.profile_version == OrdinaryMapOwnerLifecycleProfileVersion::V1
                    && action.kind == OrdinaryMapOwnerLifecycleActionKind::New
            })
    }

    /// Whether this callable contains any recognized ordinary Map operation
    /// attempt, including shapes that do not receive lifecycle actions. This
    /// prevents a partial ordinary Map from borrowing a legacy MapRoot/V1
    /// receipt for its supported-looking subset.
    pub(crate) fn has_ordinary_map_owner_lifecycle_v1_attempts(&self) -> bool {
        !self
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .is_empty()
    }

    /// Whether a direct Map observer was found by the independent Checker
    /// census. This is only a candidate signal; consumers must also require
    /// `ordinary_map_owner_observer_v1_receipts_are_complete`.
    pub(crate) fn has_ordinary_map_owner_observer_v1_attempts(&self) -> bool {
        !self.ordinary_map_owner_observer_v1_attempts.is_empty()
    }

    /// Whether the observer census is exactly covered by Checker receipts.
    /// Each callsite must match one receipt and each receipt must match one
    /// attempt; an unsupported shape or duplicate/forged receipt fails closed.
    pub(crate) fn ordinary_map_owner_observer_v1_receipts_are_complete(&self) -> bool {
        if self.ordinary_map_owner_observer_v1_attempts.len()
            != self.ordinary_map_owner_observer_v1_receipts.len()
        {
            return false;
        }
        self.ordinary_map_owner_observer_v1_attempts
            .iter()
            .all(|attempt| self.ordinary_map_owner_observer_v1_covers_attempt(attempt))
            && self
                .ordinary_map_owner_observer_v1_receipts
                .iter()
                .all(|receipt| {
                    receipt.profile_version == OrdinaryMapOwnerObserverProfileVersion::V1
                        && self
                            .ordinary_map_owner_observer_v1_attempts
                            .iter()
                            .filter(|attempt| observer_attempt_covers_receipt(attempt, receipt))
                            .count()
                            == 1
                        && receipt.root.0 .0
                            == format!(
                                "ordinary-map-owner-lifecycle-i32-string-v1:{}",
                                receipt.root_local.0 .0
                            )
                        && receipt.root_type == receipt.borrow_type
                        && ordinary_map_owner_observer_contract_is_valid(receipt)
                        && self.has_ordinary_map_owner_lifecycle_v1_receipt()
                        && self
                            .ordinary_map_owner_lifecycle_v1_actions
                            .iter()
                            .any(|action| {
                                action.profile_version
                                    == OrdinaryMapOwnerLifecycleProfileVersion::V1
                                    && action.local == receipt.root_local
                                    && action.root == receipt.root
                                    && matches!(
                                        action.kind,
                                        OrdinaryMapOwnerLifecycleActionKind::New
                                            | OrdinaryMapOwnerLifecycleActionKind::Set
                                            | OrdinaryMapOwnerLifecycleActionKind::Remove
                                    )
                            })
                })
    }

    /// Whether exactly one Checker observer receipt covers this independent
    /// source attempt.
    pub(crate) fn ordinary_map_owner_observer_v1_covers_attempt(
        &self,
        attempt: &OrdinaryMapOwnerObserverAttempt,
    ) -> bool {
        self.ordinary_map_owner_observer_v1_receipts
            .iter()
            .filter(|receipt| observer_attempt_covers_receipt(attempt, receipt))
            .count()
            == 1
    }

    /// Whether one exact MapAnyOwner v1 action covers this independently
    /// recorded ordinary Map source attempt. Unsupported attempt kinds and
    /// ambiguous duplicate actions remain uncovered.
    pub(crate) fn map_any_owner_v1_covers_ordinary_attempt(
        &self,
        attempt: &OrdinaryMapOwnerLifecycleAttempt,
    ) -> bool {
        if attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::ScopeDrop {
            return self.ordinary_scope_drop_is_settled_by_map_any_owner_v1(attempt);
        }
        let mut matching_actions = self
            .map_any_owner_actions
            .iter()
            .filter(|action| map_any_v1_action_covers_ordinary_attempt(self, action, attempt));
        matching_actions.next().is_some() && matching_actions.next().is_none()
    }

    /// Typed owned-get admission consults only its own sealed Checker receipt.
    /// A New action in another Map profile cannot satisfy this predicate.
    pub(crate) fn has_ordinary_map_owned_get_i32_string_v1_receipt(&self) -> bool {
        self.ordinary_map_owned_get_i32_string_v1_actions
            .iter()
            .any(|action| {
                action.profile_version == OrdinaryMapOwnedGetI32StringV1ProfileVersion::V1
                    && action.kind == OrdinaryMapOwnedGetI32StringV1ActionKind::New
            })
    }

    /// Whether this callable carries the exact edge-local ResultBranchV3
    /// owned-get receipt. CLI route selection uses this read-only predicate to
    /// choose the receipt-bound adapter; the adapter still validates the full
    /// CheckedProgram/MIR pair before minting any backend anchor.
    pub fn has_ordinary_map_owned_get_result_branch_v3_receipt(&self) -> bool {
        self.ordinary_map_owned_get_i32_string_v1_cfg_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt.profile_version
                    == OrdinaryMapOwnedGetI32StringV1CfgProfileVersion::ResultBranchV3
                    && !receipt.result_branches.is_empty()
            })
    }

    /// Whether this callable contains a sealed ordinary Map/Any owner-v2
    /// construction receipt.  Consumers must pair this with the operation
    /// census and the profile marker; a v1 New action is never sufficient.
    pub(crate) fn has_ordinary_map_owned_any_v2_receipt(&self) -> bool {
        self.ordinary_map_owned_any_v2_actions.iter().any(|action| {
            action.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
                && action.kind == OrdinaryMapOwnedAnyV2ActionKind::New
        })
    }

    /// Reports whether Checker recognized the pending ordinary Map/Any
    /// NewResult source shape. This is source evidence only: it grants no MIR
    /// admission, backend route, or execution capability.
    pub fn has_pending_ordinary_map_owned_any_v2_new_result_source_receipt(&self) -> bool {
        self.ordinary_map_owned_any_v2_new_result_receipt.is_some()
    }

    /// Returns the independent Checker receipt for Result-valued
    /// `map_get_any`; this is never the tuple-valued `map_get` proof.
    pub(crate) fn ordinary_map_set_get_any_result_i32_lineage_receipt(
        &self,
    ) -> Option<&OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt> {
        self.ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt
            .as_ref()
    }

    /// Returns the independent Checker receipt for the exact ordinary Map
    /// New/Set<String>/GetString lineage. This source receipt does not grant
    /// execution by itself; MIR route selection must still materialize and
    /// validate its own consumer receipt.
    pub(crate) fn ordinary_map_set_get_string_lineage_receipt(
        &self,
    ) -> Option<&OrdinaryMapOwnedAnyV2SetGetStringLineageReceipt> {
        self.ordinary_map_owned_any_v2_set_get_string_lineage_receipt
            .as_ref()
    }

    /// Reports whether the Checker recorded the exact Map
    /// New/Set<String>/GetString source receipt. This observation grants no
    /// MIR route or backend capability; consumers must materialize and
    /// validate their own receipt before using the source evidence.
    pub fn has_pending_ordinary_map_set_get_string_lineage_source_receipt(&self) -> bool {
        self.ordinary_map_set_get_string_lineage_receipt().is_some()
    }

    /// Whether the older MapAnyOwner v1 receipt independently covers every
    /// recognized ordinary Map operation attempt in this callable. This is a
    /// narrow compatibility bridge for the pre-existing consuming-root
    /// island; any unsupported operation (Get, Values, Map literal, dynamic
    /// root use, or future call kind) blocks that route.
    pub(crate) fn ordinary_map_attempts_fully_covered_by_map_any_owner_v1(&self) -> bool {
        use OrdinaryMapOwnerLifecycleAttemptKind as Attempt;

        if self
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .is_empty()
            || self.map_any_owner_actions.is_empty()
            || self
                .map_any_owner_actions
                .iter()
                .any(|action| action.profile_version != MapAnyOwnerProfileVersion::V1)
        {
            return false;
        }

        let mut unmatched_actions = self.map_any_owner_actions.iter().collect::<Vec<_>>();
        for attempt in &self.ordinary_map_owner_lifecycle_v1_operation_attempts {
            if attempt.kind == Attempt::ScopeDrop {
                if !self.ordinary_scope_drop_is_settled_by_map_any_owner_v1(attempt) {
                    return false;
                }
                continue;
            }
            let Some(index) = unmatched_actions.iter().position(|action| {
                map_any_v1_action_covers_ordinary_attempt(self, action, attempt)
            }) else {
                return false;
            };
            unmatched_actions.remove(index);
        }
        unmatched_actions.retain(|action| {
            !self.ordinary_map_any_unmatched_drop_is_settled_by_map_any_owner_v1(action)
        });
        unmatched_actions.is_empty()
    }

    /// Every MapAnyOwner v1 action is covered by exactly one same-point
    /// ordinary source attempt. Combined with the forward predicate above,
    /// this proves a non-vacuous one-to-one action census for the profile
    /// arbitration exception.
    pub(crate) fn map_any_owner_v1_actions_are_within_ordinary_map_profile(&self) -> bool {
        if self.map_any_owner_actions.is_empty()
            || self
                .map_any_owner_actions
                .iter()
                .any(|action| action.profile_version != MapAnyOwnerProfileVersion::V1)
        {
            return false;
        }
        let mut unmatched_attempts = self
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .iter()
            .filter(|attempt| attempt.kind != OrdinaryMapOwnerLifecycleAttemptKind::ScopeDrop)
            .collect::<Vec<_>>();
        for action in &self.map_any_owner_actions {
            if self.ordinary_map_any_unmatched_drop_is_settled_by_map_any_owner_v1(action) {
                continue;
            }
            let Some(index) = unmatched_attempts.iter().position(|attempt| {
                map_any_v1_action_covers_ordinary_attempt(self, action, attempt)
            }) else {
                return false;
            };
            unmatched_attempts.remove(index);
        }
        true
    }

    /// The ordinary lifecycle census does not represent every exact cleanup
    /// shape that remains visible in the independent MapAnyOwner v1 ledger:
    /// (1) a synthetic Drop after a borrowed read from a direct temporary
    /// root such as `map_size(map_new())`, (2) cleanup of the local result of
    /// `map_set(map_new(), ...)`/`map_remove(map_new(), ...)` when that
    /// temporary-root source prevents the ordinary lifecycle walk from
    /// reaching the Drop, and (3) an explicit Drop of a direct Set/Remove
    /// result when its payload shape (for example an inline scalar FFI call)
    /// withholds the ordinary action ledger. Keep each bridge tied to one
    /// exact MapAnyOwner result local and a unique ordinary producer attempt;
    /// none may authorize an unrelated local or a second cleanup.
    fn ordinary_map_any_unmatched_drop_is_settled_by_map_any_owner_v1(
        &self,
        action: &MapAnyOwnerAction,
    ) -> bool {
        if action.profile_version != MapAnyOwnerProfileVersion::V1
            || action.kind != MapAnyOwnerActionKind::Drop
            || action.key.is_some()
            || action.key_expression.is_some()
            || action.value.is_some()
            || action.value_type.is_some()
            || action.source_local.is_some()
            || action.source.is_some()
        {
            return false;
        }
        let matching_drops = self
            .map_any_owner_actions
            .iter()
            .filter(|candidate| {
                candidate.profile_version == MapAnyOwnerProfileVersion::V1
                    && candidate.kind == MapAnyOwnerActionKind::Drop
                    && candidate.local == action.local
            })
            .collect::<Vec<_>>();
        if matching_drops.len() != 1 || matching_drops[0] != action {
            return false;
        }
        if let Some(temporary_source) = action.local.0 .0.strip_prefix("map-root-temporary:") {
            let mut matching_attempts = self
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .iter()
                .filter(|attempt| {
                    attempt.point == action.point
                        && matches!(
                            attempt.kind,
                            OrdinaryMapOwnerLifecycleAttemptKind::Size
                                | OrdinaryMapOwnerLifecycleAttemptKind::HasKey
                        )
                        && attempt.receiver_local.is_none()
                        && attempt
                            .arguments
                            .first()
                            .is_some_and(|source| source.0.as_str() == temporary_source)
                });
            return matching_attempts.next().is_some() && matching_attempts.next().is_none();
        }

        let mut producers = self.map_any_owner_actions.iter().filter(|producer| {
            producer.profile_version == MapAnyOwnerProfileVersion::V1
                && matches!(
                    producer.kind,
                    MapAnyOwnerActionKind::Set | MapAnyOwnerActionKind::Remove
                )
                && producer.local == action.local
                && producer.point != action.point
        });
        let Some(producer) = producers.next() else {
            return false;
        };
        if producers.next().is_some() {
            return false;
        }

        let mut attempts = self
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .iter()
            .filter(|attempt| {
                attempt.point == producer.point
                    && attempt.result_local.as_ref() == Some(&producer.local)
                    && matches!(
                        (producer.kind, attempt.kind),
                        (
                            MapAnyOwnerActionKind::Set,
                            OrdinaryMapOwnerLifecycleAttemptKind::Set
                        ) | (
                            MapAnyOwnerActionKind::Remove,
                            OrdinaryMapOwnerLifecycleAttemptKind::Remove
                        )
                    )
                    && map_any_v1_action_covers_ordinary_attempt(self, producer, attempt)
            });
        attempts.next().is_some() && attempts.next().is_none()
    }

    /// Preserve the older consume-update optimization only when its sealed
    /// ledger has already transferred the exact local's root obligation.
    /// A scope destructor is scheduling evidence, distinct from an explicit
    /// source Drop; it cannot manufacture an older operation receipt.
    pub(crate) fn ordinary_scope_drop_is_settled_by_map_any_owner_v1(
        &self,
        attempt: &OrdinaryMapOwnerLifecycleAttempt,
    ) -> bool {
        if attempt.kind != OrdinaryMapOwnerLifecycleAttemptKind::ScopeDrop
            || attempt.arguments.len() != 1
        {
            return false;
        }
        self.map_any_owner_actions.iter().any(|action| {
            action.profile_version == MapAnyOwnerProfileVersion::V1
                && matches!(
                    action.kind,
                    MapAnyOwnerActionKind::Set | MapAnyOwnerActionKind::Remove
                )
                && action
                    .source_local
                    .as_ref()
                    .is_some_and(|local| local.0 == attempt.arguments[0])
        })
    }

    /// Exact primitive payload type ids carried by sealed MapAnyOwner v1 Set
    /// receipts. This helper keeps consumers from reconstructing the payload
    /// identity from source syntax.
    #[cfg(test)]
    pub(crate) fn map_any_owner_v1_set_payload_types(&self) -> Vec<ResolvedTypeId> {
        self.map_any_owner_actions
            .iter()
            .filter(|action| {
                action.profile_version == MapAnyOwnerProfileVersion::V1
                    && action.kind == MapAnyOwnerActionKind::Set
            })
            .filter_map(|action| action.value_type.clone())
            .collect()
    }

    /// Count actions of a given canonical kind.
    pub fn action_count(&self, kind: CanonicalActionKind) -> usize {
        self.actions.iter().filter(|a| a.kind == kind).count()
    }

    /// Sorted, deduplicated display names of resources (excluding Read/Write).
    pub fn resources(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .actions
            .iter()
            .filter(|a| {
                !matches!(
                    a.kind,
                    CanonicalActionKind::Read | CanonicalActionKind::Write
                )
            })
            .map(|a| a.resource_display())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Compute branch-merge observations from in/out states and the CFG.
    pub fn branch_merges(&self, cfg: &CallableCfg) -> Vec<BranchMerge> {
        let mut branch_merges = Vec::new();
        for (block, incoming) in &self.in_states {
            let predecessors = cfg
                .predecessors(block)
                .into_iter()
                .filter(|edge| cfg.reachable.contains(&edge.from))
                .filter_map(|edge| self.out_states.get(&edge.from))
                .collect::<Vec<_>>();
            if predecessors.len() < 2 {
                continue;
            }
            let resources = predecessors
                .iter()
                .flat_map(|state| state.keys().cloned())
                .collect::<std::collections::BTreeSet<_>>();
            for resource in resources {
                let state = |fact: Option<&ResourceFact>| {
                    fact.map(|fact| fact.availability)
                        .unwrap_or(Availability::MaybeConsumed)
                };
                let name = incoming
                    .get(&resource)
                    .and_then(|fact| fact.owner.as_ref())
                    .or_else(|| {
                        predecessors.iter().find_map(|facts| {
                            facts.get(&resource).and_then(|fact| fact.owner.as_ref())
                        })
                    })
                    .map(Place::display)
                    .unwrap_or_else(|| {
                        self.actions
                            .iter()
                            .find(|action| action.resource == resource)
                            .and_then(|action| action.source.as_ref())
                            .map(Place::display)
                            .unwrap_or_else(|| resource.0 .0.clone())
                    });
                branch_merges.push(BranchMerge {
                    resource: name,
                    then_state: state(predecessors[0].get(&resource)),
                    else_state: state(predecessors[1].get(&resource)),
                    merged_state: state(incoming.get(&resource)),
                    span: cfg
                        .block(block)
                        .map(|block| block.source.span)
                        .unwrap_or(Span::UNKNOWN),
                });
            }
        }
        branch_merges.sort_by(|left, right| {
            (left.span.start_line, left.span.start_col, &left.resource).cmp(&(
                right.span.start_line,
                right.span.start_col,
                &right.resource,
            ))
        });
        branch_merges
    }

    /// Whether any branch merge results in MaybeConsumed.
    pub fn has_maybe_consumed_merge(&self, cfg: &CallableCfg) -> bool {
        self.branch_merges(cfg)
            .iter()
            .any(|merge| merge.merged_state == Availability::MaybeConsumed)
    }
}

#[cfg(test)]
mod map_any_owner_unmatched_drop_tests {
    use super::*;

    fn checked_map_analysis(remove_after_set: bool) -> ResourceAnalysis {
        let tail = if remove_after_set {
            r#"
    let inserted = map_set(root, "answer", 41)
    let updated = map_remove(inserted, "answer")
    drop(updated)
    0
"#
        } else {
            r#"
    let updated = map_set(root, "answer", 41)
    drop(updated)
    0
"#
        };
        let source = format!(
            r#"
func main() -> i32 {{
    let root = map_new()
    {tail}
}}
"#
        );
        let tokens = crate::lexer::Lexer::new(&source)
            .tokenize()
            .expect("MapAnyOwner fixture lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("MapAnyOwner fixture parses");
        let checked = crate::core::check_program(&file)
            .unwrap_or_else(|errors| panic!("MapAnyOwner fixture checks: {errors:?}"));
        checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("main resource analysis")
            .clone()
    }

    fn checked_inline_ffi_map_analysis() -> ResourceAnalysis {
        let source = r#"
extern "C" {
    func scalar_probe(value: i32) -> i32;
}
func main() -> i32 {
    let root = map_new()
    let updated = map_set(root, "answer", scalar_probe(41 as i32))
    let size = map_size(updated)
    drop(updated)
    size
}
"#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("inline FFI MapAnyOwner fixture lexes");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("inline FFI MapAnyOwner fixture parses");
        let checked = crate::core::check_program(&file)
            .unwrap_or_else(|errors| panic!("inline FFI MapAnyOwner fixture checks: {errors:?}"));
        checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("main resource analysis")
            .clone()
    }

    fn direct_result_drop_local(analysis: &ResourceAnalysis) -> ResolvedLocalId {
        analysis
            .map_any_owner_actions
            .iter()
            .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
            .expect("explicit MapAnyOwner result Drop")
            .local
            .clone()
    }

    fn map_any_root(local: &ResolvedLocalId) -> ResourceId {
        ResourceId(NodeId(format!("map-any-owner-v1:{}", local.0 .0)))
    }

    /// Model the inline scalar FFI case: the ordinary lifecycle scanner keeps
    /// its source attempts but withholds its action ledger and fails to record
    /// the result-local Drop after rejecting the Call-shaped Set payload.
    fn withhold_ordinary_actions_and_result_drop(analysis: &mut ResourceAnalysis) {
        let dropped = direct_result_drop_local(analysis);
        analysis.ordinary_map_owner_lifecycle_v1_actions.clear();
        analysis
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .retain(|attempt| {
                !(attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::Drop
                    && attempt.receiver_local.as_ref() == Some(&dropped))
            });
    }

    #[test]
    #[ignore = "bare map_new fixtures are retired by the recoverable Result contract (E0242 at the old bare binding); the MapAnyOwner v1 ledger tamper checks await re-derivation from the family slices (map-any-contract-0.1.11 2026-11-03 family queue); un-ignore when the Set-lineage slice lands"]
    fn unmatched_explicit_drop_joins_direct_set_and_remove_results() {
        let inline_ffi = checked_inline_ffi_map_analysis();
        let inline_ffi_drop = direct_result_drop_local(&inline_ffi);
        assert!(inline_ffi
            .ordinary_map_owner_lifecycle_v1_actions
            .is_empty());
        assert!(!inline_ffi
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .iter()
            .any(|attempt| {
                attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::Drop
                    && attempt.receiver_local.as_ref() == Some(&inline_ffi_drop)
            }));
        assert!(inline_ffi.ordinary_map_attempts_fully_covered_by_map_any_owner_v1());
        assert!(inline_ffi.map_any_owner_v1_actions_are_within_ordinary_map_profile());

        for remove_after_set in [false, true] {
            let mut analysis = checked_map_analysis(remove_after_set);
            withhold_ordinary_actions_and_result_drop(&mut analysis);

            assert!(analysis.ordinary_map_attempts_fully_covered_by_map_any_owner_v1());
            assert!(analysis.map_any_owner_v1_actions_are_within_ordinary_map_profile());
        }
    }

    #[test]
    #[ignore = "bare map_new fixtures are retired by the recoverable Result contract (E0242 at the old bare binding); the MapAnyOwner v1 ledger tamper checks await re-derivation from the family slices (map-any-contract-0.1.11 2026-11-03 family queue); un-ignore when the Set-lineage slice lands"]
    fn unmatched_result_drop_rejects_wrong_local_extra_drop_and_wrong_point() {
        let base = checked_map_analysis(false);

        let mut wrong_local = base.clone();
        withhold_ordinary_actions_and_result_drop(&mut wrong_local);
        let wrong = ResolvedLocalId(NodeId("unrelated-map-local".into()));
        let drop = wrong_local
            .map_any_owner_actions
            .iter_mut()
            .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
            .expect("Drop action");
        drop.local = wrong.clone();
        drop.root = map_any_root(&wrong);
        let drop = wrong_local
            .map_any_owner_actions
            .iter()
            .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
            .expect("mutated Drop action");
        assert!(!wrong_local.ordinary_map_any_unmatched_drop_is_settled_by_map_any_owner_v1(drop));

        let mut extra_drop = base.clone();
        withhold_ordinary_actions_and_result_drop(&mut extra_drop);
        let mut duplicate = extra_drop
            .map_any_owner_actions
            .iter()
            .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
            .expect("Drop action")
            .clone();
        duplicate.point = NodeId("second-result-drop".into());
        extra_drop.map_any_owner_actions.push(duplicate);
        let drop = extra_drop
            .map_any_owner_actions
            .iter()
            .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
            .expect("first Drop action");
        assert!(!extra_drop.ordinary_map_any_unmatched_drop_is_settled_by_map_any_owner_v1(drop));

        let mut wrong_point = base;
        withhold_ordinary_actions_and_result_drop(&mut wrong_point);
        let producer_point = wrong_point
            .map_any_owner_actions
            .iter()
            .find(|action| action.kind == MapAnyOwnerActionKind::Set)
            .expect("Set producer")
            .point
            .clone();
        let drop = wrong_point
            .map_any_owner_actions
            .iter_mut()
            .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
            .expect("Drop action");
        drop.point = producer_point;
        let drop = wrong_point
            .map_any_owner_actions
            .iter()
            .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
            .expect("mutated Drop action");
        assert!(!wrong_point.ordinary_map_any_unmatched_drop_is_settled_by_map_any_owner_v1(drop));
    }

    #[test]
    #[ignore = "bare map_new fixtures are retired by the recoverable Result contract (E0242 at the old bare binding); the MapAnyOwner v1 ledger tamper checks await re-derivation from the family slices (map-any-contract-0.1.11 2026-11-03 family queue); un-ignore when the Set-lineage slice lands"]
    fn unmatched_result_drop_requires_one_exact_set_or_remove_attempt() {
        for remove_after_set in [false, true] {
            let mut missing_attempt = checked_map_analysis(remove_after_set);
            withhold_ordinary_actions_and_result_drop(&mut missing_attempt);
            let drop_local = direct_result_drop_local(&missing_attempt);
            let producer = missing_attempt
                .map_any_owner_actions
                .iter()
                .find(|action| {
                    matches!(
                        action.kind,
                        MapAnyOwnerActionKind::Set | MapAnyOwnerActionKind::Remove
                    ) && action.local == drop_local
                })
                .expect("direct result producer")
                .clone();
            missing_attempt
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .retain(|attempt| {
                    !(attempt.point == producer.point
                        && attempt.result_local.as_ref() == Some(&producer.local)
                        && matches!(
                            attempt.kind,
                            OrdinaryMapOwnerLifecycleAttemptKind::Set
                                | OrdinaryMapOwnerLifecycleAttemptKind::Remove
                        ))
                });
            let drop = missing_attempt
                .map_any_owner_actions
                .iter()
                .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
                .expect("Drop action");
            assert!(!missing_attempt
                .ordinary_map_any_unmatched_drop_is_settled_by_map_any_owner_v1(drop));

            let mut mismatched_attempt = checked_map_analysis(remove_after_set);
            withhold_ordinary_actions_and_result_drop(&mut mismatched_attempt);
            let producer = mismatched_attempt
                .map_any_owner_actions
                .iter()
                .find(|action| {
                    matches!(
                        action.kind,
                        MapAnyOwnerActionKind::Set | MapAnyOwnerActionKind::Remove
                    ) && action.local == direct_result_drop_local(&mismatched_attempt)
                })
                .expect("direct result producer")
                .clone();
            let attempt_key = mismatched_attempt
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .iter()
                .find(|attempt| {
                    attempt.point == producer.point
                        && attempt.result_local.as_ref() == Some(&producer.local)
                        && matches!(
                            attempt.kind,
                            OrdinaryMapOwnerLifecycleAttemptKind::Set
                                | OrdinaryMapOwnerLifecycleAttemptKind::Remove
                        )
                })
                .expect("producer operation attempt")
                .clone();
            let argument = if producer.kind == MapAnyOwnerActionKind::Set {
                2
            } else {
                1
            };
            mismatched_attempt
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .remove(&attempt_key);
            let mut forged_attempt = attempt_key;
            forged_attempt.arguments[argument] = NodeId("forged-operation-argument".into());
            mismatched_attempt
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .insert(forged_attempt);
            let drop = mismatched_attempt
                .map_any_owner_actions
                .iter()
                .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
                .expect("Drop action");
            assert!(!mismatched_attempt
                .ordinary_map_any_unmatched_drop_is_settled_by_map_any_owner_v1(drop));
        }
    }

    #[test]
    #[ignore = "bare map_new fixtures are retired by the recoverable Result contract (E0242 at the old bare binding); the MapAnyOwner v1 ledger tamper checks await re-derivation from the family slices (map-any-contract-0.1.11 2026-11-03 family queue); un-ignore when the Set-lineage slice lands"]
    fn unmatched_result_drop_rejects_duplicate_producer_actions() {
        let mut analysis = checked_map_analysis(false);
        withhold_ordinary_actions_and_result_drop(&mut analysis);
        let mut duplicate = analysis
            .map_any_owner_actions
            .iter()
            .find(|action| action.kind == MapAnyOwnerActionKind::Set)
            .expect("Set producer")
            .clone();
        duplicate.point = NodeId("duplicate-map-producer".into());
        analysis.map_any_owner_actions.push(duplicate);
        let drop = analysis
            .map_any_owner_actions
            .iter()
            .find(|action| action.kind == MapAnyOwnerActionKind::Drop)
            .expect("Drop action");
        assert!(!analysis.ordinary_map_any_unmatched_drop_is_settled_by_map_any_owner_v1(drop));
    }
}
