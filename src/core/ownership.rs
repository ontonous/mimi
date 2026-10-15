use std::collections::{BTreeMap, BTreeSet};

use crate::span::Span;

use super::cfg::{
    BasicBlockId, CallableCfg, CfgEdge, CfgPoint, CfgPointKind, CfgSource, EdgeId, Terminator,
};
use super::ir::{
    ResolvedBlock, ResolvedBody, ResolvedExpr, ResolvedExprKind, ResolvedFStringPart,
    ResolvedPatternKind, ResolvedStmt, ResolvedStmtKind,
};
use super::{NodeId, Origin, ResolvedLocalId, ResolvedTypeId};

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
    /// unsupported keys/to_list/map literals and temporary roots. A complete
    /// action list without a matching attempt census is not an admissible
    /// receipt.
    pub(crate) ordinary_map_owned_any_v2_operation_attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    /// Versioned source receipt for the v2 profile.  The MIR route may bind
    /// its own graph digest to this snapshot without re-reading surface AST.
    pub(crate) ordinary_map_owned_any_v2_cfg_receipt: Option<OrdinaryMapOwnedAnyV2CfgReceipt>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2Action {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) kind: OrdinaryMapOwnedAnyV2ActionKind,
    pub(crate) point: NodeId,
    pub(crate) local: ResolvedLocalId,
    pub(crate) root: ResourceId,
    pub(crate) key: Option<NodeId>,
    pub(crate) key_type: Option<ResolvedTypeId>,
    pub(crate) key_local: Option<ResolvedLocalId>,
    /// Checker classification of the exact String key expression. A local
    /// key is borrowed for the operation; literals and computed expressions
    /// are owned temporaries whose MIR lifetime must be settled at this use.
    pub(crate) key_provenance: Option<OrdinaryMapOwnedAnyV2KeyProvenance>,
    pub(crate) value: Option<NodeId>,
    pub(crate) value_type: Option<ResolvedTypeId>,
    pub(crate) payload_clone: bool,
    pub(crate) source_local: Option<ResolvedLocalId>,
    pub(crate) source: Option<ResourceId>,
    /// Exact source operand node and type for Clone/GetAny/Values actions.
    /// For Clone, this is the source expression and its resolved Map-root
    /// type; `local`/`root` identify the destination obligation while
    /// `source_local`/`source` identify the still-live source obligation.
    /// For GetAny/Values, `local` and `root` identify the observed root.
    pub(crate) source_node: Option<NodeId>,
    pub(crate) source_type: Option<ResolvedTypeId>,
    /// Exact result type and direct binding for GetAny/Values. These fields
    /// remain empty on root lifecycle actions, including Clone.
    pub(crate) result_type: Option<ResolvedTypeId>,
    pub(crate) result_local: Option<ResolvedLocalId>,
}

/// Source-certified origin class for an ordinary Map/Any V2 String key.
/// The action's `key` NodeId and `key_type` remain the exact expression and
/// type identities; this small tag tells consumers how to treat its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2KeyProvenance {
    DirectLocal,
    StringLiteral,
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
    /// `is_empty` is a staged API in the OPEN_CONTRACT, but this narrow
    /// owner-v2 slice has no action/ABI receipt for it yet.  Keep the source
    /// attempt visible so admission fails closed instead of treating the call
    /// as an unrelated scalar builtin (or silently routing to legacy).
    IsEmpty,
    GetAny,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2CfgReceipt {
    pub(crate) profile_version: OrdinaryMapOwnedAnyV2ProfileVersion,
    pub(crate) owner: NodeId,
    pub(crate) actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    pub(crate) attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    /// Exact source-level settlement for each owned GetAny/Values result.
    /// This is disjoint from the Map-root actions, so a synthetic scope drop
    /// cannot discharge an aggregate result owner.
    pub(crate) result_receipts: Vec<OrdinaryMapOwnedAnyV2ResultReceipt>,
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
    /// Compiler-owned root identities for the one admitted temporary shape:
    /// `map_get(map_new(), string_key)`. The root is settled by a dedicated
    /// Drop immediately after GetAny, never by scope cleanup.
    pub(crate) temporary_root_receipts: Vec<OrdinaryMapOwnedAnyV2TemporaryRootReceipt>,
    /// A V2-owned source-CFG and live-owner proof for one restricted,
    /// acyclic two-arm diamond. It is intentionally independent of the V1
    /// Map receipt: every live root and GetAny/Values result owner is carried
    /// on each edge and replayed by MIR validation.
    pub(crate) flow: Option<OrdinaryMapOwnedAnyV2CfgFlowReceipt>,
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

/// Exact topology and owner-state receipt for the V2 single-diamond slice.
/// The branch arms may contain read-only scalar Map observations, but root
/// creation/update/drop and result creation/settlement are confined to the
/// entry and common join respectively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedAnyV2CfgFlowReceipt {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2RootSettlementKind {
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
pub(crate) enum OrdinaryMapOwnedAnyV2ResultSettlementKind {
    ExplicitDrop,
    Transfer,
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
    pub(crate) settlement: OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: Option<NodeId>,
}

/// A Checker-owned source receipt for one precisely sourced GetAny payload
/// unpack. The projection is observational: the pair binding remains the
/// owner and must still be explicitly dropped after this conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Checker classification of the owner obligation created by one Values
/// element unpack. Copy payloads need no owner settlement, Strings are
/// settled by compiler-owned binding cleanup, and aggregate payloads require
/// a source-visible whole-binding Drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind {
    NoOwnedPayload,
    CompilerOwnedStringDrop,
    ExplicitWholeDrop,
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
    /// Ordered same-root Set/Remove lineage ending at the selecting Set.
    pub(crate) payload_selection: Vec<NodeId>,
    /// Owner disposition of the unpacked payload, independent of the Values
    /// snapshot settlement below.
    pub(crate) result_owner_settlement: OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind,
    /// Exact source point for `ExplicitWholeDrop`, or the root tail-expression
    /// anchor for compiler-owned String cleanup; absent for Copy payloads.
    pub(crate) result_owner_settlement_point: Option<NodeId>,
    /// Index of the exact root-level place in the Drop statement's places
    /// array. Present only for `ExplicitWholeDrop`.
    pub(crate) result_owner_settlement_index: Option<usize>,
    /// Exact explicit settlement of the entire Values snapshot.
    pub(crate) settlement: OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: NodeId,
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
            ),
            (
                self.result_owner_settlement,
                &self.result_owner_settlement_point,
                &self.result_owner_settlement_index,
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
                ),
                (
                    other.result_owner_settlement,
                    &other.result_owner_settlement_point,
                    &other.result_owner_settlement_index,
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

/// Version one keeps all owned Map operations in the entry block and closes
/// every root before control flow branches. Version two carries exact live
/// root facts on every edge of a supported acyclic CFG. Cycles still require
/// a separate fixed-point proof and remain rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OrdinaryMapOwnedGetI32StringV1CfgProfileVersion {
    EntryClosedTailV1,
    AcyclicPerEdgeRootsV2,
}

impl OrdinaryMapOwnedGetI32StringV1CfgProfileVersion {
    pub(crate) const fn canonical_text(self) -> &'static str {
        match self {
            Self::EntryClosedTailV1 => "ordinary-map-owned-get-entry-closed-tail-cfg-v1",
            Self::AcyclicPerEdgeRootsV2 => "ordinary-map-owned-get-acyclic-per-edge-roots-cfg-v2",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryMapOwnedGetI32StringV1CfgBlock {
    pub(crate) points: Vec<(NodeId, CfgPointKind)>,
    pub(crate) terminator: Terminator,
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
    let map_any_operands_match = match attempt.kind {
        Attempt::New => {
            arguments.is_empty()
                && attempt.receiver_local.is_none()
                && attempt.result_local.as_ref() == Some(&action.local)
                && action.key.is_none()
                && action.key_expression.is_none()
                && action.value.is_none()
                && action.value_type.is_none()
                && action.source_local.is_none()
                && action.source.is_none()
        }
        Attempt::Set => {
            arguments.len() == 3
                && attempt.receiver_local.as_ref() == action.source_local.as_ref()
                && attempt.receiver_local.is_some()
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
                && attempt.receiver_local.as_ref() == action.source_local.as_ref()
                && attempt.receiver_local.is_some()
                && attempt.result_local.as_ref() == Some(&action.local)
                && action.key.is_some()
                && action.key_expression.as_ref() == arguments.get(1)
                && action.value.is_none()
                && action.value_type.is_none()
                && action.source.is_some()
        }
        Attempt::Size => {
            arguments.len() == 1
                && attempt.receiver_local.as_ref() == Some(&action.local)
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

    /// Whether this callable contains a sealed ordinary Map/Any owner-v2
    /// construction receipt.  Consumers must pair this with the operation
    /// census and the profile marker; a v1 New action is never sufficient.
    pub(crate) fn has_ordinary_map_owned_any_v2_receipt(&self) -> bool {
        self.ordinary_map_owned_any_v2_actions.iter().any(|action| {
            action.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
                && action.kind == OrdinaryMapOwnedAnyV2ActionKind::New
        })
    }

    /// The v2 operation census is intentionally exposed through a helper so
    /// route admission cannot accidentally inspect only the admitted action
    /// subset and hide an unsupported keys/to_list/dynamic-key attempt.
    pub(crate) fn has_ordinary_map_owned_any_v2_attempts(&self) -> bool {
        !self.ordinary_map_owned_any_v2_operation_attempts.is_empty()
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
            let Some(index) = unmatched_attempts.iter().position(|attempt| {
                map_any_v1_action_covers_ordinary_attempt(self, action, attempt)
            }) else {
                return false;
            };
            unmatched_attempts.remove(index);
        }
        true
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
