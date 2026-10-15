use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::core::cfg::{EdgeKind, Terminator};
use crate::core::ir::{
    MatchArm, Permission, ResolvedBlock, ResolvedCall, ResolvedCallee, ResolvedExpr,
    ResolvedExprKind, ResolvedFStringPart, ResolvedIndex, ResolvedLiteral, ResolvedLocal,
    ResolvedPattern, ResolvedPatternKind, ResolvedPlace, ResolvedProjection, ResolvedSignature,
    ResolvedStmt, ResolvedStmtKind, ResolvedUnaryOp, ResolvedValueProjection,
};
use crate::core::{
    CanonicalActionKind, CanonicalResourceAction, CfgLocation, IndexProjection, Loan, LoanId,
    LoanKind, LocalId, MapAnyOwnerAction, MapAnyOwnerActionKind, MapAnyOwnerProfileVersion,
    MapRootAction, MapRootActionKind, NodeId, OrdinaryMapOwnedAnyV2Action,
    OrdinaryMapOwnedAnyV2ActionKind, OrdinaryMapOwnedAnyV2Attempt,
    OrdinaryMapOwnedAnyV2AttemptKind, OrdinaryMapOwnedAnyV2CfgReceipt,
    OrdinaryMapOwnedAnyV2ProfileVersion, OrdinaryMapOwnedAnyV2ResultReceipt,
    OrdinaryMapOwnedAnyV2ResultSettlementKind, OrdinaryMapOwnedGetI32StringV1Action,
    OrdinaryMapOwnedGetI32StringV1ActionKind, OrdinaryMapOwnedGetI32StringV1Attempt,
    OrdinaryMapOwnedGetI32StringV1AttemptKind, OrdinaryMapOwnedGetI32StringV1ProfileVersion,
    OrdinaryMapOwnedGetResultReceipt, OrdinaryMapOwnedGetResultSettlementKind,
    OrdinaryMapOwnerLifecycleAction, OrdinaryMapOwnerLifecycleActionKind,
    OrdinaryMapOwnerLifecycleAttempt, OrdinaryMapOwnerLifecycleAttemptKind,
    OrdinaryMapOwnerLifecycleProfileVersion, Place, PlaceProjection, ResolvedBody, ResolvedLocalId,
    ResolvedType, ResolvedTypeId, ResolvedTypeTable, ResourceAnalysis, ResourceId,
};
use crate::diagnostic::Diagnostic;

use super::{dataflow::analyze_canonical, CallableCfg};

struct ActionDraft {
    kind: CanonicalActionKind,
    resource: ResourceId,
    source: Option<Place>,
    target: Option<Place>,
    loan: Option<LoanId>,
}

/// A separate, deliberately small Checker recognizer for ordinary Map owner
/// roots. It reads only the typed ResolvedBody and never imports or converts
/// MapRoot/MapAnyOwner actions.
struct OrdinaryMapOwnerLifecycleChecker<'a> {
    body: &'a ResolvedBody,
    types: &'a ResolvedTypeTable,
    actions: Vec<OrdinaryMapOwnerLifecycleAction>,
    new_attempts: BTreeSet<NodeId>,
    operation_attempts: BTreeSet<OrdinaryMapOwnerLifecycleAttempt>,
    all_root_loads: Vec<(NodeId, ResolvedLocalId)>,
    roots: BTreeMap<ResolvedLocalId, bool>,
    allowed_root_loads: BTreeSet<NodeId>,
    invalid: bool,
}

impl<'a> OrdinaryMapOwnerLifecycleChecker<'a> {
    fn collect(
        cfg: &'a CallableCfg,
        body: &'a ResolvedBody,
        types: &'a ResolvedTypeTable,
    ) -> (
        Vec<OrdinaryMapOwnerLifecycleAction>,
        BTreeSet<NodeId>,
        BTreeSet<OrdinaryMapOwnerLifecycleAttempt>,
    ) {
        let mut checker = Self {
            body,
            types,
            actions: Vec::new(),
            new_attempts: BTreeSet::new(),
            operation_attempts: BTreeSet::new(),
            all_root_loads: Vec::new(),
            roots: BTreeMap::new(),
            allowed_root_loads: BTreeSet::new(),
            invalid: false,
        };
        checker.invalid = body
            .parameters
            .iter()
            .chain(body.captures.iter())
            .any(|local| {
                body.locals
                    .get(local)
                    .is_some_and(|binding| checker.is_record(&binding.ty))
            })
            || checker.is_record(&body.root.ty);
        checker.collect_block_evidence(&body.root);
        for expression in body.place_inputs.values() {
            checker.collect_expr_evidence(expression);
        }
        for expression in body.default_values.values() {
            checker.collect_expr_evidence(expression);
        }

        // Ordinary Map lifecycles are straight-line by default. A bounded
        // acyclic branch is also safe when every operation in an arm is a
        // read-only Size/HasKey borrow of a root introduced before the split;
        // the root remains live through the join and is settled exactly once
        // in the common continuation. Mutating or introducing a root inside
        // an arm remains fail-closed until path-local ownership facts exist.
        //
        // A loop has one narrowly closed shape: each iteration may create
        // and drop a fresh local Map root entirely inside the loop body. No
        // root is allowed to enter the loop or survive its back-edge. This
        // lets the canonical MIR body reuse one stable value identity for
        // every iteration without pretending that an owner is loop-carried.
        let straight_line = cfg.blocks.len() == 1 && cfg.edges.is_empty();
        let closed_loop = checker.supports_closed_loop_cfg(cfg);
        let closed_break_loop = checker.supports_closed_break_loop_cfg(cfg)
            && checker.closed_break_loop_body_is_map_only(&body.root);
        if !straight_line
            && !checker.supports_acyclic_read_only_cfg(cfg)
            && !closed_loop
            && !closed_break_loop
        {
            return (Vec::new(), checker.new_attempts, checker.operation_attempts);
        }

        checker.visit_block(&body.root);
        if !straight_line
            && checker.actions.iter().any(|action| {
                checker
                    .cfg_block_for_point(cfg, &action.point)
                    .is_some_and(|block| {
                        checker.is_branch_arm_block(block)
                            && !matches!(
                                action.kind,
                                OrdinaryMapOwnerLifecycleActionKind::Size
                                    | OrdinaryMapOwnerLifecycleActionKind::HasKey
                            )
                    })
            })
        {
            return (Vec::new(), checker.new_attempts, checker.operation_attempts);
        }
        if (closed_loop || closed_break_loop)
            && checker.actions.iter().any(|action| {
                !matches!(
                    action.kind,
                    OrdinaryMapOwnerLifecycleActionKind::New
                        | OrdinaryMapOwnerLifecycleActionKind::Drop
                ) || checker
                    .cfg_block_for_point(cfg, &action.point)
                    .is_none_or(|block| {
                        if closed_loop {
                            !checker.is_closed_loop_body_block(cfg, block)
                        } else {
                            !checker.is_closed_break_loop_body_block(cfg, block)
                        }
                    })
            })
        {
            // The loop receipt is intentionally iteration-local. A query,
            // persistent successor, branch arm, or operation outside the
            // loop body needs path/iteration ownership facts that this v1
            // profile does not carry.
            return (Vec::new(), checker.new_attempts, checker.operation_attempts);
        }
        checker.schedule_scope_drops(cfg);
        for (node, local) in &checker.all_root_loads {
            if checker.roots.contains_key(local) && !checker.allowed_root_loads.contains(node) {
                // Aliases, captures, aggregate/return escapes, and every
                // unrecognized use of an admitted root poison the whole
                // callable receipt.
                checker.invalid = true;
            }
        }
        if checker.roots.values().any(|dropped| !dropped) {
            checker.invalid = true;
        }

        let mut point_order = BTreeMap::new();
        for (block_rank, block) in cfg.blocks.values().enumerate() {
            for (point_rank, point) in block.points.iter().enumerate() {
                if point_order
                    .insert(point.source.node.clone(), (block_rank, point_rank))
                    .is_some()
                {
                    checker.invalid = true;
                }
            }
        }
        if checker
            .actions
            .iter()
            .any(|action| !point_order.contains_key(&action.point))
        {
            checker.invalid = true;
        }
        // Mutually exclusive arms have no single execution order. Preserve
        // source traversal order when an arm contains a read-only query so
        // New remains before the query in the receipt; straight-line and
        // post-join actions retain deterministic CFG ordering.
        let branch_read_actions = checker.actions.iter().any(|action| {
            checker
                .cfg_block_for_point(cfg, &action.point)
                .is_some_and(|block| checker.is_branch_arm_block(block))
        });
        if !branch_read_actions {
            checker.actions.sort_by_key(|action| {
                point_order
                    .get(&action.point)
                    .copied()
                    .unwrap_or((usize::MAX, usize::MAX))
            });
        }

        // Every ordinary Map call in the callable must be represented by one
        // supported action at the same typed body node. Get/Keys/Values and
        // other future Map operations remain visible as attempts, but cannot
        // be hidden behind an otherwise valid root lifecycle.
        let action_points = checker
            .actions
            .iter()
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        for attempt in &checker.operation_attempts {
            if !action_points.contains(&attempt.point) {
                checker.invalid = true;
            }
        }
        let receipt_new_points = checker
            .actions
            .iter()
            .filter(|action| action.kind == OrdinaryMapOwnerLifecycleActionKind::New)
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        if receipt_new_points != checker.new_attempts || receipt_new_points.is_empty() {
            checker.invalid = true;
        }
        if !checker.sealed_lifecycle_is_closed() {
            checker.invalid = true;
        }

        if checker.invalid {
            checker.actions.clear();
        }
        (
            checker.actions,
            checker.new_attempts,
            checker.operation_attempts,
        )
    }

    /// Checker-owned scope cleanup is an operation at the callable's final
    /// CFG point. Each surviving local contributes one separate Drop attempt
    /// and receipt; explicit drops have already removed their obligations.
    fn schedule_scope_drops(&mut self, cfg: &CallableCfg) {
        let remaining = self
            .roots
            .iter()
            .rev()
            .filter(|(_, dropped)| !**dropped)
            .map(|(local, _)| local.clone())
            .collect::<Vec<_>>();
        if remaining.is_empty() {
            return;
        }
        let Some(point) = cfg
            .blocks
            .get(&cfg.entry)
            .and_then(|block| block.points.last())
            .map(|point| point.source.node.clone())
        else {
            self.invalid = true;
            return;
        };
        for local in remaining {
            self.operation_attempts
                .insert(OrdinaryMapOwnerLifecycleAttempt {
                    kind: OrdinaryMapOwnerLifecycleAttemptKind::ScopeDrop,
                    point: point.clone(),
                    arguments: vec![local.0.clone()],
                    receiver_local: Some(local.clone()),
                    result_local: None,
                });
            self.roots.insert(local.clone(), true);
            self.actions.push(self.action(
                OrdinaryMapOwnerLifecycleActionKind::Drop,
                point.clone(),
                local,
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
    }

    /// Resolve the CFG block containing one typed source operation point.
    fn cfg_block_for_point<'cfg>(
        &self,
        cfg: &'cfg CallableCfg,
        point: &NodeId,
    ) -> Option<&'cfg crate::core::cfg::BasicBlockId> {
        cfg.blocks.iter().find_map(|(block_id, block)| {
            block
                .points
                .iter()
                .any(|cfg_point| cfg_point.source.node == *point)
                .then_some(block_id)
        })
    }

    fn is_branch_arm_block(&self, block: &crate::core::cfg::BasicBlockId) -> bool {
        block.0 .0.ends_with("/cfg:block:if-then") || block.0 .0.ends_with("/cfg:block:if-else")
    }

    /// Admit only acyclic CFGs whose branch-arm operations are read-only
    /// observations. New/Set/Remove/Clone/Drop inside an arm need path-local
    /// ownership facts and remain fail-closed; a common post-join Drop is
    /// allowed because the root is still live on both incoming edges.
    fn supports_acyclic_read_only_cfg(&self, cfg: &CallableCfg) -> bool {
        if cfg.blocks.len() <= 1 || cfg.reachable.len() != cfg.blocks.len() {
            return false;
        }
        if cfg.edges.values().any(|edge| {
            !cfg.reachable.contains(&edge.from)
                || !cfg.reachable.contains(&edge.to)
                || !matches!(
                    edge.kind,
                    EdgeKind::Then | EdgeKind::Else | EdgeKind::Fallthrough
                )
        }) {
            return false;
        }
        let mut indegree = cfg
            .reachable
            .iter()
            .cloned()
            .map(|block| (block, 0usize))
            .collect::<BTreeMap<_, _>>();
        for edge in cfg
            .edges
            .values()
            .filter(|edge| cfg.reachable.contains(&edge.from) && cfg.reachable.contains(&edge.to))
        {
            let Some(degree) = indegree.get_mut(&edge.to) else {
                return false;
            };
            *degree += 1;
        }
        let mut ready = indegree
            .iter()
            .filter_map(|(block, degree)| (*degree == 0).then_some(block.clone()))
            .collect::<Vec<_>>();
        let mut visited = 0usize;
        while let Some(block) = ready.pop() {
            visited += 1;
            for edge in cfg.successors(&block) {
                if !cfg.reachable.contains(&edge.to) {
                    continue;
                }
                let Some(degree) = indegree.get_mut(&edge.to) else {
                    return false;
                };
                *degree = degree.saturating_sub(1);
                if *degree == 0 {
                    ready.push(edge.to.clone());
                }
            }
        }
        if visited != cfg.reachable.len() {
            return false;
        }
        self.operation_attempts.iter().all(|attempt| {
            let Some(block) = self.cfg_block_for_point(cfg, &attempt.point) else {
                return false;
            };
            !self.is_branch_arm_block(block)
                || matches!(
                    attempt.kind,
                    OrdinaryMapOwnerLifecycleAttemptKind::Size
                        | OrdinaryMapOwnerLifecycleAttemptKind::HasKey
                )
        })
    }

    /// Recognize the only loop shape admitted by the ordinary owner v1
    /// receipt: one canonical while header/body/exit cycle with all Map
    /// attempts in the body, and only New/Drop attempts there. The source
    /// CFG may contain the fallthrough edge into the header and the body's
    /// back-edge; no break/continue/nested branch is admitted. Ownership is
    /// therefore reset at the next iteration instead of being carried across
    /// an edge.
    fn supports_closed_loop_cfg(&self, cfg: &CallableCfg) -> bool {
        if cfg.blocks.len() < 3 || cfg.reachable.len() != cfg.blocks.len() {
            return false;
        }
        let body_edges = cfg
            .edges
            .values()
            .filter(|edge| edge.kind == EdgeKind::LoopBody)
            .collect::<Vec<_>>();
        let exit_edges = cfg
            .edges
            .values()
            .filter(|edge| edge.kind == EdgeKind::LoopExit)
            .collect::<Vec<_>>();
        let back_edges = cfg
            .edges
            .values()
            .filter(|edge| edge.kind == EdgeKind::Backedge)
            .collect::<Vec<_>>();
        if body_edges.len() != 1 || exit_edges.len() != 1 || back_edges.len() != 1 {
            return false;
        }
        let body_edge = body_edges[0];
        let exit_edge = exit_edges[0];
        let back_edge = back_edges[0];
        if body_edge.from != exit_edge.from
            || back_edge.to != body_edge.from
            || back_edge.from != body_edge.to
        {
            return false;
        }
        let header = body_edge.from.clone();
        let body = body_edge.to.clone();
        let exit = exit_edge.to.clone();
        if header == body || header == exit || body == exit {
            return false;
        }
        // The loop body must be a single source block. This excludes nested
        // branches and nested loops until their path-local receipts exist.
        if cfg.reachable.iter().any(|block| {
            block != &cfg.entry && block != &header && block != &body && block != &exit
        }) {
            return false;
        }
        let allowed_edges = cfg
            .edges
            .values()
            .filter(|edge| cfg.reachable.contains(&edge.from) && cfg.reachable.contains(&edge.to))
            .all(|edge| {
                matches!(
                    edge.kind,
                    EdgeKind::Fallthrough
                        | EdgeKind::LoopBody
                        | EdgeKind::LoopExit
                        | EdgeKind::Backedge
                )
            });
        if !allowed_edges {
            return false;
        }
        // A loop entered from the callable entry must have a single
        // fallthrough edge into its header. Any preceding Map operation would
        // need a separate receipt for the loop boundary and is rejected here.
        let entry_edges = cfg
            .edges
            .values()
            .filter(|edge| edge.to == header && edge.kind == EdgeKind::Fallthrough)
            .collect::<Vec<_>>();
        if entry_edges.len() != 1
            || !entry_edges.iter().any(|edge| edge.from == cfg.entry)
            || self.operation_attempts.iter().any(|attempt| {
                !matches!(
                    attempt.kind,
                    OrdinaryMapOwnerLifecycleAttemptKind::New
                        | OrdinaryMapOwnerLifecycleAttemptKind::Drop
                ) || self
                    .cfg_block_for_point(cfg, &attempt.point)
                    .is_none_or(|block| block != &body)
            })
        {
            return false;
        }
        true
    }

    /// Recognize the finite form of an infinite `loop` whose body reaches a
    /// single `break` after settling every Map root introduced by that
    /// iteration. The resolved CFG has no condition/header split for this
    /// form: the callable entry falls through into one loop-header block and
    /// that block terminates with one `Break` edge to the loop exit. There is
    /// consequently no loop-carried owner value and no back-edge to replay.
    /// Any operation outside the reachable header, a second break, a
    /// continue/back-edge, or a root crossing the boundary remains rejected
    /// by this narrow receipt.
    fn supports_closed_break_loop_cfg(&self, cfg: &CallableCfg) -> bool {
        if cfg.blocks.len() < 2 || cfg.reachable.len() != cfg.blocks.len() {
            return false;
        }
        let break_edges = cfg
            .edges
            .values()
            .filter(|edge| edge.kind == EdgeKind::Break)
            .collect::<Vec<_>>();
        if break_edges.len() != 1 {
            return false;
        }
        let break_edge = break_edges[0];
        let header = break_edge.from.clone();
        let exit = break_edge.to.clone();
        if header == exit || !cfg.reachable.contains(&header) || !cfg.reachable.contains(&exit) {
            return false;
        }
        let entry_edges = cfg
            .edges
            .values()
            .filter(|edge| edge.kind == EdgeKind::Fallthrough && edge.to == header)
            .collect::<Vec<_>>();
        let fallthrough_edges = cfg
            .edges
            .values()
            .filter(|edge| edge.kind == EdgeKind::Fallthrough)
            .collect::<Vec<_>>();
        if entry_edges.len() != 1
            || fallthrough_edges.len() != 1
            || entry_edges[0].from != cfg.entry
        {
            return false;
        }
        // A finite `loop` must not silently contain another reachable block:
        // nested branches/loops need their own path-local owner receipt.
        if cfg
            .reachable
            .iter()
            .any(|block| block != &cfg.entry && block != &header && block != &exit)
        {
            return false;
        }
        if cfg.edges.values().any(|edge| {
            !cfg.reachable.contains(&edge.from)
                || !cfg.reachable.contains(&edge.to)
                || !matches!(edge.kind, EdgeKind::Fallthrough | EdgeKind::Break)
        }) {
            return false;
        }
        self.operation_attempts.iter().all(|attempt| {
            let Some(block) = self.cfg_block_for_point(cfg, &attempt.point) else {
                return false;
            };
            cfg.reachable.contains(block)
                && block == &header
                && matches!(
                    attempt.kind,
                    OrdinaryMapOwnerLifecycleAttemptKind::New
                        | OrdinaryMapOwnerLifecycleAttemptKind::Drop
                )
        })
    }

    /// The finite loop lowerer deliberately carries no outer locals through
    /// its exit block. Keep this receipt limited to the owner-only source
    /// body for which that is semantically exact: direct `map_new` bindings,
    /// direct root drops, and a value-less `break`. Scalar assignment, calls,
    /// nested control flow, and other effects stay fail-closed until a proper
    /// loop-carried locals contract is added.
    fn closed_break_loop_body_is_map_only(&self, body: &ResolvedBlock) -> bool {
        let mut loops = body
            .statements
            .iter()
            .filter_map(|statement| match &statement.kind {
                ResolvedStmtKind::Loop(loop_body) => Some(loop_body),
                _ => None,
            });
        let Some(loop_body) = loops.next() else {
            return false;
        };
        // A single owner-only finite loop may be surrounded by scalar
        // statements or a scalar tail result. Any second/nested loop needs a
        // separate receipt and stays outside this narrow profile.
        if loops.next().is_some() {
            return false;
        }
        loop_body.result.is_none()
            && loop_body
                .statements
                .iter()
                .all(|statement| match &statement.kind {
                    ResolvedStmtKind::Bind {
                        pattern,
                        initializer: Some(initializer),
                    } => {
                        matches!(
                            (&pattern.kind, &initializer.kind),
                            (
                                ResolvedPatternKind::Binding { .. },
                                ResolvedExprKind::Call(ResolvedCall {
                                    callee: ResolvedCallee::Builtin(name),
                                    arguments,
                                    ..
                                })
                            ) if name.as_str() == "map_new" && arguments.is_empty()
                        )
                    }
                    ResolvedStmtKind::Drop(places) => {
                        places.iter().all(|place| place.projections.is_empty())
                    }
                    ResolvedStmtKind::Break(None) => true,
                    _ => false,
                })
    }

    fn is_closed_loop_body_block(
        &self,
        cfg: &CallableCfg,
        block: &crate::core::cfg::BasicBlockId,
    ) -> bool {
        cfg.edges
            .values()
            .any(|edge| edge.kind == EdgeKind::LoopBody && edge.to == *block && edge.from != *block)
    }

    fn is_closed_break_loop_body_block(
        &self,
        cfg: &CallableCfg,
        block: &crate::core::cfg::BasicBlockId,
    ) -> bool {
        cfg.edges
            .values()
            .any(|edge| edge.kind == EdgeKind::Break && edge.from == *block)
    }

    /// An ordinary free-value binding creates an independent persistent root.
    /// It never copies the raw root handle or steals the source obligation.
    fn visit_clone(&mut self, expression: &ResolvedExpr, target: &ResolvedLocalId) {
        let Some(source) = self.live_root_load(expression) else {
            self.invalid = true;
            return;
        };
        if self.roots.contains_key(target)
            || !self.is_record(&expression.ty)
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|local| local.ty == expression.ty)
        {
            self.invalid = true;
            return;
        }
        self.allow_root_load(expression);
        self.operation_attempts
            .insert(OrdinaryMapOwnerLifecycleAttempt {
                kind: OrdinaryMapOwnerLifecycleAttemptKind::Clone,
                point: expression.node_id.clone(),
                arguments: vec![source.0.clone(), target.0.clone()],
                receiver_local: Some(source.clone()),
                result_local: Some(target.clone()),
            });
        self.roots.insert(target.clone(), false);
        self.actions.push(self.action(
            OrdinaryMapOwnerLifecycleActionKind::Clone,
            expression.node_id.clone(),
            target.clone(),
            None,
            None,
            None,
            None,
            None,
            Some(source),
        ));
    }

    fn sealed_lifecycle_is_closed(&self) -> bool {
        use OrdinaryMapOwnerLifecycleActionKind as Kind;

        if self.actions.is_empty() {
            return false;
        }
        let mut ever_created = BTreeSet::<ResolvedLocalId>::new();
        let mut live = BTreeSet::<ResolvedLocalId>::new();
        for action in &self.actions {
            if action.profile_version != OrdinaryMapOwnerLifecycleProfileVersion::V1
                || action.root != Self::resource_identity(&action.local)
            {
                return false;
            }
            match action.kind {
                Kind::New => {
                    if action.key.is_some()
                        || action.key_type.is_some()
                        || action.key_local.is_some()
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !ever_created.insert(action.local.clone())
                        || !live.insert(action.local.clone())
                    {
                        return false;
                    }
                }
                Kind::Clone => {
                    let (Some(source_local), Some(source)) = (&action.source_local, &action.source)
                    else {
                        return false;
                    };
                    if action.key.is_some()
                        || action.key_type.is_some()
                        || action.key_local.is_some()
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || source != &Self::resource_identity(source_local)
                        || !live.contains(source_local)
                        || source_local == &action.local
                        || !ever_created.insert(action.local.clone())
                        || !live.insert(action.local.clone())
                    {
                        return false;
                    }
                }
                Kind::Set => {
                    let (
                        Some(key),
                        Some(key_type),
                        key_local,
                        Some(_value),
                        Some(value_type),
                        Some(source_local),
                        Some(source),
                    ) = (
                        &action.key,
                        &action.key_type,
                        &action.key_local,
                        &action.value,
                        &action.value_type,
                        &action.source_local,
                        &action.source,
                    )
                    else {
                        return false;
                    };
                    if key.0.is_empty()
                        || !self.is_string(key_type)
                        || !key_local.as_ref().is_none_or(|local| {
                            self.body
                                .locals
                                .get(local)
                                .is_some_and(|binding| self.is_string(&binding.ty))
                        })
                        || !matches!(
                            self.types.get(value_type),
                            Some(ResolvedType::Primitive(
                                crate::core::PrimitiveType::I32
                                    | crate::core::PrimitiveType::String
                            ))
                        )
                        || source != &Self::resource_identity(source_local)
                        || !live.contains(source_local)
                        || !ever_created.insert(action.local.clone())
                        || !live.insert(action.local.clone())
                    {
                        return false;
                    }
                }
                Kind::Remove => {
                    let (Some(key), Some(key_type), Some(source_local), Some(source)) = (
                        &action.key,
                        &action.key_type,
                        &action.source_local,
                        &action.source,
                    ) else {
                        return false;
                    };
                    if key.0.is_empty()
                        || !self.is_string(key_type)
                        || !action.key_local.as_ref().is_none_or(|local| {
                            self.body
                                .locals
                                .get(local)
                                .is_some_and(|binding| self.is_string(&binding.ty))
                        })
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || source != &Self::resource_identity(source_local)
                        || !live.contains(source_local)
                        || !ever_created.insert(action.local.clone())
                        || !live.insert(action.local.clone())
                    {
                        return false;
                    }
                }
                Kind::Size => {
                    if action.key.is_some()
                        || action.key_type.is_some()
                        || action.key_local.is_some()
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !live.contains(&action.local)
                    {
                        return false;
                    }
                }
                Kind::HasKey => {
                    if action.key.as_ref().is_none_or(|key| key.0.is_empty())
                        || action
                            .key_type
                            .as_ref()
                            .is_none_or(|key_type| !self.is_string(key_type))
                        || !action.key_local.as_ref().is_none_or(|local| {
                            self.body
                                .locals
                                .get(local)
                                .is_some_and(|binding| self.is_string(&binding.ty))
                        })
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !live.contains(&action.local)
                    {
                        return false;
                    }
                }
                Kind::Drop => {
                    if action.key.is_some()
                        || action.key_type.is_some()
                        || action.key_local.is_some()
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !live.remove(&action.local)
                    {
                        return false;
                    }
                }
            }
        }
        live.is_empty() && ever_created == self.roots.keys().cloned().collect()
    }

    fn resource_identity(local: &ResolvedLocalId) -> ResourceId {
        ResourceId(NodeId(format!(
            "ordinary-map-owner-lifecycle-i32-string-v1:{}",
            local.0 .0
        )))
    }

    fn map_attempt_kind(name: &str) -> Option<OrdinaryMapOwnerLifecycleAttemptKind> {
        use OrdinaryMapOwnerLifecycleAttemptKind as Kind;
        match name {
            "map_new" => Some(Kind::New),
            "map_get" => Some(Kind::Get),
            "map_set" => Some(Kind::Set),
            "map_remove" => Some(Kind::Remove),
            "map_size" => Some(Kind::Size),
            "has_key" => Some(Kind::HasKey),
            "keys" => Some(Kind::Keys),
            "values" => Some(Kind::Values),
            // `to_list` is the source-compatible MapExt spelling for the
            // heterogeneous snapshot. Keep it in the attempt census even
            // though the current owned-get profile has no action receipt for
            // the pair-list shape; the unmatched attempt must stay
            // fail-closed rather than disappearing from coverage.
            "to_list" => Some(Kind::Values),
            "map_from_list" => Some(Kind::FromList),
            // `map` is a language intrinsic for list mapping
            // (`core::builtins::is_language_intrinsic_callable`), not an
            // ordinary Map owner operation. Ordinary Maps use `map_new` and
            // the `map_*` operations above.
            _ => None,
        }
    }

    fn record_call_attempt(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        result_local: Option<ResolvedLocalId>,
    ) {
        let ResolvedCallee::Builtin(name) = &call.callee else {
            return;
        };
        let Some(kind) = Self::map_attempt_kind(name.as_str()) else {
            return;
        };
        let arguments = call
            .arguments
            .iter()
            .map(|argument| argument.value.node_id.clone())
            .collect::<Vec<_>>();
        let receiver_local =
            call.arguments
                .first()
                .and_then(|argument| match &argument.value.kind {
                    ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                        Some(place.base.clone())
                    }
                    _ => None,
                });
        self.operation_attempts
            .insert(OrdinaryMapOwnerLifecycleAttempt {
                kind,
                point: expression.node_id.clone(),
                arguments,
                receiver_local,
                result_local,
            });
        if kind == OrdinaryMapOwnerLifecycleAttemptKind::New {
            self.new_attempts.insert(expression.node_id.clone());
        }
    }

    fn collect_block_evidence(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    if let Some(value) = initializer {
                        if let (
                            ResolvedExprKind::Call(call),
                            ResolvedPatternKind::Binding { local, .. },
                        ) = (&value.kind, &pattern.kind)
                        {
                            self.record_call_attempt(value, call, Some(local.clone()));
                            for argument in &call.arguments {
                                self.collect_expr_evidence(&argument.value);
                            }
                        } else {
                            self.collect_expr_evidence(value);
                        }
                    }
                }
                ResolvedStmtKind::Assign { value, .. }
                | ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => {
                    self.collect_expr_evidence(value);
                }
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.collect_expr_evidence(value);
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    self.collect_expr_evidence(condition);
                    self.collect_block_evidence(body);
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                }
                | ResolvedStmtKind::For {
                    iterable: initializer,
                    body,
                    ..
                } => {
                    self.collect_expr_evidence(initializer);
                    self.collect_block_evidence(body);
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    self.collect_expr_evidence(initializer);
                    self.collect_block_evidence(then_block);
                    if let Some(block) = else_block {
                        self.collect_block_evidence(block);
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    self.collect_block_evidence(body);
                }
                ResolvedStmtKind::Drop(_) | ResolvedStmtKind::Continue => {}
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.collect_expr_evidence(expression);
                    }
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    self.collect_expr_evidence(value);
                    self.collect_block_evidence(body);
                }
                ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            self.collect_expr_evidence(result);
        }
    }

    fn collect_expr_evidence(&mut self, expression: &ResolvedExpr) {
        match &expression.kind {
            ResolvedExprKind::Load(place) => {
                self.all_root_loads
                    .push((expression.node_id.clone(), place.base.clone()));
            }
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        self.collect_expr_evidence(value);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.collect_expr_evidence(value);
                if let ResolvedValueProjection::Index(index) = projection {
                    self.collect_expr_evidence(index);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                self.collect_expr_evidence(left);
                self.collect_expr_evidence(right);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.collect_expr_evidence(operand),
            ResolvedExprKind::Call(call) => {
                self.record_call_attempt(expression, call, None);
                for argument in &call.arguments {
                    self.collect_expr_evidence(&argument.value);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.collect_expr_evidence(value);
                }
            }
            ResolvedExprKind::Map(values) => {
                self.operation_attempts
                    .insert(OrdinaryMapOwnerLifecycleAttempt {
                        kind: OrdinaryMapOwnerLifecycleAttemptKind::MapLiteral,
                        point: expression.node_id.clone(),
                        arguments: values
                            .iter()
                            .flat_map(|(key, value)| [key.node_id.clone(), value.node_id.clone()])
                            .collect(),
                        receiver_local: None,
                        result_local: None,
                    });
                for (key, value) in values {
                    self.collect_expr_evidence(key);
                    self.collect_expr_evidence(value);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.collect_expr_evidence(value);
                self.collect_expr_evidence(iterable);
                if let Some(guard) = guard {
                    self.collect_expr_evidence(guard);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => {
                self.collect_expr_evidence(receiver)
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.collect_expr_evidence(&field.value);
                }
                if let Some(rest) = rest {
                    self.collect_expr_evidence(rest);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.collect_block_evidence(block),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.collect_expr_evidence(condition);
                self.collect_block_evidence(then_block);
                self.collect_block_evidence(else_block);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.collect_expr_evidence(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.collect_expr_evidence(guard);
                    }
                    self.collect_expr_evidence(&arm.body);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                self.collect_expr_evidence(start);
                self.collect_expr_evidence(end);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.collect_expr_evidence(target);
                if let Some(start) = start {
                    self.collect_expr_evidence(start);
                }
                if let Some(end) = end {
                    self.collect_expr_evidence(end);
                }
            }
            ResolvedExprKind::Lambda(_) => {
                // A lambda is a distinct callable owner. Its body is scanned
                // only after it has a canonical ResolvedBody/CallableCfg of
                // its own; inheriting its attempts here would let a closure
                // manufacture authority under its enclosing owner.
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }

    fn visit_block(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            self.visit_stmt(statement);
        }
        if let Some(result) = &block.result {
            self.visit_expr(result, None);
        }
    }

    fn visit_stmt(&mut self, statement: &ResolvedStmt) {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                if let Some(initializer) = initializer {
                    let target = match &pattern.kind {
                        ResolvedPatternKind::Binding { local, .. } => Some(local.clone()),
                        _ => None,
                    };
                    self.visit_expr(initializer, target.as_ref());
                }
            }
            ResolvedStmtKind::Assign { target, value, .. } => {
                if self.roots.contains_key(&target.base) {
                    self.invalid = true;
                }
                self.visit_expr(value, None);
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    self.visit_expr(value, None);
                }
            }
            ResolvedStmtKind::Continue | ResolvedStmtKind::Drop(_) => {
                if let ResolvedStmtKind::Drop(places) = &statement.kind {
                    self.visit_drop(statement, places);
                }
            }
            ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => self.visit_expr(value, None),
            ResolvedStmtKind::While { condition, body } => {
                // A closed loop may own fresh ordinary Map roots for one
                // iteration at a time. Preserve the live-root boundary at
                // the header, then analyze the body once; the CFG shape check
                // below proves that every body operation is New/Drop and no
                // root crosses the back-edge.
                let live_before = self
                    .roots
                    .iter()
                    .filter_map(|(local, dropped)| (!*dropped).then_some(local.clone()))
                    .collect::<BTreeSet<_>>();
                self.visit_expr(condition, None);
                self.visit_block(body);
                let live_after = self
                    .roots
                    .iter()
                    .filter_map(|(local, dropped)| (!*dropped).then_some(local.clone()))
                    .collect::<BTreeSet<_>>();
                if live_before != live_after {
                    self.invalid = true;
                }
            }
            ResolvedStmtKind::WhileLet { initializer, .. }
            | ResolvedStmtKind::For {
                iterable: initializer,
                ..
            }
            | ResolvedStmtKind::IfLet { initializer, .. } => {
                self.visit_expr(initializer, None);
            }
            ResolvedStmtKind::Loop(body) => {
                // A finite `loop { ... break }` has no loop-carried owner
                // state. Analyze the body once and require the live-root set
                // at the break boundary to equal the set at loop entry; the
                // dedicated CFG recognizer below further restricts all
                // reachable operations to New/Drop in the single header.
                let live_before = self
                    .roots
                    .iter()
                    .filter_map(|(local, dropped)| (!*dropped).then_some(local.clone()))
                    .collect::<BTreeSet<_>>();
                self.visit_block(body);
                let live_after = self
                    .roots
                    .iter()
                    .filter_map(|(local, dropped)| (!*dropped).then_some(local.clone()))
                    .collect::<BTreeSet<_>>();
                if live_before != live_after {
                    self.invalid = true;
                }
            }
            ResolvedStmtKind::Scope { .. } | ResolvedStmtKind::NestedCallable(_) => {
                self.invalid = true
            }
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    self.visit_expr(expression, None);
                }
            }
            ResolvedStmtKind::Pinned { value, .. } => self.visit_expr(value, None),
        }
    }

    fn visit_drop(&mut self, statement: &ResolvedStmt, places: &[ResolvedPlace]) {
        for place in places {
            let Some(was_dropped) = self.roots.get(&place.base).copied() else {
                continue;
            };
            let local = place.base.clone();
            let point = statement.node_id.clone();
            self.operation_attempts
                .insert(OrdinaryMapOwnerLifecycleAttempt {
                    kind: OrdinaryMapOwnerLifecycleAttemptKind::Drop,
                    point: point.clone(),
                    arguments: vec![local.0.clone()],
                    receiver_local: Some(local.clone()),
                    result_local: None,
                });
            if !place.projections.is_empty() || was_dropped {
                self.invalid = true;
                continue;
            }
            self.roots.insert(local.clone(), true);
            self.actions.push(self.action(
                OrdinaryMapOwnerLifecycleActionKind::Drop,
                point,
                local.clone(),
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
    }

    fn visit_expr(&mut self, expression: &ResolvedExpr, direct_bind: Option<&ResolvedLocalId>) {
        match &expression.kind {
            ResolvedExprKind::Call(call) => {
                let builtin = match &call.callee {
                    ResolvedCallee::Builtin(name) => name.as_str(),
                    _ => "",
                };
                match builtin {
                    "map_new" => self.visit_new(expression, call, direct_bind),
                    "map_set" => self.visit_set(expression, call, direct_bind),
                    "map_remove" => self.visit_remove(expression, call, direct_bind),
                    "map_size" => self.visit_size(expression, call),
                    "has_key" => self.visit_has_key(expression, call),
                    "map_get" | "keys" | "values" | "map_from_list" => {
                        self.invalid = true;
                    }
                    _ => {}
                }
                for (index, argument) in call.arguments.iter().enumerate() {
                    let list_map_callback = builtin == "map"
                        && index == 1
                        && matches!(&argument.value.kind, ResolvedExprKind::Lambda(_));
                    if list_map_callback {
                        // This exact inline callback is a nested callable, not
                        // part of the caller's ordinary Map lifecycle.  Scan
                        // its body only for *attempt evidence* so a captured
                        // root or nested Map operation poisons the enclosing
                        // receipt; the callback never contributes authority
                        // to the caller's action ledger.
                        if let ResolvedExprKind::Lambda(lambda) = &argument.value.kind {
                            self.collect_block_evidence(&lambda.body);
                            if lambda.captures.iter().any(|local| {
                                self.body.locals.get(local).is_some_and(|binding| {
                                    checker_is_record(self.types, &binding.ty)
                                })
                            }) {
                                self.invalid = true;
                            }
                        }
                        continue;
                    }
                    self.visit_expr(&argument.value, None);
                }
            }
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        self.visit_expr(value, None);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.visit_expr(value, None);
                if let ResolvedValueProjection::Index(index) = projection {
                    self.visit_expr(index, None);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                self.visit_expr(left, None);
                self.visit_expr(right, None);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.visit_expr(operand, None),
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.visit_expr(value, None);
                }
            }
            ResolvedExprKind::Map(values) => {
                for (key, value) in values {
                    self.visit_expr(key, None);
                    self.visit_expr(value, None);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.visit_expr(value, None);
                self.visit_expr(iterable, None);
                if let Some(guard) = guard {
                    self.visit_expr(guard, None);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => self.visit_expr(receiver, None),
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.visit_expr(&field.value, None);
                }
                if let Some(rest) = rest {
                    self.visit_expr(rest, None);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.visit_block(block),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.visit_expr(condition, None);
                let live_root_at_split = self.roots.values().any(|dropped| !*dropped);
                let branch_action_start = self.actions.len();
                self.visit_block(then_block);
                self.visit_block(else_block);
                let branch_has_read_only_observation =
                    self.actions[branch_action_start..].iter().any(|action| {
                        matches!(
                            action.kind,
                            OrdinaryMapOwnerLifecycleActionKind::Size
                                | OrdinaryMapOwnerLifecycleActionKind::HasKey
                        )
                    });
                if live_root_at_split && !branch_has_read_only_observation {
                    // A root may cross an acyclic branch only when that
                    // branch carries a checked read-only observation. With
                    // no such action, a prefix receipt would skip the live
                    // ownership obligation on both edges and settle it only
                    // after the join without representing the crossing.
                    self.invalid = true;
                }
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee, None);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.visit_expr(guard, None);
                    }
                    self.visit_expr(&arm.body, None);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                self.visit_expr(start, None);
                self.visit_expr(end, None);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.visit_expr(target, None);
                if let Some(start) = start {
                    self.visit_expr(start, None);
                }
                if let Some(end) = end {
                    self.visit_expr(end, None);
                }
            }
            ResolvedExprKind::Lambda(lambda) => {
                // A no-capture closure has its own independent Map collector
                // invocation and is not a caller-local Map operation. A Map
                // root captured from this body remains unsupported and must
                // not bypass the caller's ownership ledger.
                if lambda.captures.iter().any(|local| {
                    self.body
                        .locals
                        .get(local)
                        .is_some_and(|binding| checker_is_record(self.types, &binding.ty))
                }) {
                    self.invalid = true;
                }
            }
            ResolvedExprKind::Load(place) => {
                if let Some(target) = direct_bind {
                    if self.roots.contains_key(&place.base) {
                        self.visit_clone(expression, target);
                    }
                }
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }

    fn visit_new(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let Some(local) = direct_bind else {
            return;
        };
        if !call.arguments.is_empty()
            || !self.is_record(&expression.ty)
            || !self
                .body
                .locals
                .get(local)
                .is_some_and(|binding| binding.ty == expression.ty)
            || self.roots.contains_key(local)
        {
            self.invalid = true;
            return;
        }
        self.roots.insert(local.clone(), false);
        self.actions.push(self.action(
            OrdinaryMapOwnerLifecycleActionKind::New,
            expression.node_id.clone(),
            local.clone(),
            None,
            None,
            None,
            None,
            None,
            None,
        ));
    }

    fn visit_set(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let Some(target) = direct_bind else {
            return;
        };
        if call.arguments.len() != 3 || self.roots.contains_key(target) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let value = &call.arguments[2].value;
        let Some(source_local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&source.ty)
            || !self.is_record(&expression.ty)
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|binding| binding.ty == expression.ty)
            || !self.valid_key(key)
            || !self.valid_payload(value)
        {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.roots.insert(target.clone(), false);
        self.actions.push(self.action(
            OrdinaryMapOwnerLifecycleActionKind::Set,
            expression.node_id.clone(),
            target.clone(),
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Self::direct_key_local(key),
            Some(value.node_id.clone()),
            Some(value.ty.clone()),
            Some(source_local),
        ));
    }

    fn visit_remove(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let Some(target) = direct_bind else {
            return;
        };
        if call.arguments.len() != 2 || self.roots.contains_key(target) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let Some(source_local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&source.ty)
            || !self.is_record(&expression.ty)
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|binding| binding.ty == expression.ty)
            || !self.valid_key(key)
        {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.roots.insert(target.clone(), false);
        self.actions.push(self.action(
            OrdinaryMapOwnerLifecycleActionKind::Remove,
            expression.node_id.clone(),
            target.clone(),
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Self::direct_key_local(key),
            None,
            None,
            Some(source_local),
        ));
    }

    fn visit_size(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        if call.arguments.len() != 1 || !self.is_i32(&expression.ty) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let Some(local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&source.ty) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.actions.push(self.action(
            OrdinaryMapOwnerLifecycleActionKind::Size,
            expression.node_id.clone(),
            local,
            None,
            None,
            None,
            None,
            None,
            None,
        ));
    }

    fn visit_has_key(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        if call.arguments.len() != 2 || !self.is_bool(&expression.ty) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let Some(local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&source.ty) || !self.valid_key(key) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.actions.push(self.action(
            OrdinaryMapOwnerLifecycleActionKind::HasKey,
            expression.node_id.clone(),
            local,
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Self::direct_key_local(key),
            None,
            None,
            None,
        ));
    }

    fn valid_payload(&self, expression: &ResolvedExpr) -> bool {
        let supported_type = self.is_i32(&expression.ty) || self.is_string(&expression.ty);
        supported_type
            && match &expression.kind {
                ResolvedExprKind::Literal(ResolvedLiteral::Int(_) | ResolvedLiteral::String(_)) => {
                    true
                }
                ResolvedExprKind::Load(place) => place.projections.is_empty(),
                _ => false,
            }
    }

    fn valid_key(&self, expression: &ResolvedExpr) -> bool {
        self.is_string(&expression.ty)
            && match &expression.kind {
                ResolvedExprKind::Literal(ResolvedLiteral::String(_)) => true,
                ResolvedExprKind::Load(place) => place.projections.is_empty(),
                _ => false,
            }
    }

    fn direct_key_local(expression: &ResolvedExpr) -> Option<ResolvedLocalId> {
        match &expression.kind {
            ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                Some(place.base.clone())
            }
            _ => None,
        }
    }

    fn live_root_load(&self, expression: &ResolvedExpr) -> Option<ResolvedLocalId> {
        let ResolvedExprKind::Load(place) = &expression.kind else {
            return None;
        };
        if !place.projections.is_empty() || self.roots.get(&place.base) != Some(&false) {
            return None;
        }
        Some(place.base.clone())
    }

    fn allow_root_load(&mut self, expression: &ResolvedExpr) {
        self.allowed_root_loads.insert(expression.node_id.clone());
    }

    fn action(
        &self,
        kind: OrdinaryMapOwnerLifecycleActionKind,
        point: NodeId,
        local: ResolvedLocalId,
        key: Option<NodeId>,
        key_type: Option<ResolvedTypeId>,
        key_local: Option<ResolvedLocalId>,
        value: Option<NodeId>,
        value_type: Option<ResolvedTypeId>,
        source_local: Option<ResolvedLocalId>,
    ) -> OrdinaryMapOwnerLifecycleAction {
        let root = Self::resource_identity(&local);
        let source = source_local.as_ref().map(Self::resource_identity);
        OrdinaryMapOwnerLifecycleAction {
            profile_version: OrdinaryMapOwnerLifecycleProfileVersion::V1,
            kind,
            point,
            local,
            root,
            key,
            key_type,
            key_local,
            value,
            value_type,
            source_local,
            source,
        }
    }

    fn is_record(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
        )
    }

    fn is_i32(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
        )
    }

    fn is_string(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
        )
    }

    fn is_bool(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::Bool))
        )
    }
}

/// Independent source checker for the first ordinary Map/Any owner-v2
/// profile.  The implementation is intentionally straight-line and local:
/// it records every operation attempt before deciding whether the operation
/// receives an action receipt. This keeps unsupported keys, `to_list`,
/// aliases, and non-concrete record payloads visible to route admission.
struct OrdinaryMapOwnedAnyV2Checker<'a> {
    body: &'a ResolvedBody,
    types: &'a ResolvedTypeTable,
    actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    operation_attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    roots: BTreeMap<ResolvedLocalId, bool>,
    /// Exact immediate map_new arguments accepted only for GetAny. The
    /// synthetic local is compiler-owned and identifies that temporary root
    /// across the New, GetAny, and immediate Drop actions.
    temporary_roots: BTreeMap<NodeId, (ResolvedLocalId, NodeId, ResolvedTypeId)>,
    temporary_root_receipts: Vec<crate::core::OrdinaryMapOwnedAnyV2TemporaryRootReceipt>,
    allowed_root_loads: BTreeSet<NodeId>,
    all_root_loads: Vec<(NodeId, ResolvedLocalId)>,
    invalid: bool,
}

impl<'a> OrdinaryMapOwnedAnyV2Checker<'a> {
    fn collect(
        cfg: &'a CallableCfg,
        body: &'a ResolvedBody,
        types: &'a ResolvedTypeTable,
    ) -> (
        Vec<OrdinaryMapOwnedAnyV2Action>,
        BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
        Option<OrdinaryMapOwnedAnyV2CfgReceipt>,
    ) {
        let mut checker = Self {
            body,
            types,
            actions: Vec::new(),
            operation_attempts: BTreeSet::new(),
            roots: BTreeMap::new(),
            temporary_roots: BTreeMap::new(),
            temporary_root_receipts: Vec::new(),
            allowed_root_loads: BTreeSet::new(),
            all_root_loads: Vec::new(),
            invalid: false,
        };

        // V2 has a dedicated owner-flow proof for one acyclic diamond. Keep
        // every other non-straight-line shape visible as an invalid
        // candidate; no V1 receipt is consulted here.
        if cfg.owner != body.owner {
            checker.invalid = true;
        }
        checker.visit_block(&body.root);
        // These typed expression roots are not walked through `body.root`.
        // A Map/Any operation hidden in a dynamic lvalue input or a callable
        // default must still be visible to the attempt census. Most such
        // operations have no source CFG point in this callable and therefore
        // fail the exact-point gate below; scanning them prevents an otherwise
        // receipted body from silently omitting an additional owner operation.
        for expression in body
            .place_inputs
            .values()
            .chain(body.default_values.values())
        {
            checker.visit_expr(expression, None);
        }
        checker.schedule_scope_drops(cfg);

        // Ordinary primitive Map traffic remains owned by the established
        // Map profile. V2 activates only when the callable actually carries
        // a DynamicAny observation (GetAny/Values) or stores a concrete
        // nominal payload. In particular, New/Set(i32)/Size/Drop must not
        // manufacture V2 roots merely because the broad attempt census saw
        // the same Map builtin names. Keep those attempts so a primitive
        // callable still conflicts with a separate V2 candidate at the
        // program-level admission boundary.
        let has_v2_authorizing_signal = checker.operation_attempts.iter().any(|attempt| {
            matches!(
                attempt.kind,
                OrdinaryMapOwnedAnyV2AttemptKind::GetAny | OrdinaryMapOwnedAnyV2AttemptKind::Values
            )
        }) || checker.actions.iter().any(|action| {
            action.kind == OrdinaryMapOwnedAnyV2ActionKind::Set
                && action
                    .value_type
                    .as_ref()
                    .is_some_and(|ty| checker.is_concrete_record(ty))
        });
        if !has_v2_authorizing_signal {
            return (Vec::new(), checker.operation_attempts, None);
        }

        if !Self::is_v2_straight_line_cfg(cfg)
            && Self::ordinary_map_owned_any_v2_diamond_shape(cfg).is_none()
        {
            checker.invalid = true;
        }

        if checker.roots.values().any(|dropped| !dropped) {
            checker.invalid = true;
        }
        for (node, local) in &checker.all_root_loads {
            if checker.roots.contains_key(local) && !checker.allowed_root_loads.contains(node) {
                checker.invalid = true;
            }
        }

        // One action is required for every attempt at the exact source node;
        // scope cleanup is evidence of an unreceipted owner and therefore
        // never becomes a v2 Drop action.
        let action_points = checker
            .actions
            .iter()
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        if checker
            .operation_attempts
            .iter()
            .any(|attempt| !action_points.contains(&attempt.point))
        {
            checker.invalid = true;
        }
        if checker
            .actions
            .iter()
            .any(|action| ordinary_map_owned_any_v2_cfg_point_count(cfg, &action.point) != 1)
            || checker
                .operation_attempts
                .iter()
                .any(|attempt| ordinary_map_owned_any_v2_cfg_point_count(cfg, &attempt.point) != 1)
        {
            checker.invalid = true;
        }
        for action in checker.actions.iter().filter(|action| {
            matches!(
                action.kind,
                OrdinaryMapOwnedAnyV2ActionKind::GetAny | OrdinaryMapOwnedAnyV2ActionKind::Values
            )
        }) {
            let expected_kind = match action.kind {
                OrdinaryMapOwnedAnyV2ActionKind::GetAny => OrdinaryMapOwnedAnyV2AttemptKind::GetAny,
                OrdinaryMapOwnedAnyV2ActionKind::Values => OrdinaryMapOwnedAnyV2AttemptKind::Values,
                _ => unreachable!("filtered V2 result action"),
            };
            let exact_attempt = checker
                .operation_attempts
                .iter()
                .find(|attempt| attempt.point == action.point && attempt.kind == expected_kind);
            if !exact_attempt.is_some_and(|attempt| {
                attempt.receiver_local.as_ref() == action.source_local.as_ref()
                    && attempt.result_local.as_ref() == action.result_local.as_ref()
                    && attempt.arguments.first() == action.source_node.as_ref()
                    && (action.kind == OrdinaryMapOwnedAnyV2ActionKind::Values
                        || attempt.arguments.get(1) == action.key.as_ref())
            }) {
                checker.invalid = true;
            }
        }
        for action in checker
            .actions
            .iter()
            .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Clone)
        {
            let clone_attempts = checker
                .operation_attempts
                .iter()
                .filter(|attempt| {
                    attempt.point == action.point
                        && attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::Clone
                })
                .collect::<Vec<_>>();
            let source_local = action.source_local.as_ref();
            let source_node = action.source_node.as_ref();
            let source_type = action.source_type.as_ref();
            let exact_attempt = clone_attempts.first().is_some_and(|attempt| {
                attempt.arguments.len() == 1
                    && attempt.arguments.first() == source_node
                    && attempt.receiver_local.as_ref() == source_local
                    && attempt.result_local.as_ref() == Some(&action.local)
            });
            let source_type_matches = source_local
                .and_then(|local| checker.body.locals.get(local))
                .is_some_and(|binding| Some(&binding.ty) == source_type);
            let destination_type_matches = checker
                .body
                .locals
                .get(&action.local)
                .is_some_and(|binding| Some(&binding.ty) == source_type);
            if clone_attempts.len() != 1
                || !exact_attempt
                || action.root != OrdinaryMapOwnedAnyV2Checker::resource_identity(&action.local)
                || action.source
                    != source_local.map(OrdinaryMapOwnedAnyV2Checker::resource_identity)
                || source_node != Some(&action.point)
                || !source_type_matches
                || !destination_type_matches
            {
                checker.invalid = true;
            }
        }
        let new_attempts = checker
            .operation_attempts
            .iter()
            .filter(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::New)
            .map(|attempt| attempt.point.clone())
            .collect::<BTreeSet<_>>();
        let new_actions = checker
            .actions
            .iter()
            .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::New)
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        if new_attempts.is_empty() || new_attempts != new_actions {
            checker.invalid = true;
        }

        let result_receipts =
            collect_ordinary_map_owned_any_v2_result_receipts(cfg, body, &checker.actions);
        let (unpack_receipts, values_unpack_receipts, unpack_attempts_match) =
            collect_ordinary_map_owned_any_v2_unpack_receipts(
                cfg,
                body,
                types,
                &checker.actions,
                &result_receipts,
            );
        if !unpack_attempts_match {
            checker.invalid = true;
        }
        let result_action_count = checker
            .actions
            .iter()
            .filter(|action| {
                matches!(
                    action.kind,
                    OrdinaryMapOwnedAnyV2ActionKind::GetAny
                        | OrdinaryMapOwnedAnyV2ActionKind::Values
                )
            })
            .count();
        if result_receipts.len() != result_action_count
            || result_receipts
                .iter()
                .any(|receipt| !receipt.settlement.is_receipted())
        {
            checker.invalid = true;
        }
        let flow = if Self::is_v2_straight_line_cfg(cfg) {
            None
        } else {
            let flow = Self::build_ordinary_map_owned_any_v2_flow_receipt(
                cfg,
                &checker.actions,
                &result_receipts,
                &unpack_receipts,
                &values_unpack_receipts,
                &checker.all_root_loads,
            );
            if flow.is_none() {
                checker.invalid = true;
            }
            flow
        };
        for temporary_root in &checker.temporary_root_receipts {
            let root_actions = checker
                .actions
                .iter()
                .filter(|action| action.local == temporary_root.local)
                .collect::<Vec<_>>();
            let construction = root_actions
                .iter()
                .filter(|action| {
                    action.kind == OrdinaryMapOwnedAnyV2ActionKind::New
                        && action.point == temporary_root.construction
                })
                .count();
            let get_any = root_actions
                .iter()
                .filter(|action| {
                    action.kind == OrdinaryMapOwnedAnyV2ActionKind::GetAny
                        && action.point == temporary_root.get_any
                        && action.source_node.as_ref() == Some(&temporary_root.source_node)
                        && action.source_type.as_ref() == Some(&temporary_root.source_type)
                })
                .count();
            let drop = root_actions
                .iter()
                .filter(|action| {
                    action.kind == OrdinaryMapOwnedAnyV2ActionKind::Drop
                        && action.point == temporary_root.settlement_point
                })
                .count();
            let matching_source = temporary_root.construction == temporary_root.source_node
                && temporary_root.root_type == temporary_root.source_type
                && temporary_root.get_any == temporary_root.settlement_point
                && temporary_root.settlement
                    == crate::core::OrdinaryMapOwnedAnyV2RootSettlementKind::ImmediateGetAnyDrop
                && temporary_root.root
                    == OrdinaryMapOwnedAnyV2Checker::resource_identity(&temporary_root.local);
            if construction != 1 || get_any != 1 || drop != 1 || !matching_source {
                checker.invalid = true;
            }
        }

        let cfg_order = Self::acyclic_v2_cfg_order(cfg).unwrap_or_default();
        let block_rank = cfg_order
            .iter()
            .enumerate()
            .map(|(rank, block)| (block.clone(), rank))
            .collect::<BTreeMap<_, _>>();
        let mut point_order = BTreeMap::new();
        for block_id in &cfg_order {
            if let Some(block) = cfg.blocks.get(block_id) {
                for (point_rank, point) in block.points.iter().enumerate() {
                    if point_order
                        .insert(
                            point.source.node.clone(),
                            (*block_rank.get(block_id).unwrap_or(&usize::MAX), point_rank),
                        )
                        .is_some()
                    {
                        checker.invalid = true;
                    }
                }
            }
        }
        checker.actions.sort_by_key(|action| {
            point_order
                .get(&action.point)
                .copied()
                .unwrap_or((usize::MAX, usize::MAX))
        });
        if checker
            .actions
            .iter()
            .any(|action| !point_order.contains_key(&action.point))
        {
            checker.invalid = true;
        }
        let cfg_receipt = (!checker.invalid).then(|| OrdinaryMapOwnedAnyV2CfgReceipt {
            profile_version: OrdinaryMapOwnedAnyV2ProfileVersion::V2,
            owner: body.owner.clone(),
            actions: checker.actions.clone(),
            attempts: checker.operation_attempts.clone(),
            result_receipts,
            unpack_receipts,
            values_unpack_receipts,
            temporary_root_receipts: checker.temporary_root_receipts.clone(),
            flow,
        });
        if checker.invalid {
            checker.actions.clear();
        }
        (checker.actions, checker.operation_attempts, cfg_receipt)
    }

    fn is_v2_straight_line_cfg(cfg: &CallableCfg) -> bool {
        cfg.blocks.len() == 1
            && cfg.blocks.contains_key(&cfg.entry)
            && cfg.reachable.len() == 1
            && cfg.reachable.contains(&cfg.entry)
            && cfg.edges.is_empty()
    }

    /// Return the exact split/join of the only multi-block V2 control-flow
    /// shape admitted by this slice. It is one entry split, two straight arm
    /// blocks, and one common return join. This is deliberately narrower than
    /// the V1 per-edge root flow proof.
    fn ordinary_map_owned_any_v2_diamond_shape(
        cfg: &CallableCfg,
    ) -> Option<(
        crate::core::cfg::BasicBlockId,
        crate::core::cfg::BasicBlockId,
        BTreeSet<crate::core::cfg::EdgeId>,
        BTreeSet<crate::core::cfg::BasicBlockId>,
    )> {
        use crate::core::cfg::{EdgeKind, Terminator};

        if cfg.blocks.len() != 4
            || cfg.reachable.len() != 4
            || !cfg.reachable.contains(&cfg.entry)
            || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
            || cfg.edges.len() != 4
            || cfg
                .entry
                .0
                 .0
                .rsplit_once("/cfg:block:")
                .map(|(_, role)| role)
                != Some("entry")
        {
            return None;
        }
        let split = cfg.blocks.get(&cfg.entry)?;
        let arm_edges = match &split.terminator {
            Terminator::Branch {
                then_edge,
                else_edge,
                ..
            } => [then_edge.clone(), else_edge.clone()]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            Terminator::Match { arms, .. } if arms.len() == 2 => {
                arms.iter().cloned().collect::<BTreeSet<_>>()
            }
            _ => return None,
        };
        if arm_edges.len() != 2 {
            return None;
        }
        let expected_join_role = match &split.terminator {
            Terminator::Branch { .. } => "if-join",
            Terminator::Match { .. } => "match-join",
            _ => return None,
        };
        let mut arm_blocks = BTreeSet::new();
        let mut joins = BTreeSet::new();
        for edge_id in &arm_edges {
            let edge = cfg.edges.get(edge_id)?;
            let kind_matches = match (&split.terminator, edge.kind) {
                (Terminator::Branch { then_edge, .. }, EdgeKind::Then) => edge_id == then_edge,
                (Terminator::Branch { else_edge, .. }, EdgeKind::Else) => edge_id == else_edge,
                (Terminator::Match { arms, .. }, EdgeKind::MatchArm) => arms.contains(edge_id),
                _ => false,
            };
            let expected_arm_role = match (&split.terminator, edge.kind) {
                (Terminator::Branch { .. }, EdgeKind::Then) => "if-then",
                (Terminator::Branch { .. }, EdgeKind::Else) => "if-else",
                (Terminator::Match { .. }, EdgeKind::MatchArm) => "match-arm",
                _ => return None,
            };
            if edge.from != cfg.entry || !kind_matches {
                return None;
            }
            let arm = cfg.blocks.get(&edge.to)?;
            if edge
                .to
                .0
                 .0
                .rsplit_once("/cfg:block:")
                .map(|(_, role)| role)
                != Some(expected_arm_role)
                || !arm_blocks.insert(edge.to.clone())
                || arm
                    .points
                    .iter()
                    .any(|point| point.kind == crate::core::cfg::CfgPointKind::Condition)
            {
                return None;
            }
            let Terminator::Goto { edge: join_edge_id } = &arm.terminator else {
                return None;
            };
            let join_edge = cfg.edges.get(join_edge_id)?;
            if join_edge.from != arm.id || join_edge.kind != EdgeKind::Fallthrough {
                return None;
            }
            joins.insert(join_edge.to.clone());
        }
        if joins.len() != 1 {
            return None;
        }
        let join = joins.into_iter().next()?;
        if arm_blocks.len() != 2
            || join.0 .0.rsplit_once("/cfg:block:").map(|(_, role)| role)
                != Some(expected_join_role)
            || !matches!(
                &cfg.blocks.get(&join)?.terminator,
                Terminator::Return { .. }
            )
            || cfg.blocks.get(&join)?.points.iter().any(|point| {
                cfg.blocks.values().any(|block| {
                    block.id != join
                        && block
                            .points
                            .iter()
                            .any(|other| other.source.node == point.source.node)
                })
            })
        {
            return None;
        }
        Some((cfg.entry.clone(), join, arm_edges, arm_blocks))
    }

    fn acyclic_v2_cfg_order(cfg: &CallableCfg) -> Option<Vec<crate::core::cfg::BasicBlockId>> {
        let mut indegree = cfg
            .reachable
            .iter()
            .cloned()
            .map(|block| (block, 0usize))
            .collect::<BTreeMap<_, _>>();
        for edge in cfg
            .edges
            .values()
            .filter(|edge| cfg.reachable.contains(&edge.from))
        {
            *indegree.get_mut(&edge.to)? += 1;
        }
        let mut ready = indegree
            .iter()
            .filter_map(|(block, degree)| (*degree == 0).then_some(block.clone()))
            .collect::<BTreeSet<_>>();
        let mut order = Vec::with_capacity(cfg.reachable.len());
        while let Some(block) = ready.pop_first() {
            order.push(block.clone());
            for edge in cfg.successors(&block) {
                if !cfg.reachable.contains(&edge.to) {
                    continue;
                }
                let degree = indegree.get_mut(&edge.to)?;
                *degree = degree.checked_sub(1)?;
                if *degree == 0 {
                    ready.insert(edge.to.clone());
                }
            }
        }
        (order.len() == cfg.reachable.len()).then_some(order)
    }

    fn build_ordinary_map_owned_any_v2_flow_receipt(
        cfg: &CallableCfg,
        actions: &[OrdinaryMapOwnedAnyV2Action],
        results: &[crate::core::OrdinaryMapOwnedAnyV2ResultReceipt],
        unpacks: &[crate::core::OrdinaryMapOwnedAnyV2UnpackReceipt],
        values_unpacks: &[crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackReceipt],
        local_loads: &[(NodeId, ResolvedLocalId)],
    ) -> Option<crate::core::OrdinaryMapOwnedAnyV2CfgFlowReceipt> {
        use crate::core::cfg::{BasicBlockId, Terminator};
        use crate::core::{
            OrdinaryMapOwnedAnyV2ActionKind as Action,
            OrdinaryMapOwnedAnyV2CfgBlock as ReceiptBlock, OrdinaryMapOwnedAnyV2LiveOwner as Owner,
            OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement,
        };

        let (split, join, arm_edges, arm_blocks) =
            Self::ordinary_map_owned_any_v2_diamond_shape(cfg)?;
        let order = Self::acyclic_v2_cfg_order(cfg)?;
        let reachable_blocks = cfg
            .reachable
            .iter()
            .map(|id| {
                let block = cfg.blocks.get(id)?;
                Some((
                    id.clone(),
                    ReceiptBlock {
                        source: block.source.clone(),
                        points: block
                            .points
                            .iter()
                            .map(|point| (point.source.node.clone(), point.kind))
                            .collect(),
                        terminator: block.terminator.clone(),
                    },
                ))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        let reachable_edges = cfg
            .edges
            .iter()
            .filter(|(_, edge)| cfg.reachable.contains(&edge.from))
            .map(|(id, edge)| (id.clone(), edge.clone()))
            .collect::<BTreeMap<_, _>>();
        if reachable_edges.len() != cfg.edges.len() {
            return None;
        }

        let mut point_location = BTreeMap::<NodeId, (BasicBlockId, usize)>::new();
        for block_id in &order {
            let block = cfg.blocks.get(block_id)?;
            for (point_rank, point) in block.points.iter().enumerate() {
                if point_location
                    .insert(point.source.node.clone(), (block_id.clone(), point_rank))
                    .is_some()
                {
                    return None;
                }
            }
        }
        let location = |point: &NodeId| point_location.get(point).map(|(block, _)| block);
        if actions
            .iter()
            .any(|action| location(&action.point).is_none())
            || results.iter().any(|result| {
                location(&result.operation).is_none()
                    || location(&result.binding_point).is_none()
                    || result
                        .settlement_point
                        .as_ref()
                        .is_none_or(|point| location(point).is_none())
            })
        {
            return None;
        }

        // Operations that create an owner and all result bindings belong to
        // the split's entry. The arms can only observe a pre-existing root
        // and the common join can only settle owners explicitly.
        for action in actions {
            let block = location(&action.point)?;
            match block {
                block if block == &split => {
                    if action.kind == Action::Drop {
                        return None;
                    }
                }
                block if arm_blocks.contains(block) => {
                    if !matches!(action.kind, Action::Size | Action::HasKey | Action::IsEmpty) {
                        return None;
                    }
                }
                block if block == &join => {
                    if action.kind != Action::Drop {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        for result in results {
            if location(&result.operation)? != &split
                || location(&result.binding_point)? != &split
                || result.settlement != Settlement::ExplicitDrop
                || location(result.settlement_point.as_ref()?)? != &join
            {
                return None;
            }
        }
        for unpack in unpacks {
            if arm_blocks.contains(location(&unpack.operation)?)
                || arm_blocks.contains(location(&unpack.pair_binding_point)?)
                || arm_blocks.contains(location(&unpack.unpack_binding_point)?)
                || arm_blocks.contains(location(&unpack.projection)?)
                || arm_blocks.contains(location(&unpack.cast)?)
                || arm_blocks.contains(location(&unpack.settlement_point)?)
            {
                return None;
            }
        }
        for unpack in values_unpacks {
            if arm_blocks.contains(location(&unpack.operation)?)
                || arm_blocks.contains(location(&unpack.snapshot_binding_point)?)
                || arm_blocks.contains(location(&unpack.unpack_binding_point)?)
                || arm_blocks.contains(location(&unpack.index_projection)?)
                || arm_blocks.contains(location(&unpack.cast)?)
                || arm_blocks.contains(location(&unpack.settlement_point)?)
            {
                return None;
            }
        }
        let result_bindings = results
            .iter()
            .map(|result| result.binding.clone())
            .collect::<BTreeSet<_>>();
        if local_loads.iter().any(|(point, local)| {
            result_bindings.contains(local)
                && location(point).is_some_and(|block| arm_blocks.contains(block))
        }) {
            return None;
        }

        let mut live_roots = BTreeSet::<ResolvedLocalId>::new();
        let mut live_results = BTreeSet::<(NodeId, Action, ResolvedLocalId)>::new();
        let mut live_owners_in = BTreeMap::new();
        let mut live_owners_out = BTreeMap::new();
        let mut live_owners_on_edge = BTreeMap::new();
        let owners = |roots: &BTreeSet<ResolvedLocalId>,
                      results: &BTreeSet<(NodeId, Action, ResolvedLocalId)>| {
            roots
                .iter()
                .map(|local| Owner::Root {
                    local: local.clone(),
                    resource: Self::resource_identity(local),
                })
                .chain(
                    results
                        .iter()
                        .map(|(operation, kind, binding)| Owner::Result {
                            operation: operation.clone(),
                            kind: *kind,
                            binding: binding.clone(),
                        }),
                )
                .collect::<BTreeSet<_>>()
        };

        for block_id in &order {
            let incoming = reachable_edges
                .iter()
                .filter(|(_, edge)| &edge.to == block_id)
                .map(|(edge_id, _)| edge_id)
                .collect::<Vec<_>>();
            if block_id == &cfg.entry {
                if !incoming.is_empty() {
                    return None;
                }
            } else {
                let mut incoming_facts = incoming
                    .iter()
                    .map(|edge| live_owners_on_edge.get(*edge).cloned())
                    .collect::<Option<Vec<_>>>()?;
                let first = incoming_facts.pop()?;
                if incoming_facts.iter().any(|fact| fact != &first) {
                    return None;
                }
                let (roots, results) = Self::ordinary_map_owned_any_v2_decode_live_owners(&first)?;
                live_roots = roots;
                live_results = results;
            }
            live_owners_in.insert(block_id.clone(), owners(&live_roots, &live_results));

            let block = cfg.blocks.get(block_id)?;
            for point in &block.points {
                let point_id = &point.source.node;
                for action in actions.iter().filter(|action| &action.point == point_id) {
                    match action.kind {
                        Action::New | Action::Clone | Action::Set | Action::Remove => {
                            if live_roots.contains(&action.local) {
                                return None;
                            }
                            if matches!(action.kind, Action::Clone | Action::Set | Action::Remove)
                                && !live_roots.contains(action.source_local.as_ref()?)
                            {
                                return None;
                            }
                            live_roots.insert(action.local.clone());
                        }
                        Action::Drop => {
                            if !live_roots.remove(&action.local) {
                                return None;
                            }
                        }
                        Action::Size | Action::HasKey | Action::IsEmpty => {
                            if !live_roots.contains(&action.local) {
                                return None;
                            }
                        }
                        Action::GetAny | Action::Values => {
                            if !live_roots.contains(&action.local) {
                                return None;
                            }
                        }
                    }
                }
                for result in results
                    .iter()
                    .filter(|result| &result.binding_point == point_id)
                {
                    if !live_results.insert((
                        result.operation.clone(),
                        result.kind,
                        result.binding.clone(),
                    )) {
                        return None;
                    }
                }
                for result in results
                    .iter()
                    .filter(|result| result.settlement_point.as_ref() == Some(point_id))
                {
                    if !live_results.remove(&(
                        result.operation.clone(),
                        result.kind,
                        result.binding.clone(),
                    )) {
                        return None;
                    }
                }
            }
            let live = owners(&live_roots, &live_results);
            live_owners_out.insert(block_id.clone(), live.clone());
            for (edge_id, edge) in &reachable_edges {
                if &edge.from == block_id {
                    live_owners_on_edge.insert(edge_id.clone(), live.clone());
                }
            }
        }
        let split_owners = live_owners_out.get(&split)?;
        if !split_owners
            .iter()
            .any(|owner| matches!(owner, Owner::Root { .. }))
            || !split_owners
                .iter()
                .any(|owner| matches!(owner, Owner::Result { .. }))
        {
            return None;
        }
        for edge in &arm_edges {
            let target = &reachable_edges.get(edge)?.to;
            if live_owners_on_edge.get(edge) != Some(split_owners)
                || live_owners_in.get(target) != Some(split_owners)
            {
                return None;
            }
        }
        if !live_owners_out.get(&join)?.is_empty() {
            return None;
        }
        for (block_id, block) in &reachable_blocks {
            if matches!(&block.terminator, Terminator::Return { .. })
                && !live_owners_out
                    .get(block_id)
                    .is_some_and(BTreeSet::is_empty)
            {
                return None;
            }
        }
        if live_owners_in.len() != reachable_blocks.len()
            || live_owners_out.len() != reachable_blocks.len()
            || live_owners_on_edge.len() != reachable_edges.len()
        {
            return None;
        }

        Some(crate::core::OrdinaryMapOwnedAnyV2CfgFlowReceipt {
            entry: cfg.entry.clone(),
            split,
            join,
            arm_edges,
            reachable_blocks,
            reachable_edges,
            live_owners_in,
            live_owners_out,
            live_owners_on_edge,
        })
    }

    fn ordinary_map_owned_any_v2_decode_live_owners(
        owners: &BTreeSet<crate::core::OrdinaryMapOwnedAnyV2LiveOwner>,
    ) -> Option<(
        BTreeSet<ResolvedLocalId>,
        BTreeSet<(NodeId, OrdinaryMapOwnedAnyV2ActionKind, ResolvedLocalId)>,
    )> {
        use crate::core::OrdinaryMapOwnedAnyV2LiveOwner as Owner;
        let mut roots = BTreeSet::new();
        let mut results = BTreeSet::new();
        for owner in owners {
            match owner {
                Owner::Root { local, resource }
                    if *resource == Self::resource_identity(local)
                        && roots.insert(local.clone()) => {}
                Owner::Result {
                    operation,
                    kind,
                    binding,
                } if matches!(
                    kind,
                    OrdinaryMapOwnedAnyV2ActionKind::GetAny
                        | OrdinaryMapOwnedAnyV2ActionKind::Values
                ) && results.insert((operation.clone(), *kind, binding.clone())) => {}
                _ => return None,
            }
        }
        Some((roots, results))
    }

    fn visit_block(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    let target = match &pattern.kind {
                        ResolvedPatternKind::Binding { local, .. } => Some(local.clone()),
                        _ => None,
                    };
                    if let Some(initializer) = initializer {
                        self.visit_expr(initializer, target.as_ref());
                    }
                }
                ResolvedStmtKind::Drop(places) => self.visit_drop(statement, places),
                ResolvedStmtKind::Assign { target, value, .. } => {
                    if self.roots.contains_key(&target.base) {
                        self.invalid = true;
                    }
                    self.visit_expr(value, None);
                }
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if value.is_some() {
                        self.invalid = true;
                    }
                    if let Some(value) = value {
                        self.visit_expr(value, None);
                    }
                }
                ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => self.visit_expr(value, None),
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.visit_expr(expression, None);
                    }
                }
                // Nested control flow, closures, and scopes require
                // path-local root facts not present in this first slice.
                ResolvedStmtKind::While { condition, body } => {
                    self.visit_expr(condition, None);
                    self.visit_block(body);
                    self.invalid = true;
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                }
                | ResolvedStmtKind::For {
                    iterable: initializer,
                    body,
                    ..
                } => {
                    self.visit_expr(initializer, None);
                    self.visit_block(body);
                    self.invalid = true;
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    self.visit_expr(initializer, None);
                    self.visit_block(then_block);
                    if let Some(else_block) = else_block {
                        self.visit_block(else_block);
                    }
                    self.invalid = true;
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    self.visit_block(body);
                    self.invalid = true;
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    self.visit_expr(value, None);
                    self.visit_block(body);
                    self.invalid = true;
                }
                ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {
                    self.invalid = true
                }
            }
        }
        if let Some(result) = &block.result {
            self.visit_expr(result, None);
        }
    }

    fn visit_drop(&mut self, statement: &ResolvedStmt, places: &[ResolvedPlace]) {
        for place in places {
            let Some(was_dropped) = self.roots.get(&place.base).copied() else {
                continue;
            };
            let local = place.base.clone();
            let point = statement.node_id.clone();
            self.operation_attempts
                .insert(OrdinaryMapOwnedAnyV2Attempt {
                    kind: OrdinaryMapOwnedAnyV2AttemptKind::Drop,
                    point: point.clone(),
                    arguments: vec![local.0.clone()],
                    receiver_local: Some(local.clone()),
                    result_local: None,
                });
            if !place.projections.is_empty() || was_dropped {
                self.invalid = true;
                continue;
            }
            self.roots.insert(local.clone(), true);
            self.actions.push(self.action(
                OrdinaryMapOwnedAnyV2ActionKind::Drop,
                point,
                local,
                None,
                None,
                None,
                None,
                false,
                None,
            ));
        }
    }

    fn visit_expr(&mut self, expression: &ResolvedExpr, direct_bind: Option<&ResolvedLocalId>) {
        if let ResolvedExprKind::Load(place) = &expression.kind {
            self.all_root_loads
                .push((expression.node_id.clone(), place.base.clone()));
            if let Some(target) = direct_bind.filter(|_| self.is_map_root(&expression.ty)) {
                self.visit_clone(expression, target);
            }
            return;
        }
        if let ResolvedExprKind::Call(call) = &expression.kind {
            let builtin = match &call.callee {
                ResolvedCallee::Builtin(name) => name.as_str(),
                _ => "",
            };
            match builtin {
                "map_new" => self.visit_new(expression, call, direct_bind),
                "map_set" => self.visit_set(expression, call, direct_bind),
                "map_remove" => self.visit_remove(expression, call, direct_bind),
                "map_size" => self.visit_size(expression, call),
                "has_key" => self.visit_has_key(expression, call),
                "is_empty" => self.visit_is_empty(expression, call),
                "map_get" => self.visit_get_any(expression, call, direct_bind),
                "keys" => self.record_unsupported(
                    expression,
                    call,
                    OrdinaryMapOwnedAnyV2AttemptKind::Keys,
                    direct_bind.cloned(),
                ),
                "values" => self.visit_values(expression, call, direct_bind),
                "to_list" => self.record_unsupported(
                    expression,
                    call,
                    OrdinaryMapOwnedAnyV2AttemptKind::ToList,
                    direct_bind.cloned(),
                ),
                "map_from_list" => self.record_unsupported(
                    expression,
                    call,
                    OrdinaryMapOwnedAnyV2AttemptKind::FromList,
                    direct_bind.cloned(),
                ),
                _ => {}
            }
            for argument in &call.arguments {
                self.visit_expr(&argument.value, None);
            }
            return;
        }
        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        self.visit_expr(value, None);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.visit_expr(value, None);
                if let ResolvedValueProjection::Index(index) = projection {
                    self.visit_expr(index, None);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                self.visit_expr(left, None);
                self.visit_expr(right, None);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.visit_expr(operand, None),
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.visit_expr(value, None);
                }
            }
            ResolvedExprKind::Map(values) => {
                self.operation_attempts
                    .insert(OrdinaryMapOwnedAnyV2Attempt {
                        kind: OrdinaryMapOwnedAnyV2AttemptKind::MapLiteral,
                        point: expression.node_id.clone(),
                        arguments: values
                            .iter()
                            .flat_map(|(key, value)| [key.node_id.clone(), value.node_id.clone()])
                            .collect(),
                        receiver_local: None,
                        result_local: direct_bind.cloned(),
                    });
                self.invalid = true;
                for (key, value) in values {
                    self.visit_expr(key, None);
                    self.visit_expr(value, None);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.visit_expr(value, None);
                self.visit_expr(iterable, None);
                if let Some(guard) = guard {
                    self.visit_expr(guard, None);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => self.visit_expr(receiver, None),
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.visit_expr(&field.value, None);
                }
                if let Some(rest) = rest {
                    self.visit_expr(rest, None);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => {
                self.visit_block(block);
                self.invalid = true;
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.visit_expr(condition, None);
                self.visit_block(then_block);
                self.visit_block(else_block);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee, None);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.visit_expr(guard, None);
                    }
                    if let ResolvedExprKind::Block(block) = &arm.body.kind {
                        // Match lowering represents an arm's ordinary block
                        // body as an expression wrapper. Walk that block with
                        // the same statement-aware visitor used for If arms;
                        // the CFG diamond proof below still rejects nested
                        // control flow and every arm-local owner mutation.
                        self.visit_block(block);
                    } else {
                        self.visit_expr(&arm.body, None);
                    }
                }
            }
            ResolvedExprKind::Range { start, end } => {
                self.visit_expr(start, None);
                self.visit_expr(end, None);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.visit_expr(target, None);
                if let Some(start) = start {
                    self.visit_expr(start, None);
                }
                if let Some(end) = end {
                    self.visit_expr(end, None);
                }
            }
            ResolvedExprKind::Lambda(_) => self.invalid = true,
            ResolvedExprKind::Load(_) | ResolvedExprKind::Call(_) => {
                // Handled by the early-return arms above.
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }

    fn visit_new(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let Some(local) = direct_bind else {
            if let Some((local, _, root_type)) =
                self.temporary_roots.get(&expression.node_id).cloned()
            {
                self.record_attempt_with_result(
                    expression,
                    call,
                    OrdinaryMapOwnedAnyV2AttemptKind::New,
                    Some(local.clone()),
                );
                if !call.arguments.is_empty()
                    || !self.is_map_root(&expression.ty)
                    || root_type != expression.ty
                {
                    self.invalid = true;
                    return;
                }
                self.actions.push(self.action(
                    OrdinaryMapOwnedAnyV2ActionKind::New,
                    expression.node_id.clone(),
                    local,
                    None,
                    None,
                    None,
                    None,
                    false,
                    None,
                ));
                return;
            }
            self.record_unsupported(
                expression,
                call,
                OrdinaryMapOwnedAnyV2AttemptKind::New,
                None,
            );
            return;
        };
        self.record_attempt(
            expression,
            call,
            OrdinaryMapOwnedAnyV2AttemptKind::New,
            Some(local),
        );
        if !call.arguments.is_empty()
            || !self.is_map_root(&expression.ty)
            || self.roots.contains_key(local)
            || !self
                .body
                .locals
                .get(local)
                .is_some_and(|binding| binding.ty == expression.ty)
        {
            self.invalid = true;
            return;
        }
        self.roots.insert(local.clone(), false);
        self.actions.push(self.action(
            OrdinaryMapOwnedAnyV2ActionKind::New,
            expression.node_id.clone(),
            local.clone(),
            None,
            None,
            None,
            None,
            false,
            None,
        ));
    }

    /// A direct local Map-root bind creates an independent owner obligation.
    /// The attempt is recorded before validation so projected and unknown
    /// source roots remain visible to the v2 admission gate.
    fn visit_clone(&mut self, expression: &ResolvedExpr, target: &ResolvedLocalId) {
        let direct_source = match &expression.kind {
            ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                Some(place.base.clone())
            }
            _ => None,
        };
        self.operation_attempts
            .insert(OrdinaryMapOwnedAnyV2Attempt {
                kind: OrdinaryMapOwnedAnyV2AttemptKind::Clone,
                point: expression.node_id.clone(),
                arguments: vec![expression.node_id.clone()],
                receiver_local: direct_source.clone(),
                result_local: Some(target.clone()),
            });

        let Some(source_local) = self.live_root_load(expression) else {
            self.invalid = true;
            return;
        };
        if self.roots.contains_key(target)
            || !self.is_map_root(&expression.ty)
            || !self
                .body
                .locals
                .get(&source_local)
                .is_some_and(|binding| binding.ty == expression.ty)
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|binding| binding.ty == expression.ty)
        {
            self.invalid = true;
            return;
        }

        self.allow_root_load(expression);
        self.roots.insert(target.clone(), false);
        let mut action = self.action(
            OrdinaryMapOwnedAnyV2ActionKind::Clone,
            expression.node_id.clone(),
            target.clone(),
            None,
            None,
            None,
            None,
            false,
            Some(source_local),
        );
        action.source_node = Some(expression.node_id.clone());
        action.source_type = Some(expression.ty.clone());
        self.actions.push(action);
    }

    fn visit_set(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let Some(target) = direct_bind else {
            self.record_unsupported(
                expression,
                call,
                OrdinaryMapOwnedAnyV2AttemptKind::Set,
                None,
            );
            return;
        };
        self.record_attempt(
            expression,
            call,
            OrdinaryMapOwnedAnyV2AttemptKind::Set,
            Some(target),
        );
        if call.arguments.len() != 3 || self.roots.contains_key(target) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let value = &call.arguments[2].value;
        let Some(source_local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_map_root(&source.ty)
            || !self.is_map_root(&expression.ty)
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|binding| binding.ty == expression.ty)
            || !self.valid_key(key)
            || !self.valid_payload(value)
        {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.roots.insert(target.clone(), false);
        let mut action = self.action(
            OrdinaryMapOwnedAnyV2ActionKind::Set,
            expression.node_id.clone(),
            target.clone(),
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Some(value.node_id.clone()),
            Some(value.ty.clone()),
            true,
            Some(source_local),
        );
        action.source_node = Some(source.node_id.clone());
        action.source_type = Some(source.ty.clone());
        action.key_local = self.direct_string_key_local(key);
        action.key_provenance = Some(self.key_provenance(key));
        self.actions.push(action);
    }

    fn visit_remove(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let Some(target) = direct_bind else {
            self.record_unsupported(
                expression,
                call,
                OrdinaryMapOwnedAnyV2AttemptKind::Remove,
                None,
            );
            return;
        };
        self.record_attempt(
            expression,
            call,
            OrdinaryMapOwnedAnyV2AttemptKind::Remove,
            Some(target),
        );
        if call.arguments.len() != 2 || self.roots.contains_key(target) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let Some(source_local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_map_root(&source.ty)
            || !self.is_map_root(&expression.ty)
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|binding| binding.ty == expression.ty)
            || !self.valid_key(key)
        {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.roots.insert(target.clone(), false);
        let mut action = self.action(
            OrdinaryMapOwnedAnyV2ActionKind::Remove,
            expression.node_id.clone(),
            target.clone(),
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            None,
            None,
            false,
            Some(source_local),
        );
        action.source_node = Some(source.node_id.clone());
        action.source_type = Some(source.ty.clone());
        action.key_local = self.direct_string_key_local(key);
        action.key_provenance = Some(self.key_provenance(key));
        self.actions.push(action);
    }

    fn visit_size(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        self.record_attempt(
            expression,
            call,
            OrdinaryMapOwnedAnyV2AttemptKind::Size,
            None,
        );
        if call.arguments.len() != 1 || !self.is_i32(&expression.ty) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let Some(local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_map_root(&source.ty) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        let mut action = self.action(
            OrdinaryMapOwnedAnyV2ActionKind::Size,
            expression.node_id.clone(),
            local,
            None,
            None,
            None,
            None,
            false,
            None,
        );
        action.source_node = Some(source.node_id.clone());
        action.source_type = Some(source.ty.clone());
        self.actions.push(action);
    }

    fn visit_has_key(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        self.record_attempt(
            expression,
            call,
            OrdinaryMapOwnedAnyV2AttemptKind::HasKey,
            None,
        );
        if call.arguments.len() != 2 || !self.is_bool(&expression.ty) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let Some(local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_map_root(&source.ty) || !self.valid_key(key) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        let mut action = self.action(
            OrdinaryMapOwnedAnyV2ActionKind::HasKey,
            expression.node_id.clone(),
            local,
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            None,
            None,
            false,
            None,
        );
        action.source_node = Some(source.node_id.clone());
        action.source_type = Some(source.ty.clone());
        action.key_local = self.direct_string_key_local(key);
        action.key_provenance = Some(self.key_provenance(key));
        self.actions.push(action);
    }

    fn visit_is_empty(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        self.record_attempt(
            expression,
            call,
            OrdinaryMapOwnedAnyV2AttemptKind::IsEmpty,
            None,
        );
        if call.arguments.len() != 1 || !self.is_bool(&expression.ty) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let Some(local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_map_root(&source.ty) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        let mut action = self.action(
            OrdinaryMapOwnedAnyV2ActionKind::IsEmpty,
            expression.node_id.clone(),
            local,
            None,
            None,
            None,
            None,
            false,
            None,
        );
        action.source_node = Some(source.node_id.clone());
        action.source_type = Some(source.ty.clone());
        self.actions.push(action);
    }

    fn is_get_any_tuple(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Tuple(elements))
                if elements.len() == 2
                    && self.is_bool(&elements[0])
                    && matches!(
                        self.types.get(&elements[1]),
                        Some(ResolvedType::DynamicAny { .. })
                    )
        )
    }

    fn is_values_list(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Nominal { item, arguments, .. })
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && matches!(
                        self.types.get(&arguments[0]),
                        Some(ResolvedType::DynamicAny { .. })
                    )
        )
    }

    fn visit_get_any(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let temporary_root = call.arguments.first().and_then(|argument| {
            let source = &argument.value;
            let ResolvedExprKind::Call(new_call) = &source.kind else {
                return None;
            };
            if !matches!(&new_call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "map_new")
                || !new_call.arguments.is_empty()
                || !self.is_map_root(&source.ty)
            {
                return None;
            }
            let local = Self::temporary_root_local(&source.node_id);
            self.temporary_roots.insert(
                source.node_id.clone(),
                (local.clone(), expression.node_id.clone(), source.ty.clone()),
            );
            Some((local, source.node_id.clone(), source.ty.clone()))
        });
        self.record_attempt(
            expression,
            call,
            OrdinaryMapOwnedAnyV2AttemptKind::GetAny,
            direct_bind,
        );
        let Some(result_local) = direct_bind else {
            self.invalid = true;
            return;
        };
        if call.arguments.len() != 2
            || !self.is_get_any_tuple(&expression.ty)
            || !self
                .body
                .locals
                .get(result_local)
                .is_some_and(|binding| binding.ty == expression.ty)
        {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let source_local = self
            .live_root_load(source)
            .or_else(|| temporary_root.as_ref().map(|(local, _, _)| local.clone()));
        let Some(source_local) = source_local else {
            self.invalid = true;
            return;
        };
        if !self.is_map_root(&source.ty) || !self.valid_key(key) {
            self.invalid = true;
            return;
        }
        if matches!(&source.kind, ResolvedExprKind::Load(_)) {
            self.allow_root_load(source);
        }
        let mut action = self.action(
            OrdinaryMapOwnedAnyV2ActionKind::GetAny,
            expression.node_id.clone(),
            source_local.clone(),
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            None,
            None,
            false,
            Some(source_local),
        );
        action.source_node = Some(source.node_id.clone());
        action.source_type = Some(source.ty.clone());
        action.result_type = Some(expression.ty.clone());
        action.result_local = Some(result_local.clone());
        action.key_local = self.direct_string_key_local(key);
        action.key_provenance = Some(self.key_provenance(key));
        self.actions.push(action);
        if let Some((local, construction, root_type)) = temporary_root {
            let root = Self::resource_identity(&local);
            self.temporary_root_receipts.push(
                crate::core::OrdinaryMapOwnedAnyV2TemporaryRootReceipt {
                    profile_version: OrdinaryMapOwnedAnyV2ProfileVersion::V2,
                    local: local.clone(),
                    root,
                    construction: construction.clone(),
                    root_type: root_type.clone(),
                    get_any: expression.node_id.clone(),
                    source_node: construction,
                    source_type: root_type,
                    settlement:
                        crate::core::OrdinaryMapOwnedAnyV2RootSettlementKind::ImmediateGetAnyDrop,
                    settlement_point: expression.node_id.clone(),
                },
            );
            self.operation_attempts
                .insert(OrdinaryMapOwnedAnyV2Attempt {
                    kind: OrdinaryMapOwnedAnyV2AttemptKind::Drop,
                    point: expression.node_id.clone(),
                    arguments: vec![local.0.clone()],
                    receiver_local: Some(local.clone()),
                    result_local: None,
                });
            self.actions.push(self.action(
                OrdinaryMapOwnedAnyV2ActionKind::Drop,
                expression.node_id.clone(),
                local,
                None,
                None,
                None,
                None,
                false,
                None,
            ));
        }
    }

    fn visit_values(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        self.record_attempt(
            expression,
            call,
            OrdinaryMapOwnedAnyV2AttemptKind::Values,
            direct_bind,
        );
        let Some(result_local) = direct_bind else {
            self.invalid = true;
            return;
        };
        if call.arguments.len() != 1
            || !self.is_values_list(&expression.ty)
            || !self
                .body
                .locals
                .get(result_local)
                .is_some_and(|binding| binding.ty == expression.ty)
        {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let Some(source_local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_map_root(&source.ty) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        let mut action = self.action(
            OrdinaryMapOwnedAnyV2ActionKind::Values,
            expression.node_id.clone(),
            source_local.clone(),
            None,
            None,
            None,
            None,
            false,
            Some(source_local),
        );
        action.source_node = Some(source.node_id.clone());
        action.source_type = Some(source.ty.clone());
        action.result_type = Some(expression.ty.clone());
        action.result_local = Some(result_local.clone());
        self.actions.push(action);
    }

    fn record_unsupported(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        kind: OrdinaryMapOwnedAnyV2AttemptKind,
        result_local: Option<ResolvedLocalId>,
    ) {
        self.record_attempt_with_result(expression, call, kind, result_local);
        self.invalid = true;
    }

    fn record_attempt(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        kind: OrdinaryMapOwnedAnyV2AttemptKind,
        result_local: Option<&ResolvedLocalId>,
    ) {
        self.record_attempt_with_result(expression, call, kind, result_local.cloned());
    }

    fn record_attempt_with_result(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        kind: OrdinaryMapOwnedAnyV2AttemptKind,
        result_local: Option<ResolvedLocalId>,
    ) {
        let arguments = call
            .arguments
            .iter()
            .map(|argument| argument.value.node_id.clone())
            .collect::<Vec<_>>();
        let receiver_local =
            call.arguments
                .first()
                .and_then(|argument| match &argument.value.kind {
                    ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                        Some(place.base.clone())
                    }
                    _ => self
                        .temporary_roots
                        .get(&argument.value.node_id)
                        .map(|(local, _, _)| local.clone()),
                });
        self.operation_attempts
            .insert(OrdinaryMapOwnedAnyV2Attempt {
                kind,
                point: expression.node_id.clone(),
                arguments,
                receiver_local,
                result_local,
            });
    }

    fn schedule_scope_drops(&mut self, cfg: &CallableCfg) {
        let remaining = self
            .roots
            .iter()
            .filter_map(|(local, dropped)| (!*dropped).then_some(local.clone()))
            .collect::<Vec<_>>();
        let Some(point) = cfg
            .blocks
            .get(&cfg.entry)
            .and_then(|block| block.points.last())
            .map(|point| point.source.node.clone())
        else {
            if !remaining.is_empty() {
                self.invalid = true;
            }
            return;
        };
        for local in remaining {
            self.operation_attempts
                .insert(OrdinaryMapOwnedAnyV2Attempt {
                    kind: OrdinaryMapOwnedAnyV2AttemptKind::ScopeDrop,
                    point: point.clone(),
                    arguments: vec![local.0.clone()],
                    receiver_local: Some(local),
                    result_local: None,
                });
            self.invalid = true;
        }
    }

    fn live_root_load(&self, expression: &ResolvedExpr) -> Option<ResolvedLocalId> {
        let ResolvedExprKind::Load(place) = &expression.kind else {
            return None;
        };
        if !place.projections.is_empty() || self.roots.get(&place.base) != Some(&false) {
            return None;
        }
        Some(place.base.clone())
    }

    fn allow_root_load(&mut self, expression: &ResolvedExpr) {
        self.allowed_root_loads.insert(expression.node_id.clone());
    }

    fn direct_string_key_local(&self, expression: &ResolvedExpr) -> Option<ResolvedLocalId> {
        if !self.is_string(&expression.ty) {
            return None;
        }
        let ResolvedExprKind::Load(place) = &expression.kind else {
            return None;
        };
        if !place.projections.is_empty()
            || !self
                .body
                .locals
                .get(&place.base)
                .is_some_and(|binding| binding.ty == expression.ty)
        {
            return None;
        }
        Some(place.base.clone())
    }

    fn key_provenance(
        &self,
        expression: &ResolvedExpr,
    ) -> crate::core::OrdinaryMapOwnedAnyV2KeyProvenance {
        use crate::core::OrdinaryMapOwnedAnyV2KeyProvenance as Provenance;
        match &expression.kind {
            ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                Provenance::DirectLocal
            }
            ResolvedExprKind::Literal(ResolvedLiteral::String(_)) => Provenance::StringLiteral,
            _ => Provenance::ComputedString,
        }
    }

    fn valid_key(&self, expression: &ResolvedExpr) -> bool {
        // The precise resolved type is the Checker boundary. A direct local
        // is recorded separately so consumers can borrow it; every other
        // String expression is an owned compiler temporary that MIR must
        // lower and settle exactly once.
        self.is_string(&expression.ty)
    }

    fn valid_payload(&self, expression: &ResolvedExpr) -> bool {
        if self.is_i32(&expression.ty) || self.is_string(&expression.ty) {
            return match &expression.kind {
                ResolvedExprKind::Literal(ResolvedLiteral::Int(_) | ResolvedLiteral::String(_)) => {
                    true
                }
                ResolvedExprKind::Load(place) => place.projections.is_empty(),
                _ => false,
            };
        }
        if !self.is_concrete_record(&expression.ty) {
            return false;
        }
        match &expression.kind {
            ResolvedExprKind::Load(place) => place.projections.is_empty(),
            ResolvedExprKind::Record { rest: None, .. } => true,
            _ => false,
        }
    }

    fn is_map_root(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
        )
    }

    fn is_concrete_record(&self, ty: &ResolvedTypeId) -> bool {
        match self.types.get(ty) {
            Some(ResolvedType::Nominal {
                item, arguments, ..
            }) => !item.as_str().starts_with("builtin:type:") && arguments.is_empty(),
            _ => false,
        }
    }

    fn is_i32(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
        )
    }

    fn is_string(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
        )
    }

    fn is_bool(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::Bool))
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn action(
        &self,
        kind: OrdinaryMapOwnedAnyV2ActionKind,
        point: NodeId,
        local: ResolvedLocalId,
        key: Option<NodeId>,
        key_type: Option<ResolvedTypeId>,
        value: Option<NodeId>,
        value_type: Option<ResolvedTypeId>,
        payload_clone: bool,
        source_local: Option<ResolvedLocalId>,
    ) -> OrdinaryMapOwnedAnyV2Action {
        let root = Self::resource_identity(&local);
        let source = source_local.as_ref().map(Self::resource_identity);
        OrdinaryMapOwnedAnyV2Action {
            profile_version: OrdinaryMapOwnedAnyV2ProfileVersion::V2,
            kind,
            point,
            local,
            root,
            key,
            key_type,
            key_local: None,
            key_provenance: None,
            value,
            value_type,
            payload_clone,
            source_local,
            source,
            source_node: None,
            source_type: None,
            result_type: None,
            result_local: None,
        }
    }

    fn temporary_root_local(construction: &NodeId) -> ResolvedLocalId {
        ResolvedLocalId(NodeId(format!(
            "ordinary-map-owned-any-v2:temporary-root:{}",
            construction.0
        )))
    }

    fn resource_identity(local: &ResolvedLocalId) -> ResourceId {
        ResourceId(NodeId(format!("ordinary-map-owned-any-v2:{}", local.0 .0)))
    }
}

#[derive(Debug, Clone)]
struct OrdinaryMapOwnedAnyV2ResultUseState {
    action: OrdinaryMapOwnedAnyV2Action,
    observed_point: Option<NodeId>,
    binding_point: Option<NodeId>,
    settlement: Option<(OrdinaryMapOwnedAnyV2ResultSettlementKind, NodeId)>,
}

fn ordinary_map_owned_any_v2_cfg_point_count(cfg: &CallableCfg, node: &NodeId) -> usize {
    cfg.blocks
        .values()
        .flat_map(|block| &block.points)
        .filter(|point| &point.source.node == node)
        .count()
}

fn ordinary_map_owned_any_v2_cfg_point_index(cfg: &CallableCfg, node: &NodeId) -> Option<usize> {
    let order = OrdinaryMapOwnedAnyV2Checker::acyclic_v2_cfg_order(cfg)?;
    let mut offset = 0usize;
    for block_id in order {
        let block = cfg.blocks.get(&block_id)?;
        if let Some(index) = block
            .points
            .iter()
            .position(|point| &point.source.node == node)
        {
            return offset.checked_add(index);
        }
        offset = offset.checked_add(block.points.len())?;
    }
    None
}

fn ordinary_map_owned_any_v2_unpack_target_supported(
    types: &ResolvedTypeTable,
    ty: &ResolvedTypeId,
) -> bool {
    match types.get(ty) {
        Some(ResolvedType::Primitive(
            crate::core::PrimitiveType::I32 | crate::core::PrimitiveType::String,
        )) => true,
        // The ResolvedTypeTable intentionally carries only canonical type
        // identity, not declaration fields. The MIR receipt performs the
        // structural gate: this nominal must resolve to the payload selector's
        // single owned `text: string` record TypeDesc. Generic and erased
        // builtin Record values remain outside the Checker target allowlist.
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) => arguments.is_empty() && item.as_str() != "builtin:type:Record",
        _ => false,
    }
}

/// Classify the owner obligation for a Values-unpacked binding. This is kept
/// separate from the Values snapshot receipt: the snapshot's existing explicit
/// Drop remains unchanged, while the unpacked payload gets its own narrow
/// disposition proof.
fn ordinary_map_owned_any_v2_values_unpack_owner_settlement(
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    unpack_binding: &ResolvedLocalId,
    unpack_binding_point: &NodeId,
    unpack_type: &ResolvedTypeId,
) -> Option<(
    crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind,
    Option<NodeId>,
    Option<usize>,
)> {
    use crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind as Settlement;

    match types.get(unpack_type) {
        Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32)) => {
            Some((Settlement::NoOwnedPayload, None, None))
        }
        Some(ResolvedType::Primitive(crate::core::PrimitiveType::String)) => {
            body.root.result.as_ref().map(|result| {
                (
                    Settlement::CompilerOwnedStringDrop,
                    Some(result.node_id.clone()),
                    None,
                )
            })
        }
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) if arguments.is_empty() && !item.as_str().starts_with("builtin:type:") => {
            let binding_order = body
                .root
                .statements
                .iter()
                .position(|statement| &statement.node_id == unpack_binding_point)?;
            let mut whole_drops = Vec::new();
            let mut has_projected_drop = false;
            for (statement_order, statement) in body.root.statements.iter().enumerate() {
                let ResolvedStmtKind::Drop(places) = &statement.kind else {
                    continue;
                };
                for (place_index, place) in places.iter().enumerate() {
                    if &place.base != unpack_binding {
                        continue;
                    }
                    if place.projections.is_empty() {
                        whole_drops.push((statement_order, statement.node_id.clone(), place_index));
                    } else {
                        has_projected_drop = true;
                    }
                }
            }
            if has_projected_drop || whole_drops.len() != 1 {
                return None;
            }
            let (drop_order, drop_point, drop_index) = whole_drops.pop()?;
            (binding_order < drop_order).then_some((
                Settlement::ExplicitWholeDrop,
                Some(drop_point),
                Some(drop_index),
            ))
        }
        _ => None,
    }
}

/// Prove that the GetAny receiver's own persistent root lineage selected the
/// requested payload type. A Set on an unrelated Map in the same callable is
/// not authority to unpack this root's erased value as that type.
fn ordinary_map_owned_any_v2_root_selects_payload_type(
    actions: &[OrdinaryMapOwnedAnyV2Action],
    root: &ResolvedLocalId,
    ty: &ResolvedTypeId,
) -> bool {
    let mut current = root.clone();
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return false;
        }
        let root_actions = actions
            .iter()
            .filter(|action| {
                action.local == current
                    && matches!(
                        action.kind,
                        OrdinaryMapOwnedAnyV2ActionKind::New
                            | OrdinaryMapOwnedAnyV2ActionKind::Clone
                            | OrdinaryMapOwnedAnyV2ActionKind::Set
                            | OrdinaryMapOwnedAnyV2ActionKind::Remove
                    )
            })
            .collect::<Vec<_>>();
        let [root_action] = root_actions.as_slice() else {
            return false;
        };
        match root_action.kind {
            OrdinaryMapOwnedAnyV2ActionKind::New => return false,
            OrdinaryMapOwnedAnyV2ActionKind::Set => {
                if root_action.payload_clone && root_action.value_type.as_ref() == Some(ty) {
                    return true;
                }
                let Some(source) = root_action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            OrdinaryMapOwnedAnyV2ActionKind::Remove => {
                let Some(source) = root_action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            OrdinaryMapOwnedAnyV2ActionKind::Clone => {
                let Some(source) = root_action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            _ => unreachable!("root action filter is exhaustive"),
        }
    }
}

/// Return the exact root action lineage which selected `ty`. The final entry
/// is the Set whose cloned payload supplied the authority; preceding entries
/// are newer Set/Remove versions or Clone edges leading back to that Set.
fn ordinary_map_owned_any_v2_payload_selection_lineage(
    actions: &[OrdinaryMapOwnedAnyV2Action],
    root: &ResolvedLocalId,
    ty: &ResolvedTypeId,
) -> Option<Vec<NodeId>> {
    let mut current = root.clone();
    let mut seen = BTreeSet::new();
    let mut lineage = Vec::new();
    loop {
        if !seen.insert(current.clone()) {
            return None;
        }
        let root_actions = actions
            .iter()
            .filter(|action| {
                action.local == current
                    && matches!(
                        action.kind,
                        OrdinaryMapOwnedAnyV2ActionKind::New
                            | OrdinaryMapOwnedAnyV2ActionKind::Clone
                            | OrdinaryMapOwnedAnyV2ActionKind::Set
                            | OrdinaryMapOwnedAnyV2ActionKind::Remove
                    )
            })
            .collect::<Vec<_>>();
        let [root_action] = root_actions.as_slice() else {
            return None;
        };
        match root_action.kind {
            OrdinaryMapOwnedAnyV2ActionKind::New => return None,
            OrdinaryMapOwnedAnyV2ActionKind::Set => {
                lineage.push(root_action.point.clone());
                if root_action.payload_clone && root_action.value_type.as_ref() == Some(ty) {
                    return Some(lineage);
                }
                current = root_action.source_local.as_ref()?.clone();
            }
            OrdinaryMapOwnedAnyV2ActionKind::Remove => {
                lineage.push(root_action.point.clone());
                current = root_action.source_local.as_ref()?.clone();
            }
            OrdinaryMapOwnedAnyV2ActionKind::Clone => {
                lineage.push(root_action.point.clone());
                current = root_action.source_local.as_ref()?.clone();
            }
            _ => unreachable!("root action filter is exhaustive"),
        }
    }
}

impl OrdinaryMapOwnedAnyV2ResultUseState {
    fn observe(&mut self, point: &NodeId) {
        self.observed_point.get_or_insert_with(|| point.clone());
        if self.settlement.as_ref().is_some_and(|(kind, _)| {
            *kind == OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        }) {
            self.settlement = Some((
                OrdinaryMapOwnedAnyV2ResultSettlementKind::Unsettled,
                point.clone(),
            ));
        }
    }

    fn settle(&mut self, kind: OrdinaryMapOwnedAnyV2ResultSettlementKind, point: &NodeId) {
        if self.settlement.is_some() {
            self.settlement = Some((
                OrdinaryMapOwnedAnyV2ResultSettlementKind::DoubleSettlement,
                point.clone(),
            ));
        } else {
            self.settlement = Some((kind, point.clone()));
        }
    }
}

/// Collect the admitted direct source forms for DynamicAny unpacking. GetAny
/// still requires its exact tuple field-1 projection and pair Drop; Values
/// separately admits direct indexing of a directly-bound snapshot and its
/// exact snapshot Drop. Every DynamicAnyUnpack in the callable is counted,
/// so any other cast shape withholds the whole V2 candidate.
fn collect_ordinary_map_owned_any_v2_unpack_receipts(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    actions: &[OrdinaryMapOwnedAnyV2Action],
    result_receipts: &[OrdinaryMapOwnedAnyV2ResultReceipt],
) -> (
    Vec<crate::core::ownership::OrdinaryMapOwnedAnyV2UnpackReceipt>,
    Vec<crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesUnpackReceipt>,
    bool,
) {
    #[derive(Clone)]
    struct Attempt {
        cast: NodeId,
        projection: NodeId,
        cast_type: ResolvedTypeId,
        source_type: ResolvedTypeId,
        source_expression: ResolvedExpr,
        source_place: Option<ResolvedPlace>,
        conversion_kind: crate::core::ir::CheckedConversionKind,
        conversion_from: ResolvedTypeId,
        conversion_to: ResolvedTypeId,
        binding: Option<(NodeId, ResolvedLocalId, ResolvedTypeId)>,
    }

    fn binding_target(
        body: &ResolvedBody,
        pattern: &ResolvedPattern,
        statement: &ResolvedStmt,
    ) -> Option<(NodeId, ResolvedLocalId, ResolvedTypeId)> {
        let ResolvedPatternKind::Binding {
            local,
            by_reference: None,
        } = &pattern.kind
        else {
            return None;
        };
        let ty = body.locals.get(local)?.ty.clone();
        (pattern.ty == ty).then(|| (statement.node_id.clone(), local.clone(), ty))
    }

    fn record_conversion_attempt(
        owner: &NodeId,
        source: &ResolvedExpr,
        conversion: &crate::core::ir::CheckedConversion,
        attempts: &mut Vec<Attempt>,
    ) {
        if conversion.kind != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack {
            return;
        }
        attempts.push(Attempt {
            cast: owner.clone(),
            projection: source.node_id.clone(),
            cast_type: conversion.to.clone(),
            source_type: source.ty.clone(),
            source_expression: source.clone(),
            source_place: match &source.kind {
                ResolvedExprKind::Load(place) => Some(place.clone()),
                _ => None,
            },
            conversion_kind: conversion.kind,
            conversion_from: conversion.from.clone(),
            conversion_to: conversion.to.clone(),
            binding: None,
        });
    }

    fn visit_expr(
        expression: &ResolvedExpr,
        direct_binding: Option<(NodeId, ResolvedLocalId, ResolvedTypeId)>,
        body: &ResolvedBody,
        actions: &[OrdinaryMapOwnedAnyV2Action],
        pair_bindings: &mut BTreeMap<NodeId, (NodeId, ResolvedLocalId)>,
        attempts: &mut Vec<Attempt>,
        seen: &mut BTreeSet<NodeId>,
    ) {
        if !seen.insert(expression.node_id.clone()) {
            return;
        }

        if let ResolvedExprKind::Cast { value, conversion } = &expression.kind {
            if conversion.kind == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack {
                let source_place = match &value.kind {
                    ResolvedExprKind::Load(place) => Some(place.clone()),
                    _ => None,
                };
                attempts.push(Attempt {
                    cast: expression.node_id.clone(),
                    projection: value.node_id.clone(),
                    cast_type: expression.ty.clone(),
                    source_type: value.ty.clone(),
                    source_expression: (**value).clone(),
                    source_place,
                    conversion_kind: conversion.kind,
                    conversion_from: conversion.from.clone(),
                    conversion_to: conversion.to.clone(),
                    binding: direct_binding,
                });
            }
            visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
            return;
        }

        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
                if let ResolvedValueProjection::Index(index) = projection {
                    visit_expr(index, None, body, actions, pair_bindings, attempts, seen);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                visit_expr(left, None, body, actions, pair_bindings, attempts, seen);
                visit_expr(right, None, body, actions, pair_bindings, attempts, seen);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => {
                visit_expr(operand, None, body, actions, pair_bindings, attempts, seen)
            }
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    record_conversion_attempt(
                        &expression.node_id,
                        &argument.value,
                        &argument.conversion,
                        attempts,
                    );
                    visit_expr(
                        &argument.value,
                        None,
                        body,
                        actions,
                        pair_bindings,
                        attempts,
                        seen,
                    );
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
                }
            }
            ResolvedExprKind::Map(values) => {
                for (key, value) in values {
                    visit_expr(key, None, body, actions, pair_bindings, attempts, seen);
                    visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
                visit_expr(iterable, None, body, actions, pair_bindings, attempts, seen);
                if let Some(guard) = guard {
                    visit_expr(guard, None, body, actions, pair_bindings, attempts, seen);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => {
                visit_expr(receiver, None, body, actions, pair_bindings, attempts, seen)
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    record_conversion_attempt(
                        &expression.node_id,
                        &field.value,
                        &field.conversion,
                        attempts,
                    );
                    visit_expr(
                        &field.value,
                        None,
                        body,
                        actions,
                        pair_bindings,
                        attempts,
                        seen,
                    );
                }
                if let Some(rest) = rest {
                    visit_expr(rest, None, body, actions, pair_bindings, attempts, seen);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => {
                visit_block(block, body, actions, pair_bindings, attempts, seen)
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_expr(
                    condition,
                    None,
                    body,
                    actions,
                    pair_bindings,
                    attempts,
                    seen,
                );
                visit_block(then_block, body, actions, pair_bindings, attempts, seen);
                visit_block(else_block, body, actions, pair_bindings, attempts, seen);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                visit_expr(
                    scrutinee,
                    None,
                    body,
                    actions,
                    pair_bindings,
                    attempts,
                    seen,
                );
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit_expr(guard, None, body, actions, pair_bindings, attempts, seen);
                    }
                    visit_expr(
                        &arm.body,
                        None,
                        body,
                        actions,
                        pair_bindings,
                        attempts,
                        seen,
                    );
                }
            }
            ResolvedExprKind::Range { start, end } => {
                visit_expr(start, None, body, actions, pair_bindings, attempts, seen);
                visit_expr(end, None, body, actions, pair_bindings, attempts, seen);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                visit_expr(target, None, body, actions, pair_bindings, attempts, seen);
                if let Some(start) = start {
                    visit_expr(start, None, body, actions, pair_bindings, attempts, seen);
                }
                if let Some(end) = end {
                    visit_expr(end, None, body, actions, pair_bindings, attempts, seen);
                }
            }
            ResolvedExprKind::Lambda(lambda) => {
                visit_block(&lambda.body, body, actions, pair_bindings, attempts, seen)
            }
            ResolvedExprKind::Load(_)
            | ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
            ResolvedExprKind::Cast { .. } => unreachable!("casts handled above"),
        }
    }

    fn visit_block(
        block: &ResolvedBlock,
        body: &ResolvedBody,
        actions: &[OrdinaryMapOwnedAnyV2Action],
        pair_bindings: &mut BTreeMap<NodeId, (NodeId, ResolvedLocalId)>,
        attempts: &mut Vec<Attempt>,
        seen: &mut BTreeSet<NodeId>,
    ) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    let binding = binding_target(body, pattern, statement);
                    if let (Some((binding_point, local, _)), Some(initializer)) =
                        (&binding, initializer)
                    {
                        let result_action_kind = match &initializer.kind {
                            ResolvedExprKind::Call(ResolvedCall {
                                callee: ResolvedCallee::Builtin(name),
                                ..
                            }) if name.as_str() == "map_get" => {
                                Some(OrdinaryMapOwnedAnyV2ActionKind::GetAny)
                            }
                            ResolvedExprKind::Call(ResolvedCall {
                                callee: ResolvedCallee::Builtin(name),
                                ..
                            }) if name.as_str() == "values" => {
                                Some(OrdinaryMapOwnedAnyV2ActionKind::Values)
                            }
                            _ => None,
                        };
                        if result_action_kind.is_some_and(|kind| {
                            actions.iter().any(|action| {
                                action.kind == kind
                                    && action.point == initializer.node_id
                                    && action.result_local.as_ref() == Some(local)
                            })
                        }) {
                            pair_bindings.insert(
                                initializer.node_id.clone(),
                                (binding_point.clone(), local.clone()),
                            );
                        }
                    }
                    if let Some(initializer) = initializer {
                        visit_expr(
                            initializer,
                            binding,
                            body,
                            actions,
                            pair_bindings,
                            attempts,
                            seen,
                        );
                    }
                }
                ResolvedStmtKind::Assign {
                    value, conversion, ..
                } => {
                    record_conversion_attempt(&statement.node_id, value, conversion, attempts);
                    visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
                }
                ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => visit_expr(value, None, body, actions, pair_bindings, attempts, seen),
                ResolvedStmtKind::Return { value, conversion } => {
                    if let Some(value) = value {
                        if let Some(conversion) = conversion {
                            record_conversion_attempt(
                                &statement.node_id,
                                value,
                                conversion,
                                attempts,
                            );
                        }
                        visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
                    }
                }
                ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
                    }
                }
                ResolvedStmtKind::While {
                    condition,
                    body: nested,
                } => {
                    visit_expr(
                        condition,
                        None,
                        body,
                        actions,
                        pair_bindings,
                        attempts,
                        seen,
                    );
                    visit_block(nested, body, actions, pair_bindings, attempts, seen);
                }
                ResolvedStmtKind::WhileLet {
                    initializer,
                    body: nested,
                    ..
                }
                | ResolvedStmtKind::For {
                    iterable: initializer,
                    body: nested,
                    ..
                } => {
                    visit_expr(
                        initializer,
                        None,
                        body,
                        actions,
                        pair_bindings,
                        attempts,
                        seen,
                    );
                    visit_block(nested, body, actions, pair_bindings, attempts, seen);
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    visit_expr(
                        initializer,
                        None,
                        body,
                        actions,
                        pair_bindings,
                        attempts,
                        seen,
                    );
                    visit_block(then_block, body, actions, pair_bindings, attempts, seen);
                    if let Some(else_block) = else_block {
                        visit_block(else_block, body, actions, pair_bindings, attempts, seen);
                    }
                }
                ResolvedStmtKind::Loop(nested) | ResolvedStmtKind::Scope { body: nested, .. } => {
                    visit_block(nested, body, actions, pair_bindings, attempts, seen)
                }
                ResolvedStmtKind::Pinned {
                    value,
                    body: nested,
                    ..
                } => {
                    visit_expr(value, None, body, actions, pair_bindings, attempts, seen);
                    visit_block(nested, body, actions, pair_bindings, attempts, seen);
                }
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        visit_expr(
                            expression,
                            None,
                            body,
                            actions,
                            pair_bindings,
                            attempts,
                            seen,
                        );
                    }
                }
                ResolvedStmtKind::Drop(_)
                | ResolvedStmtKind::Continue
                | ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            visit_expr(result, None, body, actions, pair_bindings, attempts, seen);
        }
    }

    let mut pair_bindings = BTreeMap::new();
    let mut attempts = Vec::new();
    let mut seen = BTreeSet::new();
    visit_block(
        &body.root,
        body,
        actions,
        &mut pair_bindings,
        &mut attempts,
        &mut seen,
    );
    for expression in body
        .place_inputs
        .values()
        .chain(body.default_values.values())
    {
        visit_expr(
            expression,
            None,
            body,
            actions,
            &mut pair_bindings,
            &mut attempts,
            &mut seen,
        );
    }
    let statement_order = body
        .root
        .statements
        .iter()
        .enumerate()
        .map(|(index, statement)| (statement.node_id.clone(), index))
        .collect::<BTreeMap<_, _>>();

    let mut receipts = Vec::new();
    let mut values_receipts = Vec::new();
    let mut receipted_operations = BTreeSet::new();
    let mut receipted_values_casts = BTreeSet::new();
    let mut all_attempts_match = true;
    for attempt in attempts {
        let Some((unpack_binding_point, unpack_binding, unpack_binding_type)) =
            attempt.binding.clone()
        else {
            all_attempts_match = false;
            continue;
        };

        // Values is a separate source contract from GetAny's tuple field-1
        // projection. Admit only `let item = snapshot[index] as T`, where the
        // indexed base is the direct local bound from this exact Values call.
        if let ResolvedExprKind::Project {
            value,
            projection: ResolvedValueProjection::Index(index),
        } = &attempt.source_expression.kind
        {
            let index_local = match &index.kind {
                ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                    Some(place.base.clone())
                }
                _ => None,
            };
            let index_local_type_matches = index_local.as_ref().is_none_or(|local_id| {
                body.locals
                    .get(local_id)
                    .is_some_and(|local| local.ty == index.ty)
            });
            let unsupported_projected_index = match &index.kind {
                ResolvedExprKind::Project { .. } => true,
                ResolvedExprKind::Load(place) => !place.projections.is_empty(),
                _ => false,
            };
            let snapshot_binding = match &value.kind {
                ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                    Some(place.base.clone())
                }
                _ => None,
            };
            let Some(snapshot_binding) = snapshot_binding else {
                all_attempts_match = false;
                continue;
            };
            let mut matching_actions = actions.iter().filter(|action| {
                action.kind == OrdinaryMapOwnedAnyV2ActionKind::Values
                    && action.result_local.as_ref() == Some(&snapshot_binding)
            });
            let Some(action) = matching_actions.next() else {
                all_attempts_match = false;
                continue;
            };
            if matching_actions.next().is_some() {
                all_attempts_match = false;
                continue;
            }
            let Some((snapshot_binding_point, recorded_snapshot)) =
                pair_bindings.get(&action.point)
            else {
                all_attempts_match = false;
                continue;
            };
            let Some(result_receipt) = result_receipts
                .iter()
                .find(|receipt| receipt.operation == action.point)
            else {
                all_attempts_match = false;
                continue;
            };
            let (Some(snapshot_type), Some(element_type)) = (
                action.result_type.as_ref(),
                action.result_type.as_ref().and_then(|snapshot_type| {
                    match types.get(snapshot_type) {
                        Some(ResolvedType::Nominal {
                            item, arguments, ..
                        }) if item.as_str() == "builtin:type:List" && arguments.len() == 1 => {
                            Some(&arguments[0])
                        }
                        _ => None,
                    }
                }),
            ) else {
                all_attempts_match = false;
                continue;
            };
            let element_type = element_type.clone();
            let snapshot_local_type = body.locals.get(&snapshot_binding).map(|local| &local.ty);
            let unpack_local_type = body.locals.get(&unpack_binding).map(|local| &local.ty);
            let Some(settlement_point) = result_receipt.settlement_point.as_ref() else {
                all_attempts_match = false;
                continue;
            };
            let snapshot_is_dropped_after_unpack = statement_order
                .get(snapshot_binding_point)
                .zip(statement_order.get(&unpack_binding_point))
                .zip(statement_order.get(settlement_point))
                .is_some_and(|((snapshot, unpack), drop)| snapshot < unpack && unpack < drop);
            let payload_selection = ordinary_map_owned_any_v2_payload_selection_lineage(
                actions,
                &action.local,
                &attempt.conversion_to,
            );
            let Some((
                result_owner_settlement,
                result_owner_settlement_point,
                result_owner_settlement_index,
            )) = ordinary_map_owned_any_v2_values_unpack_owner_settlement(
                body,
                types,
                &unpack_binding,
                &unpack_binding_point,
                &unpack_binding_type,
            )
            else {
                all_attempts_match = false;
                continue;
            };
            let result_owner_settlement_point_is_unique = result_owner_settlement_point
                .as_ref()
                .is_none_or(|point| ordinary_map_owned_any_v2_cfg_point_count(cfg, point) == 1);
            let exact_values_list = matches!(
                types.get(&element_type),
                Some(ResolvedType::DynamicAny { capability })
                    if capability == "type.dynamic_value"
            );
            if action.point != result_receipt.result
                || action.kind != OrdinaryMapOwnedAnyV2ActionKind::Values
                || action.profile_version != OrdinaryMapOwnedAnyV2ProfileVersion::V2
                || action.source_local.as_ref() != Some(&action.local)
                || action.source.as_ref() != Some(&action.root)
                || action.result_local.as_ref() != Some(&snapshot_binding)
                || action.result_type.as_ref() != Some(snapshot_type)
                || recorded_snapshot != &snapshot_binding
                || result_receipt.profile_version != OrdinaryMapOwnedAnyV2ProfileVersion::V2
                || result_receipt.kind != OrdinaryMapOwnedAnyV2ActionKind::Values
                || result_receipt.binding != snapshot_binding
                || result_receipt.binding_point != *snapshot_binding_point
                || result_receipt.result_type != *snapshot_type
                || result_receipt.source_local != action.local
                || result_receipt.source != action.root
                || Some(&result_receipt.source) != action.source.as_ref()
                || Some(&result_receipt.source_node) != action.source_node.as_ref()
                || Some(&result_receipt.source_type) != action.source_type.as_ref()
                || snapshot_local_type != Some(snapshot_type)
                || value.ty != *snapshot_type
                || !exact_values_list
                || !index_local_type_matches
                || unsupported_projected_index
                || attempt.source_type != element_type
                || attempt.conversion_kind
                    != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                || attempt.conversion_from != element_type
                || attempt.conversion_to != unpack_binding_type
                || attempt.cast_type != attempt.conversion_to
                || unpack_local_type != Some(&unpack_binding_type)
                || !ordinary_map_owned_any_v2_unpack_target_supported(types, &attempt.conversion_to)
                || payload_selection.is_none()
                || !matches!(
                    types.get(&attempt.conversion_from),
                    Some(ResolvedType::DynamicAny { capability })
                        if capability == "type.dynamic_value"
                )
                || result_receipt.settlement
                    != OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                || !snapshot_is_dropped_after_unpack
                || !result_owner_settlement_point_is_unique
                || [
                    &action.point,
                    snapshot_binding_point,
                    &unpack_binding_point,
                    &attempt.projection,
                    &index.node_id,
                    &attempt.cast,
                    settlement_point,
                ]
                .into_iter()
                .any(|point| ordinary_map_owned_any_v2_cfg_point_count(cfg, point) != 1)
                || !receipted_values_casts.insert(attempt.cast.clone())
            {
                all_attempts_match = false;
                continue;
            }
            values_receipts.push(
                crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesUnpackReceipt {
                    profile_version: OrdinaryMapOwnedAnyV2ProfileVersion::V2,
                    operation: action.point.clone(),
                    source_local: action.local.clone(),
                    source: action
                        .source
                        .clone()
                        .expect("Values action source root was checked above"),
                    source_node: action
                        .source_node
                        .clone()
                        .expect("Values action source expression was checked above"),
                    source_type: action
                        .source_type
                        .clone()
                        .expect("Values action source type was checked above"),
                    snapshot_binding_point: snapshot_binding_point.clone(),
                    snapshot_binding,
                    snapshot_type: snapshot_type.clone(),
                    unpack_binding_point,
                    unpack_binding,
                    index_projection: attempt.projection.clone(),
                    index_expression: index.node_id.clone(),
                    index_type: index.ty.clone(),
                    index_local,
                    cast: attempt.cast.clone(),
                    conversion_kind: attempt.conversion_kind,
                    conversion_from: attempt.conversion_from.clone(),
                    conversion_to: attempt.conversion_to.clone(),
                    payload_selection: payload_selection
                        .expect("selected payload lineage was checked above"),
                    result_owner_settlement,
                    result_owner_settlement_point,
                    result_owner_settlement_index,
                    settlement: result_receipt.settlement,
                    settlement_point: settlement_point.clone(),
                },
            );
            continue;
        }

        let Some(place) = attempt.source_place else {
            all_attempts_match = false;
            continue;
        };
        let [ResolvedProjection::Tuple {
            index: 1,
            ty: projection_type,
        }] = place.projections.as_slice()
        else {
            all_attempts_match = false;
            continue;
        };
        let mut matching_actions = actions.iter().filter(|action| {
            action.kind == OrdinaryMapOwnedAnyV2ActionKind::GetAny
                && action.result_local.as_ref() == Some(&place.base)
        });
        let Some(action) = matching_actions.next() else {
            all_attempts_match = false;
            continue;
        };
        if matching_actions.next().is_some() {
            all_attempts_match = false;
            continue;
        }
        let Some((pair_binding_point, pair_binding)) = pair_bindings.get(&action.point) else {
            all_attempts_match = false;
            continue;
        };
        let result_receipts_for_operation = result_receipts
            .iter()
            .filter(|receipt| receipt.operation == action.point);
        let result_receipts_for_operation = result_receipts_for_operation.collect::<Vec<_>>();
        let [result_receipt] = result_receipts_for_operation.as_slice() else {
            all_attempts_match = false;
            continue;
        };
        let Some(result_type) = action.result_type.as_ref() else {
            all_attempts_match = false;
            continue;
        };
        let target_is_canonical_i32 = attempt.conversion_to
            == crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let target_selected_on_source_root = ordinary_map_owned_any_v2_root_selects_payload_type(
            actions,
            &action.local,
            &attempt.conversion_to,
        );
        let result_tuple_types = match types.get(result_type) {
            Some(ResolvedType::Tuple(elements)) if elements.len() == 2 => elements,
            _ => {
                all_attempts_match = false;
                continue;
            }
        };
        let exact_get_any_tuple = matches!(
            types.get(&result_tuple_types[0]),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::Bool))
        ) && matches!(
            types.get(&result_tuple_types[1]),
            Some(ResolvedType::DynamicAny { .. })
        );
        let pair_local_type = body.locals.get(pair_binding).map(|local| &local.ty);
        let unpack_local_type = body.locals.get(&unpack_binding).map(|local| &local.ty);
        let pair_is_dropped_after_unpack = result_receipt
            .settlement_point
            .as_ref()
            .and_then(|settlement| {
                Some((
                    *statement_order.get(pair_binding_point)?,
                    *statement_order.get(&unpack_binding_point)?,
                    *statement_order.get(settlement)?,
                ))
            })
            .is_some_and(|(pair, unpack, drop)| pair < unpack && unpack < drop);
        if action.point != result_receipt.result
            || result_receipt.profile_version != OrdinaryMapOwnedAnyV2ProfileVersion::V2
            || result_receipt.result_type != *result_type
            || action.result_local.as_ref() != Some(pair_binding)
            || pair_binding != &place.base
            || pair_local_type != Some(result_type)
            || !exact_get_any_tuple
            || projection_type != &result_tuple_types[1]
            || attempt.source_type != *projection_type
            || attempt.conversion_kind != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
            || attempt.conversion_from != *projection_type
            || attempt.conversion_to != unpack_binding_type
            || attempt.cast_type != attempt.conversion_to
            || unpack_local_type != Some(&unpack_binding_type)
            || !ordinary_map_owned_any_v2_unpack_target_supported(types, &attempt.conversion_to)
            || (!target_selected_on_source_root && !target_is_canonical_i32)
            || !matches!(
                types.get(&attempt.conversion_from),
                Some(ResolvedType::DynamicAny { .. })
            )
            || result_receipt.kind != OrdinaryMapOwnedAnyV2ActionKind::GetAny
            || result_receipt.binding != *pair_binding
            || result_receipt.settlement != OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
            || !pair_is_dropped_after_unpack
            || [
                &action.point,
                pair_binding_point,
                &unpack_binding_point,
                &attempt.projection,
                &attempt.cast,
                result_receipt
                    .settlement_point
                    .as_ref()
                    .expect("checked explicit settlement"),
            ]
            .into_iter()
            .any(|point| ordinary_map_owned_any_v2_cfg_point_count(cfg, point) != 1)
            || !receipted_operations.insert(action.point.clone())
        {
            all_attempts_match = false;
            continue;
        }
        receipts.push(crate::core::ownership::OrdinaryMapOwnedAnyV2UnpackReceipt {
            profile_version: OrdinaryMapOwnedAnyV2ProfileVersion::V2,
            operation: action.point.clone(),
            pair_binding_point: pair_binding_point.clone(),
            pair_binding: pair_binding.clone(),
            unpack_binding_point,
            unpack_binding,
            projection: attempt.projection,
            cast: attempt.cast,
            conversion_kind: attempt.conversion_kind,
            conversion_from: attempt.conversion_from,
            conversion_to: attempt.conversion_to,
            settlement: result_receipt.settlement,
            settlement_point: result_receipt
                .settlement_point
                .clone()
                .expect("explicit result settlement has a source point"),
        });
    }
    receipts
        .sort_by(|left, right| (&left.operation, &left.cast).cmp(&(&right.operation, &right.cast)));
    values_receipts
        .sort_by(|left, right| (&left.operation, &left.cast).cmp(&(&right.operation, &right.cast)));
    (receipts, values_receipts, all_attempts_match)
}

/// Prove the independent owner edge for each V2 GetAny/Values result. The
/// current V2 slice admits exactly one explicit `drop(binding)`; ordinary
/// observations may precede it, while transfers, escapes, and synthetic
/// scope cleanup are retained as distinct non-receipted outcomes.
fn collect_ordinary_map_owned_any_v2_result_receipts(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    actions: &[OrdinaryMapOwnedAnyV2Action],
) -> Vec<OrdinaryMapOwnedAnyV2ResultReceipt> {
    let mut states = actions
        .iter()
        .filter(|action| {
            matches!(
                action.kind,
                OrdinaryMapOwnedAnyV2ActionKind::GetAny | OrdinaryMapOwnedAnyV2ActionKind::Values
            )
        })
        .filter_map(|action| {
            action.result_local.as_ref()?;
            Some((
                action.point.clone(),
                OrdinaryMapOwnedAnyV2ResultUseState {
                    action: action.clone(),
                    observed_point: None,
                    binding_point: None,
                    settlement: None,
                },
            ))
        })
        .collect::<BTreeMap<_, _>>();
    if states.is_empty() {
        return Vec::new();
    }

    fn is_v2_result_call(expression: &ResolvedExpr) -> bool {
        matches!(
            &expression.kind,
            ResolvedExprKind::Call(ResolvedCall {
                callee: ResolvedCallee::Builtin(name),
                ..
            }) if matches!(name.as_str(), "map_get" | "values")
        )
    }

    fn call_arg_context(call: &ResolvedCall) -> OrdinaryMapOwnedGetResultUseContext {
        let ResolvedCallee::Builtin(name) = &call.callee else {
            return OrdinaryMapOwnedGetResultUseContext::Transfer;
        };
        if matches!(
            name.as_str(),
            "len"
                | "builtin.method.list.len"
                | "println"
                | "print_line"
                | "print_raw"
                | "print_format"
        ) {
            OrdinaryMapOwnedGetResultUseContext::Observe
        } else {
            OrdinaryMapOwnedGetResultUseContext::Transfer
        }
    }

    fn settle_state(
        state: &mut OrdinaryMapOwnedAnyV2ResultUseState,
        kind: OrdinaryMapOwnedAnyV2ResultSettlementKind,
        point: &NodeId,
    ) {
        state.settle(kind, point);
    }

    fn mark_expression(
        expression: &ResolvedExpr,
        context: OrdinaryMapOwnedGetResultUseContext,
        states: &mut BTreeMap<NodeId, OrdinaryMapOwnedAnyV2ResultUseState>,
    ) {
        let state = if let Some(state) = states.get_mut(&expression.node_id) {
            Some(state)
        } else if let ResolvedExprKind::Load(place) = &expression.kind {
            let owner_bearing_payload_projection = matches!(
                place.projections.as_slice(),
                [ResolvedProjection::Tuple { index: 1, .. }]
            );
            if place.projections.is_empty() || owner_bearing_payload_projection {
                states
                    .values_mut()
                    .find(|state| state.action.result_local.as_ref() == Some(&place.base))
            } else {
                None
            }
        } else {
            None
        };
        let Some(state) = state else {
            return;
        };
        match context {
            OrdinaryMapOwnedGetResultUseContext::Observe => state.observe(&expression.node_id),
            OrdinaryMapOwnedGetResultUseContext::Transfer => settle_state(
                state,
                OrdinaryMapOwnedAnyV2ResultSettlementKind::Transfer,
                &expression.node_id,
            ),
            OrdinaryMapOwnedGetResultUseContext::Escape => settle_state(
                state,
                OrdinaryMapOwnedAnyV2ResultSettlementKind::Escape,
                &expression.node_id,
            ),
            OrdinaryMapOwnedGetResultUseContext::Return => settle_state(
                state,
                OrdinaryMapOwnedAnyV2ResultSettlementKind::Transfer,
                &expression.node_id,
            ),
        }
    }

    fn visit_expr(
        expression: &ResolvedExpr,
        context: OrdinaryMapOwnedGetResultUseContext,
        states: &mut BTreeMap<NodeId, OrdinaryMapOwnedAnyV2ResultUseState>,
    ) {
        if is_v2_result_call(expression) {
            let direct_binding = states
                .get(&expression.node_id)
                .is_some_and(|state| state.action.result_local.is_some());
            if !direct_binding {
                mark_expression(expression, context, states);
            }
        } else {
            mark_expression(expression, context, states);
        }

        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                if let ResolvedValueProjection::Index(index) = projection {
                    visit_expr(index, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                visit_expr(left, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                visit_expr(right, OrdinaryMapOwnedGetResultUseContext::Observe, states);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Await(operand) => visit_expr(
                operand,
                OrdinaryMapOwnedGetResultUseContext::Observe,
                states,
            ),
            ResolvedExprKind::Spawn(operand) => {
                visit_expr(operand, OrdinaryMapOwnedGetResultUseContext::Escape, states)
            }
            ResolvedExprKind::Call(call) => {
                let argument_context = call_arg_context(call);
                for argument in &call.arguments {
                    visit_expr(&argument.value, argument_context, states);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                }
            }
            ResolvedExprKind::Map(values) => {
                for (key, value) in values {
                    visit_expr(key, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                    visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                visit_expr(
                    iterable,
                    OrdinaryMapOwnedGetResultUseContext::Observe,
                    states,
                );
                if let Some(guard) = guard {
                    visit_expr(guard, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => visit_expr(
                receiver,
                OrdinaryMapOwnedGetResultUseContext::Observe,
                states,
            ),
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    visit_expr(
                        &field.value,
                        OrdinaryMapOwnedGetResultUseContext::Escape,
                        states,
                    );
                }
                if let Some(rest) = rest {
                    visit_expr(rest, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => visit_block(block, context, states),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_expr(
                    condition,
                    OrdinaryMapOwnedGetResultUseContext::Observe,
                    states,
                );
                visit_block(then_block, context, states);
                visit_block(else_block, context, states);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                visit_expr(
                    scrutinee,
                    OrdinaryMapOwnedGetResultUseContext::Observe,
                    states,
                );
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit_expr(guard, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                    }
                    visit_expr(&arm.body, context, states);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                visit_expr(start, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                visit_expr(end, OrdinaryMapOwnedGetResultUseContext::Observe, states);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                visit_expr(target, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                if let Some(start) = start {
                    visit_expr(start, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
                if let Some(end) = end {
                    visit_expr(end, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
            }
            ResolvedExprKind::Lambda(lambda) => {
                for capture in &lambda.captures {
                    if let Some(state) = states
                        .values_mut()
                        .find(|state| state.action.result_local.as_ref() == Some(capture))
                    {
                        settle_state(
                            state,
                            OrdinaryMapOwnedAnyV2ResultSettlementKind::Escape,
                            &expression.node_id,
                        );
                    }
                }
            }
            ResolvedExprKind::Load(_)
            | ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }

    fn visit_block(
        block: &ResolvedBlock,
        result_context: OrdinaryMapOwnedGetResultUseContext,
        states: &mut BTreeMap<NodeId, OrdinaryMapOwnedAnyV2ResultUseState>,
    ) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    let direct_result_binding = initializer.as_ref().is_some_and(|value| {
                        is_v2_result_call(value)
                            && matches!(pattern.kind, ResolvedPatternKind::Binding { .. })
                    });
                    if let Some(value) = initializer {
                        if direct_result_binding {
                            if let Some(state) = states.get_mut(&value.node_id) {
                                if state
                                    .binding_point
                                    .replace(statement.node_id.clone())
                                    .is_some()
                                {
                                    state.settle(
                                        OrdinaryMapOwnedAnyV2ResultSettlementKind::DoubleSettlement,
                                        &statement.node_id,
                                    );
                                }
                            }
                        } else {
                            visit_expr(
                                value,
                                OrdinaryMapOwnedGetResultUseContext::Transfer,
                                states,
                            );
                        }
                    }
                }
                ResolvedStmtKind::Assign { value, .. } => {
                    visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Transfer, states)
                }
                ResolvedStmtKind::Return { value, .. } => {
                    if let Some(value) = value {
                        visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Return, states);
                    }
                }
                ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Transfer, states);
                    }
                }
                ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Observe, states),
                ResolvedStmtKind::Drop(places) => {
                    for place in places {
                        for state in states
                            .values_mut()
                            .filter(|state| state.action.result_local.as_ref() == Some(&place.base))
                        {
                            if place.projections.is_empty() {
                                state.settle(
                                    OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop,
                                    &statement.node_id,
                                );
                            } else {
                                state.settle(
                                    OrdinaryMapOwnedAnyV2ResultSettlementKind::Unsettled,
                                    &statement.node_id,
                                );
                            }
                        }
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    visit_expr(
                        condition,
                        OrdinaryMapOwnedGetResultUseContext::Observe,
                        states,
                    );
                    visit_block(body, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                }
                | ResolvedStmtKind::For {
                    iterable: initializer,
                    body,
                    ..
                } => {
                    visit_expr(
                        initializer,
                        OrdinaryMapOwnedGetResultUseContext::Observe,
                        states,
                    );
                    visit_block(body, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    visit_expr(
                        initializer,
                        OrdinaryMapOwnedGetResultUseContext::Observe,
                        states,
                    );
                    visit_block(
                        then_block,
                        OrdinaryMapOwnedGetResultUseContext::Observe,
                        states,
                    );
                    if let Some(else_block) = else_block {
                        visit_block(
                            else_block,
                            OrdinaryMapOwnedGetResultUseContext::Observe,
                            states,
                        );
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    visit_block(body, OrdinaryMapOwnedGetResultUseContext::Observe, states)
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                    visit_block(body, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        visit_expr(
                            expression,
                            OrdinaryMapOwnedGetResultUseContext::Observe,
                            states,
                        );
                    }
                }
                ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            visit_expr(result, result_context, states);
        }
    }

    visit_block(
        &body.root,
        OrdinaryMapOwnedGetResultUseContext::Return,
        &mut states,
    );
    // These expression stores are not reachable by walking `body.root`:
    // dynamic lvalue indices live in `place_inputs`, and parameter defaults
    // live in `default_values`. Both can contain an owner-bearing Any use, so
    // include them in the same exhaustive result-use census before accepting
    // a later explicit Drop as the unique settlement.
    for input in body.place_inputs.values() {
        visit_expr(
            input,
            OrdinaryMapOwnedGetResultUseContext::Observe,
            &mut states,
        );
    }
    for value in body.default_values.values() {
        visit_expr(
            value,
            OrdinaryMapOwnedGetResultUseContext::Escape,
            &mut states,
        );
    }

    states
        .into_iter()
        .map(|(operation, state)| {
            let binding_point = state.binding_point;
            let action = state.action;
            let (mut settlement, settlement_point) = state.settlement.unwrap_or_else(|| {
                state
                    .observed_point
                    .map(|point| (OrdinaryMapOwnedAnyV2ResultSettlementKind::Unsettled, point))
                    .unwrap_or_else(|| {
                        (
                            OrdinaryMapOwnedAnyV2ResultSettlementKind::ScopeDrop,
                            operation.clone(),
                        )
                    })
            });
            let Some(binding_point) = binding_point else {
                return None;
            };
            if settlement == OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop {
                let source_order = [
                    ordinary_map_owned_any_v2_cfg_point_index(cfg, &operation),
                    ordinary_map_owned_any_v2_cfg_point_index(cfg, &binding_point),
                    ordinary_map_owned_any_v2_cfg_point_index(cfg, &settlement_point),
                ];
                let points_are_unique = [&operation, &binding_point, &settlement_point]
                    .into_iter()
                    .all(|point| ordinary_map_owned_any_v2_cfg_point_count(cfg, point) == 1);
                if !points_are_unique
                    || !matches!(
                        source_order,
                        [Some(operation_index), Some(binding_index), Some(drop_index)]
                            if operation_index < binding_index && binding_index < drop_index
                    )
                {
                    settlement = OrdinaryMapOwnedAnyV2ResultSettlementKind::Unsettled;
                }
            }
            let (
                Some(source_local),
                Some(source),
                Some(source_node),
                Some(source_type),
                Some(result_type),
                Some(binding),
            ) = (
                action.source_local,
                action.source,
                action.source_node,
                action.source_type,
                action.result_type,
                action.result_local,
            )
            else {
                return None;
            };
            Some(OrdinaryMapOwnedAnyV2ResultReceipt {
                profile_version: OrdinaryMapOwnedAnyV2ProfileVersion::V2,
                operation: operation.clone(),
                kind: action.kind,
                source_local,
                source,
                source_node,
                source_type,
                key: action.key,
                key_type: action.key_type,
                key_local: action.key_local,
                result: operation,
                result_type,
                binding,
                binding_point,
                settlement,
                settlement_point: Some(settlement_point),
            })
        })
        .flatten()
        .collect()
}

/// Independent Checker recognizer for the owned-result typed getter profile.
/// It deliberately re-scans the ResolvedBody and keeps its own root identity,
/// action ledger, and operation attempts; no older Map profile can authorize
/// or complete this receipt.
#[derive(Clone)]
struct OrdinaryMapOwnedGetI32StringV1Checker<'a> {
    body: &'a ResolvedBody,
    types: &'a ResolvedTypeTable,
    actions: Vec<OrdinaryMapOwnedGetI32StringV1Action>,
    new_attempts: BTreeSet<NodeId>,
    operation_attempts: BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
    all_root_loads: Vec<(NodeId, ResolvedLocalId)>,
    roots: BTreeMap<ResolvedLocalId, bool>,
    allowed_root_loads: BTreeSet<NodeId>,
    invalid: bool,
}

/// Result of the standalone, checker-derived owned-Map collector. This is
/// intentionally callable on one independently materialized callable body
/// (including a closure body) so its Map actions and receipt keep that
/// callable's owner identity instead of being folded into an enclosing body.
#[derive(Debug, Clone)]
pub(crate) struct OrdinaryMapOwnedGetI32StringCfgCollection {
    pub(crate) owner: NodeId,
    pub(crate) actions: Vec<OrdinaryMapOwnedGetI32StringV1Action>,
    pub(crate) new_attempts: BTreeSet<NodeId>,
    pub(crate) operation_attempts: BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
    pub(crate) cfg_receipt: crate::core::OrdinaryMapOwnedGetI32StringV1CfgReceipt,
}

/// Collect exact ordinary Map owned-get evidence for one checker-derived
/// `(owner, ResolvedBody, CallableCfg, ResolvedTypeTable)` tuple. A mismatched
/// owner or any unsupported source operation/CFG shape returns `None` rather
/// than authorizing the supported-looking subset.
pub(crate) fn collect_ordinary_map_owned_get_i32_string_cfg(
    owner: &NodeId,
    body: &ResolvedBody,
    cfg: &CallableCfg,
    types: &ResolvedTypeTable,
) -> Option<OrdinaryMapOwnedGetI32StringCfgCollection> {
    if &cfg.owner != owner || &body.owner != owner {
        return None;
    }
    let (actions, new_attempts, operation_attempts, cfg_receipt) =
        OrdinaryMapOwnedGetI32StringV1Checker::collect(cfg, body, types);
    let cfg_receipt = cfg_receipt?;
    if &cfg_receipt.owner != owner {
        return None;
    }
    Some(OrdinaryMapOwnedGetI32StringCfgCollection {
        owner: owner.to_owned(),
        actions,
        new_attempts,
        operation_attempts,
        cfg_receipt,
    })
}

impl<'a> OrdinaryMapOwnedGetI32StringV1Checker<'a> {
    fn collect(
        cfg: &'a CallableCfg,
        body: &'a ResolvedBody,
        types: &'a ResolvedTypeTable,
    ) -> (
        Vec<OrdinaryMapOwnedGetI32StringV1Action>,
        BTreeSet<NodeId>,
        BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
        Option<crate::core::OrdinaryMapOwnedGetI32StringV1CfgReceipt>,
    ) {
        let mut checker = Self {
            body,
            types,
            actions: Vec::new(),
            new_attempts: BTreeSet::new(),
            operation_attempts: BTreeSet::new(),
            all_root_loads: Vec::new(),
            roots: BTreeMap::new(),
            allowed_root_loads: BTreeSet::new(),
            invalid: body
                .parameters
                .iter()
                .chain(body.captures.iter())
                .any(|local| {
                    body.locals
                        .get(local)
                        .is_some_and(|binding| checker_is_record(types, &binding.ty))
                })
                || checker_is_record(types, &body.root.ty),
        };
        checker.collect_block_evidence(&body.root);
        for expression in body.place_inputs.values() {
            checker.collect_expr_evidence(expression);
        }
        for expression in body.default_values.values() {
            checker.collect_expr_evidence(expression);
        }

        // The acyclic profile binds every source Map attempt to a reachable
        // block and carries owner-root facts along every edge. Cycles and
        // unsupported control-flow shapes remain fail-closed.
        if !Self::supports_acyclic_cfg(cfg, &checker.operation_attempts) {
            return (
                Vec::new(),
                checker.new_attempts,
                checker.operation_attempts,
                None,
            );
        }

        checker.visit_block(&body.root);
        checker.schedule_scope_drops(cfg);
        for (node, local) in &checker.all_root_loads {
            if checker.roots.contains_key(local) && !checker.allowed_root_loads.contains(node) {
                // Aliases, captures, aggregate escapes, and unrecognized root
                // observations withhold the complete receipt.
                checker.invalid = true;
            }
        }
        if checker.roots.values().any(|dropped| !dropped) {
            checker.invalid = true;
        }

        let mut point_order = BTreeMap::new();
        if let Some(block_order) = Self::acyclic_cfg_order(cfg) {
            for (block_rank, block_id) in block_order.into_iter().enumerate() {
                let Some(block) = cfg.blocks.get(&block_id) else {
                    checker.invalid = true;
                    continue;
                };
                for (point_rank, point) in block.points.iter().enumerate() {
                    if point_order
                        .insert(point.source.node.clone(), (block_rank, point_rank))
                        .is_some()
                    {
                        checker.invalid = true;
                    }
                }
            }
            if checker
                .actions
                .iter()
                .any(|action| !point_order.contains_key(&action.point))
            {
                checker.invalid = true;
            }
            checker.actions.sort_by_key(|action| {
                point_order
                    .get(&action.point)
                    .copied()
                    .unwrap_or((usize::MAX, usize::MAX))
            });
        } else {
            checker.invalid = true;
        }

        // Coverage is independent of the older lifecycle attempt ledger.
        // Untyped tuple map_get, Map literals, and future Map operations stay
        // visible here and cannot be hidden by supported actions elsewhere.
        let action_points = checker
            .actions
            .iter()
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        if checker
            .operation_attempts
            .iter()
            .any(|attempt| !action_points.contains(&attempt.point))
        {
            checker.invalid = true;
        }
        if cfg.reachable.len() > 1
            && checker
                .operation_attempts
                .iter()
                .any(|attempt| attempt.kind == OrdinaryMapOwnedGetI32StringV1AttemptKind::ScopeDrop)
        {
            // Scope drops are not path-local yet. Requiring explicit Drops in
            // the owner entry block prevents one synthetic cleanup point from
            // being misinterpreted as cleanup on every tail exit.
            checker.invalid = true;
        }
        let receipt_new_points = checker
            .actions
            .iter()
            .filter(|action| action.kind == OrdinaryMapOwnedGetI32StringV1ActionKind::New)
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        let has_typed_getter = checker.actions.iter().any(|action| {
            matches!(
                action.kind,
                OrdinaryMapOwnedGetI32StringV1ActionKind::GetI32
                    | OrdinaryMapOwnedGetI32StringV1ActionKind::GetString
                    | OrdinaryMapOwnedGetI32StringV1ActionKind::GetAny
                    | OrdinaryMapOwnedGetI32StringV1ActionKind::Values
            )
        });
        let entry_closed_profile =
            Self::supports_entry_closed_tail_cfg(cfg, &checker.operation_attempts)
                && checker.actions.iter().all(|action| {
                    cfg.blocks.get(&cfg.entry).is_some_and(|block| {
                        block
                            .points
                            .iter()
                            .any(|point| point.source.node == action.point)
                    })
                });
        // A Map operation created inside a branch would need path-local
        // introduction and cleanup at the join. The acyclic receipt can
        // safely carry operations in a post-join block, so reject only action
        // points strictly inside a branch arm. Compute each split's arm
        // region up to its first merge; the merge block itself is a valid
        // common continuation and is intentionally retained.
        if cfg.reachable.len() > 1 {
            let mut predecessors = BTreeMap::<crate::core::cfg::BasicBlockId, Vec<_>>::new();
            let mut successors = BTreeMap::<crate::core::cfg::BasicBlockId, Vec<_>>::new();
            for block in &cfg.reachable {
                predecessors.entry(block.clone()).or_default();
                successors.entry(block.clone()).or_default();
            }
            for edge in cfg.edges.values().filter(|edge| {
                cfg.reachable.contains(&edge.from) && cfg.reachable.contains(&edge.to)
            }) {
                predecessors
                    .entry(edge.to.clone())
                    .or_default()
                    .push(edge.from.clone());
                successors
                    .entry(edge.from.clone())
                    .or_default()
                    .push(edge.to.clone());
            }
            let mut action_blocks = BTreeMap::<NodeId, crate::core::cfg::BasicBlockId>::new();
            for (block_id, block) in &cfg.blocks {
                for point in &block.points {
                    action_blocks.insert(point.source.node.clone(), block_id.clone());
                }
            }
            let mut branch_arm_blocks = BTreeSet::new();
            for split in &cfg.reachable {
                let Some(arms) = successors.get(split) else {
                    continue;
                };
                if arms.len() < 2 {
                    continue;
                }
                for first in arms {
                    let mut work = vec![first.clone()];
                    let mut seen = BTreeSet::new();
                    while let Some(block_id) = work.pop() {
                        if !seen.insert(block_id.clone()) {
                            continue;
                        }
                        // A block reached by multiple incoming edges is the
                        // first merge for this arm. It is a safe continuation.
                        if predecessors
                            .get(&block_id)
                            .is_some_and(|preds| preds.len() > 1)
                        {
                            continue;
                        }
                        branch_arm_blocks.insert(block_id.clone());
                        if let Some(next) = successors.get(&block_id) {
                            work.extend(next.iter().cloned());
                        }
                    }
                }
            }
            if checker.actions.iter().any(|action| {
                action_blocks
                    .get(&action.point)
                    .is_some_and(|block| branch_arm_blocks.contains(block))
            }) {
                checker.invalid = true;
            }
        }
        if receipt_new_points != checker.new_attempts
            || receipt_new_points.is_empty()
            || !has_typed_getter
            || (entry_closed_profile && !checker.sealed_lifecycle_is_closed())
        {
            checker.invalid = true;
        }
        let result_receipts = collect_ordinary_map_owned_get_result_receipts(
            body,
            &checker.actions,
            &checker.operation_attempts,
        );
        let cfg_receipt = if checker.invalid {
            None
        } else if entry_closed_profile {
            Self::build_entry_closed_tail_cfg_receipt(
                cfg,
                &checker.operation_attempts,
                &checker.actions,
                result_receipts.clone(),
            )
        } else {
            Self::build_acyclic_per_edge_cfg_receipt(
                cfg,
                &checker.operation_attempts,
                &checker.actions,
                result_receipts,
            )
        };
        if checker.invalid || cfg_receipt.is_none() {
            checker.actions.clear();
        }
        (
            checker.actions,
            checker.new_attempts,
            checker.operation_attempts,
            cfg_receipt.filter(|_| !checker.invalid),
        )
    }

    fn supports_entry_closed_tail_cfg(
        cfg: &CallableCfg,
        attempts: &BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
    ) -> bool {
        Self::supports_acyclic_cfg(cfg, attempts)
            && attempts.iter().all(|attempt| {
                cfg.blocks.get(&cfg.entry).is_some_and(|block| {
                    block
                        .points
                        .iter()
                        .any(|point| point.source.node == attempt.point)
                })
            })
    }

    fn acyclic_cfg_order(cfg: &CallableCfg) -> Option<Vec<crate::core::cfg::BasicBlockId>> {
        let reachable = &cfg.reachable;
        let mut indegree = reachable
            .iter()
            .cloned()
            .map(|block| (block, 0usize))
            .collect::<BTreeMap<_, _>>();
        for edge in cfg
            .edges
            .values()
            .filter(|edge| reachable.contains(&edge.from))
        {
            *indegree.get_mut(&edge.to)? += 1;
        }
        let mut ready = indegree
            .iter()
            .filter_map(|(block, degree)| (*degree == 0).then_some(block.clone()))
            .collect::<BTreeSet<_>>();
        let mut order = Vec::with_capacity(reachable.len());
        while let Some(block) = ready.pop_first() {
            order.push(block.clone());
            for edge in cfg.successors(&block) {
                if !reachable.contains(&edge.to) {
                    continue;
                }
                let degree = indegree.get_mut(&edge.to)?;
                *degree = degree.checked_sub(1)?;
                if *degree == 0 {
                    ready.insert(edge.to.clone());
                }
            }
        }
        (order.len() == reachable.len()).then_some(order)
    }

    fn supports_acyclic_cfg(
        cfg: &CallableCfg,
        attempts: &BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
    ) -> bool {
        if attempts.is_empty() || !cfg.reachable.contains(&cfg.entry) {
            return false;
        }
        let reachable = &cfg.reachable;
        if reachable.iter().any(|block_id| {
            let Some(block) = cfg.blocks.get(block_id) else {
                return true;
            };
            let Some((_, role)) = block_id.0 .0.rsplit_once("/cfg:block:") else {
                return true;
            };
            !matches!(
                role,
                "entry" | "if-then" | "if-else" | "if-join" | "match-arm" | "match-join"
            ) || match &block.terminator {
                Terminator::Goto { .. } | Terminator::Branch { .. } | Terminator::Return { .. } => {
                    false
                }
                Terminator::Break { .. }
                | Terminator::Continue { .. }
                | Terminator::Diverge
                | Terminator::Unreachable => true,
                Terminator::Match { .. } => false,
            }
        }) {
            return false;
        }
        if cfg.edges.values().any(|edge| {
            reachable.contains(&edge.from)
                && (!reachable.contains(&edge.to)
                    || !matches!(
                        edge.kind,
                        EdgeKind::Then
                            | EdgeKind::Else
                            | EdgeKind::MatchArm
                            | EdgeKind::Fallthrough
                    ))
        }) {
            return false;
        }
        // The MIR projection for this receipt version understands only the
        // branch-arm-to-join shape emitted for one source `if`. Reject
        // continuation edges between joins (including nested/serial `if`s)
        // here rather than issuing a receipt that the MIR validator cannot
        // project.
        if cfg
            .edges
            .values()
            .filter(|edge| reachable.contains(&edge.from))
            .any(|edge| {
                let to_role = edge
                    .to
                    .0
                     .0
                    .rsplit_once("/cfg:block:")
                    .map(|(_, role)| role);
                match edge.kind {
                    EdgeKind::Then => to_role != Some("if-then"),
                    EdgeKind::Else => to_role != Some("if-else"),
                    EdgeKind::MatchArm => to_role != Some("match-arm"),
                    EdgeKind::Fallthrough => {
                        Self::fallthrough_if_arm(cfg, edge).is_none()
                            && Self::fallthrough_match_arm(cfg, edge).is_none()
                    }
                    _ => true,
                }
            })
        {
            return false;
        }
        // Cycles, including any backedge that might re-enter the Map owner
        // entry block, are not admitted by this first CFG receipt.
        let mut indegree = reachable
            .iter()
            .cloned()
            .map(|block| (block, 0usize))
            .collect::<BTreeMap<_, _>>();
        for edge in cfg
            .edges
            .values()
            .filter(|edge| reachable.contains(&edge.from))
        {
            *indegree.entry(edge.to.clone()).or_default() += 1;
        }
        let mut ready = indegree
            .iter()
            .filter_map(|(block, degree)| (*degree == 0).then_some(block.clone()))
            .collect::<Vec<_>>();
        let mut visited = 0usize;
        while let Some(block) = ready.pop() {
            visited += 1;
            for edge in cfg.successors(&block) {
                if !reachable.contains(&edge.to) {
                    continue;
                }
                let Some(degree) = indegree.get_mut(&edge.to) else {
                    return false;
                };
                *degree -= 1;
                if *degree == 0 {
                    ready.push(edge.to.clone());
                }
            }
        }
        if visited != reachable.len() {
            return false;
        }
        attempts.iter().all(|attempt| {
            let containing = cfg
                .blocks
                .iter()
                .filter(|(_, block)| {
                    block
                        .points
                        .iter()
                        .any(|point| point.source.node == attempt.point)
                })
                .map(|(block, _)| block)
                .collect::<Vec<_>>();
            containing.len() == 1 && reachable.contains(containing[0])
        })
    }

    /// Resolve a source CFG fallthrough to the enclosing `if` arm that owns
    /// it. The edge's `from` block may itself be a nested if-join, while the
    /// edge source is the stable identity of the outer then/else body. The
    /// branch edge into that body supplies the actual enclosing if identity.
    /// Comparing `from` and `to` block-name prefixes is incorrect: the then
    /// body and the if expression have distinct source node identities.
    fn fallthrough_if_arm(
        cfg: &CallableCfg,
        edge: &crate::core::cfg::CfgEdge,
    ) -> Option<(NodeId, EdgeKind)> {
        if edge.kind != EdgeKind::Fallthrough {
            return None;
        }
        for (role, branch_kind) in [("if-then", EdgeKind::Then), ("if-else", EdgeKind::Else)] {
            let arm = crate::core::cfg::BasicBlockId(NodeId(format!(
                "{}/cfg:block:{role}",
                edge.source.node.0
            )));
            if !cfg.reachable.contains(&arm) {
                continue;
            }
            let mut incoming = cfg
                .predecessors(&arm)
                .into_iter()
                .filter(|candidate| candidate.kind == branch_kind);
            let branch = incoming.next()?;
            if incoming.next().is_some() {
                return None;
            }
            let join = crate::core::cfg::BasicBlockId(NodeId(format!(
                "{}/cfg:block:if-join",
                branch.source.node.0
            )));
            if edge.to != join {
                return None;
            }
            return Some((branch.source.node.clone(), branch_kind));
        }
        None
    }

    /// Resolve a source CFG fallthrough from a `match` arm to its shared join.
    /// The arm edge carries the arm pattern's stable node identity while the
    /// join block is named after the enclosing match expression. Keeping this
    /// lookup explicit prevents an arm-local Map operation from being
    /// mistaken for an entry or `if` block during receipt projection.
    fn fallthrough_match_arm(
        cfg: &CallableCfg,
        edge: &crate::core::cfg::CfgEdge,
    ) -> Option<(NodeId, EdgeKind)> {
        if edge.kind != EdgeKind::Fallthrough {
            return None;
        }
        let arm = crate::core::cfg::BasicBlockId(NodeId(format!(
            "{}/cfg:block:match-arm",
            edge.source.node.0
        )));
        if !cfg.reachable.contains(&arm) || edge.from != arm {
            return None;
        }
        let mut incoming = cfg
            .predecessors(&arm)
            .into_iter()
            .filter(|candidate| candidate.kind == EdgeKind::MatchArm);
        let _match_arm = incoming.next()?;
        if incoming.next().is_some() {
            return None;
        }
        let to_role = edge
            .to
            .0
             .0
            .rsplit_once("/cfg:block:")
            .map(|(_, role)| role);
        if to_role != Some("match-join") {
            return None;
        }
        Some((edge.source.node.clone(), EdgeKind::MatchArm))
    }

    fn build_entry_closed_tail_cfg_receipt(
        cfg: &CallableCfg,
        attempts: &BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
        actions: &[OrdinaryMapOwnedGetI32StringV1Action],
        result_receipts: Vec<OrdinaryMapOwnedGetResultReceipt>,
    ) -> Option<crate::core::OrdinaryMapOwnedGetI32StringV1CfgReceipt> {
        use crate::core::{
            OrdinaryMapOwnedGetI32StringV1CfgBlock,
            OrdinaryMapOwnedGetI32StringV1CfgProfileVersion as Profile,
            OrdinaryMapOwnedGetI32StringV1CfgReceipt,
        };

        if !Self::supports_entry_closed_tail_cfg(cfg, attempts) || actions.is_empty() {
            return None;
        }
        let reachable_blocks = cfg
            .reachable
            .iter()
            .map(|id| {
                let block = cfg.blocks.get(id)?;
                Some((
                    id.clone(),
                    OrdinaryMapOwnedGetI32StringV1CfgBlock {
                        points: block
                            .points
                            .iter()
                            .map(|point| (point.source.node.clone(), point.kind))
                            .collect(),
                        terminator: block.terminator.clone(),
                    },
                ))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        let reachable_edges = cfg
            .edges
            .iter()
            .filter(|(_, edge)| cfg.reachable.contains(&edge.from))
            .map(|(id, edge)| (id.clone(), edge.clone()))
            .collect::<BTreeMap<_, _>>();
        let action_points = actions
            .iter()
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        let attempt_points = attempts
            .iter()
            .map(|attempt| attempt.point.clone())
            .collect::<BTreeSet<_>>();
        if action_points != attempt_points {
            return None;
        }
        Some(OrdinaryMapOwnedGetI32StringV1CfgReceipt {
            owner: cfg.owner.clone(),
            profile_version: Profile::EntryClosedTailV1,
            entry: cfg.entry.clone(),
            map_operation_block: cfg.entry.clone(),
            reachable_blocks,
            reachable_edges,
            attempts: attempts.clone(),
            action_points,
            result_receipts,
            live_roots_in: BTreeMap::new(),
            live_roots_out: BTreeMap::new(),
            live_roots_on_edge: BTreeMap::new(),
        })
    }

    fn build_acyclic_per_edge_cfg_receipt(
        cfg: &CallableCfg,
        attempts: &BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
        actions: &[OrdinaryMapOwnedGetI32StringV1Action],
        result_receipts: Vec<OrdinaryMapOwnedGetResultReceipt>,
    ) -> Option<crate::core::OrdinaryMapOwnedGetI32StringV1CfgReceipt> {
        use crate::core::{
            OrdinaryMapOwnedGetI32StringV1ActionKind as Action,
            OrdinaryMapOwnedGetI32StringV1AttemptKind as Attempt,
            OrdinaryMapOwnedGetI32StringV1CfgBlock,
            OrdinaryMapOwnedGetI32StringV1CfgProfileVersion as Profile,
            OrdinaryMapOwnedGetI32StringV1CfgReceipt,
        };

        if !Self::supports_acyclic_cfg(cfg, attempts) || actions.is_empty() {
            return None;
        }
        let order = Self::acyclic_cfg_order(cfg)?;
        let reachable_blocks = cfg
            .reachable
            .iter()
            .map(|id| {
                let block = cfg.blocks.get(id)?;
                Some((
                    id.clone(),
                    OrdinaryMapOwnedGetI32StringV1CfgBlock {
                        points: block
                            .points
                            .iter()
                            .map(|point| (point.source.node.clone(), point.kind))
                            .collect(),
                        terminator: block.terminator.clone(),
                    },
                ))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        let reachable_edges = cfg
            .edges
            .iter()
            .filter(|(_, edge)| cfg.reachable.contains(&edge.from))
            .map(|(id, edge)| (id.clone(), edge.clone()))
            .collect::<BTreeMap<_, _>>();
        let action_points = actions
            .iter()
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        let attempt_points = attempts
            .iter()
            .map(|attempt| attempt.point.clone())
            .collect::<BTreeSet<_>>();
        if action_points != attempt_points {
            return None;
        }

        let mut point_location = BTreeMap::new();
        for block_id in &order {
            let block = cfg.blocks.get(block_id)?;
            for (point_index, point) in block.points.iter().enumerate() {
                if point_location
                    .insert(point.source.node.clone(), (block_id.clone(), point_index))
                    .is_some()
                {
                    return None;
                }
            }
        }
        if attempts
            .iter()
            .any(|attempt| !point_location.contains_key(&attempt.point))
            || actions
                .iter()
                .any(|action| !point_location.contains_key(&action.point))
        {
            return None;
        }

        let mut expected_events = BTreeMap::new();
        for attempt in attempts {
            let action_kind = match attempt.kind {
                Attempt::New => Action::New,
                Attempt::UntypedGet => Action::GetAny,
                Attempt::GetI32 => Action::GetI32,
                Attempt::GetString => Action::GetString,
                Attempt::Set => Action::Set,
                Attempt::Remove => Action::Remove,
                Attempt::Size => Action::Size,
                Attempt::HasKey => Action::HasKey,
                Attempt::Drop | Attempt::ScopeDrop => Action::Drop,
                Attempt::Clone => Action::Clone,
                Attempt::Values => Action::Values,
                Attempt::Keys | Attempt::FromList | Attempt::MapLiteral => return None,
            };
            *expected_events
                .entry((attempt.point.clone(), action_kind))
                .or_insert(0usize) += 1;
        }
        let mut actual_events = BTreeMap::new();
        for action in actions {
            *actual_events
                .entry((action.point.clone(), action.kind))
                .or_insert(0usize) += 1;
        }
        if expected_events != actual_events {
            return None;
        }

        let mut live_roots_in = BTreeMap::new();
        let mut live_roots_out = BTreeMap::new();
        let mut live_roots_on_edge_sources =
            BTreeMap::<crate::core::cfg::EdgeId, BTreeSet<ResolvedLocalId>>::new();
        let mut ever_created = BTreeSet::new();
        let canonical_roots = |roots: &BTreeSet<ResolvedLocalId>| {
            roots
                .iter()
                .map(|local| Self::resource_identity(local).0 .0)
                .collect::<BTreeSet<_>>()
        };
        for block_id in &order {
            let block = cfg.blocks.get(block_id)?;
            let incoming = reachable_edges
                .iter()
                .filter(|(_, edge)| &edge.to == block_id)
                .map(|(edge_id, _)| edge_id)
                .collect::<Vec<_>>();
            let mut incoming_facts = incoming
                .iter()
                .map(|edge_id| live_roots_on_edge_sources.get(*edge_id).cloned())
                .collect::<Option<Vec<_>>>()?;
            let live_in = if block_id == &cfg.entry {
                if !incoming_facts.is_empty() {
                    return None;
                }
                BTreeSet::new()
            } else {
                if incoming_facts.is_empty() {
                    return None;
                }
                let first = incoming_facts.pop()?;
                if incoming_facts.iter().any(|fact| fact != &first) {
                    return None;
                }
                first
            };
            live_roots_in.insert(block_id.clone(), canonical_roots(&live_in));
            let mut live = live_in;
            let mut block_actions = actions
                .iter()
                .filter_map(|action| {
                    let (owner_block, point_index) = point_location.get(&action.point)?;
                    (owner_block == block_id).then_some((*point_index, action))
                })
                .collect::<Vec<_>>();
            block_actions.sort_by_key(|(point_index, _)| *point_index);
            for (_, action) in block_actions {
                let creates = matches!(
                    action.kind,
                    Action::New | Action::Clone | Action::Set | Action::Remove
                );
                if creates {
                    if !ever_created.insert(action.local.clone()) || live.contains(&action.local) {
                        return None;
                    }
                    if matches!(action.kind, Action::Clone | Action::Set | Action::Remove) {
                        let source = action.source_local.as_ref()?;
                        if source == &action.local || !live.contains(source) {
                            return None;
                        }
                    } else if action.source_local.is_some() {
                        return None;
                    }
                    live.insert(action.local.clone());
                } else if action.kind == Action::Drop {
                    if !live.remove(&action.local) {
                        return None;
                    }
                } else if !live.contains(&action.local) {
                    return None;
                }
            }
            if matches!(&block.terminator, Terminator::Return { .. }) && !live.is_empty() {
                return None;
            }
            live_roots_out.insert(block_id.clone(), canonical_roots(&live));
            for edge in reachable_edges
                .values()
                .filter(|edge| &edge.from == block_id)
            {
                live_roots_on_edge_sources.insert(edge.id.clone(), live.clone());
            }
        }
        let live_roots_on_edge = live_roots_on_edge_sources
            .into_iter()
            .map(|(edge, roots)| (edge, canonical_roots(&roots)))
            .collect::<BTreeMap<_, _>>();
        if live_roots_in.len() != reachable_blocks.len()
            || live_roots_out.len() != reachable_blocks.len()
            || live_roots_on_edge.len() != reachable_edges.len()
        {
            return None;
        }

        Some(OrdinaryMapOwnedGetI32StringV1CfgReceipt {
            owner: cfg.owner.clone(),
            profile_version: Profile::AcyclicPerEdgeRootsV2,
            entry: cfg.entry.clone(),
            map_operation_block: cfg.entry.clone(),
            reachable_blocks,
            reachable_edges,
            attempts: attempts.clone(),
            action_points,
            result_receipts,
            live_roots_in,
            live_roots_out,
            live_roots_on_edge,
        })
    }

    /// Checker-owned scope cleanup is an operation at the callable's final
    /// CFG point. Each surviving local contributes one separate Drop attempt
    /// and receipt; explicit drops have already removed their obligations.
    fn schedule_scope_drops(&mut self, cfg: &CallableCfg) {
        let remaining = self
            .roots
            .iter()
            .rev()
            .filter(|(_, dropped)| !**dropped)
            .map(|(local, _)| local.clone())
            .collect::<Vec<_>>();
        if remaining.is_empty() {
            return;
        }
        let Some(point) = cfg
            .blocks
            .values()
            .next()
            .and_then(|block| block.points.last())
            .map(|point| point.source.node.clone())
        else {
            self.invalid = true;
            return;
        };
        for local in remaining {
            self.operation_attempts
                .insert(OrdinaryMapOwnedGetI32StringV1Attempt {
                    kind: OrdinaryMapOwnedGetI32StringV1AttemptKind::ScopeDrop,
                    point: point.clone(),
                    arguments: vec![local.0.clone()],
                    receiver_local: None,
                    result_local: None,
                });
            self.roots.insert(local.clone(), true);
            self.actions.push(self.action(
                OrdinaryMapOwnedGetI32StringV1ActionKind::Drop,
                point.clone(),
                local,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
    }

    /// An ordinary free-value binding creates an independent persistent root.
    /// It never copies the raw root handle or steals the source obligation.
    fn visit_clone(&mut self, expression: &ResolvedExpr, target: &ResolvedLocalId) {
        let Some(source) = self.live_root_load(expression) else {
            self.invalid = true;
            return;
        };
        if self.roots.contains_key(target)
            || !self.is_record(&expression.ty)
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|local| local.ty == expression.ty)
        {
            self.invalid = true;
            return;
        }
        self.allow_root_load(expression);
        self.operation_attempts
            .insert(OrdinaryMapOwnedGetI32StringV1Attempt {
                kind: OrdinaryMapOwnedGetI32StringV1AttemptKind::Clone,
                point: expression.node_id.clone(),
                arguments: vec![source.0.clone(), target.0.clone()],
                receiver_local: Some(source.clone()),
                result_local: Some(target.clone()),
            });
        self.roots.insert(target.clone(), false);
        self.actions.push(self.action(
            OrdinaryMapOwnedGetI32StringV1ActionKind::Clone,
            expression.node_id.clone(),
            target.clone(),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(source),
        ));
    }

    fn sealed_lifecycle_is_closed(&self) -> bool {
        use OrdinaryMapOwnedGetI32StringV1ActionKind as Kind;

        if self.actions.is_empty() {
            return false;
        }
        let mut ever_created = BTreeSet::<ResolvedLocalId>::new();
        let mut live = BTreeSet::<ResolvedLocalId>::new();
        for action in &self.actions {
            if action.profile_version != OrdinaryMapOwnedGetI32StringV1ProfileVersion::V1
                || action.root != Self::resource_identity(&action.local)
            {
                return false;
            }
            match action.kind {
                Kind::New => {
                    if Self::action_has_key(action)
                        || action.key_local.is_some()
                        || Self::action_has_value(action)
                        || action.result_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !ever_created.insert(action.local.clone())
                        || !live.insert(action.local.clone())
                    {
                        return false;
                    }
                }
                Kind::Clone => {
                    let (Some(source_local), Some(source)) = (&action.source_local, &action.source)
                    else {
                        return false;
                    };
                    if action.key.is_some()
                        || action.key_type.is_some()
                        || action.key_local.is_some()
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.result_type.is_some()
                        || source != &Self::resource_identity(source_local)
                        || !live.contains(source_local)
                        || source_local == &action.local
                        || !ever_created.insert(action.local.clone())
                        || !live.insert(action.local.clone())
                    {
                        return false;
                    }
                }
                Kind::Set => {
                    let (
                        Some(key),
                        Some(key_type),
                        key_local,
                        Some(_value),
                        Some(value_type),
                        Some(source_local),
                        Some(source),
                    ) = (
                        &action.key,
                        &action.key_type,
                        &action.key_local,
                        &action.value,
                        &action.value_type,
                        &action.source_local,
                        &action.source,
                    )
                    else {
                        return false;
                    };
                    if !self.is_string(key_type)
                        || !key_local.as_ref().is_none_or(|local| {
                            self.body
                                .locals
                                .get(local)
                                .is_some_and(|binding| self.is_string(&binding.ty))
                        })
                        || !(self.is_i32(value_type) || self.is_string(value_type))
                        || action.result_type.is_some()
                        || source != &Self::resource_identity(source_local)
                        || !live.contains(source_local)
                        || !ever_created.insert(action.local.clone())
                        || !live.insert(action.local.clone())
                    {
                        return false;
                    }
                    let _ = key;
                }
                Kind::Remove => {
                    let (Some(key), Some(key_type), Some(source_local), Some(source)) = (
                        &action.key,
                        &action.key_type,
                        &action.source_local,
                        &action.source,
                    ) else {
                        return false;
                    };
                    if !self.is_string(key_type)
                        || !action.key_local.as_ref().is_none_or(|local| {
                            self.body
                                .locals
                                .get(local)
                                .is_some_and(|binding| self.is_string(&binding.ty))
                        })
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.result_type.is_some()
                        || source != &Self::resource_identity(source_local)
                        || !live.contains(source_local)
                        || !ever_created.insert(action.local.clone())
                        || !live.insert(action.local.clone())
                    {
                        return false;
                    }
                    let _ = key;
                }
                Kind::Size => {
                    if Self::action_has_key(action)
                        || action.key_local.is_some()
                        || Self::action_has_value(action)
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || action
                            .result_type
                            .as_ref()
                            .is_none_or(|ty| !self.is_i32(ty))
                        || !live.contains(&action.local)
                    {
                        return false;
                    }
                }
                Kind::HasKey => {
                    if action.key.is_none()
                        || action
                            .key_type
                            .as_ref()
                            .is_none_or(|ty| !self.is_string(ty))
                        || !action.key_local.as_ref().is_none_or(|local| {
                            self.body
                                .locals
                                .get(local)
                                .is_some_and(|binding| self.is_string(&binding.ty))
                        })
                        || Self::action_has_value(action)
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || action
                            .result_type
                            .as_ref()
                            .is_none_or(|ty| !self.is_bool(ty))
                        || !live.contains(&action.local)
                    {
                        return false;
                    }
                }
                Kind::GetI32 | Kind::GetString | Kind::GetAny => {
                    let Some(result_type) = &action.result_type else {
                        return false;
                    };
                    let ok_type = if action.kind == Kind::GetI32 {
                        crate::core::PrimitiveType::I32
                    } else {
                        crate::core::PrimitiveType::String
                    };
                    if action.key.is_none()
                        || action
                            .key_type
                            .as_ref()
                            .is_none_or(|ty| !self.is_string(ty))
                        || !action.key_local.as_ref().is_none_or(|local| {
                            self.body
                                .locals
                                .get(local)
                                .is_some_and(|binding| self.is_string(&binding.ty))
                        })
                        || Self::action_has_value(action)
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !(if action.kind == Kind::GetAny {
                            self.is_get_any_tuple(result_type)
                        } else {
                            self.is_result(result_type, ok_type, crate::core::PrimitiveType::I32)
                        })
                        || !live.contains(&action.local)
                    {
                        return false;
                    }
                }
                Kind::Values => {
                    let Some(result_type) = &action.result_type else {
                        return false;
                    };
                    if Self::action_has_key(action)
                        || Self::action_has_value(action)
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !self.is_values_list(result_type)
                        || !live.contains(&action.local)
                    {
                        return false;
                    }
                }
                Kind::Drop => {
                    if Self::action_has_key(action)
                        || action.key_local.is_some()
                        || Self::action_has_value(action)
                        || action.result_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !live.remove(&action.local)
                    {
                        return false;
                    }
                }
            }
        }
        live.is_empty() && ever_created == self.roots.keys().cloned().collect()
    }

    fn map_attempt_kind(name: &str) -> Option<OrdinaryMapOwnedGetI32StringV1AttemptKind> {
        use OrdinaryMapOwnedGetI32StringV1AttemptKind as Kind;
        match name {
            "map_new" => Some(Kind::New),
            "map_get" => Some(Kind::UntypedGet),
            "map_get_i32" => Some(Kind::GetI32),
            "map_get_string" => Some(Kind::GetString),
            "map_set" => Some(Kind::Set),
            "map_remove" => Some(Kind::Remove),
            "map_size" => Some(Kind::Size),
            "has_key" => Some(Kind::HasKey),
            "keys" => Some(Kind::Keys),
            "values" => Some(Kind::Values),
            "to_list" => Some(Kind::Values),
            "map_from_list" => Some(Kind::FromList),
            _ => None,
        }
    }

    fn record_call_attempt(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        result_local: Option<ResolvedLocalId>,
    ) {
        let ResolvedCallee::Builtin(name) = &call.callee else {
            return;
        };
        let Some(kind) = Self::map_attempt_kind(name.as_str()) else {
            return;
        };
        let arguments = call
            .arguments
            .iter()
            .map(|argument| argument.value.node_id.clone())
            .collect::<Vec<_>>();
        self.operation_attempts
            .insert(OrdinaryMapOwnedGetI32StringV1Attempt {
                kind,
                point: expression.node_id.clone(),
                arguments,
                receiver_local: call.arguments.first().and_then(|argument| {
                    match &argument.value.kind {
                        ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                            Some(place.base.clone())
                        }
                        _ => None,
                    }
                }),
                result_local,
            });
        if kind == OrdinaryMapOwnedGetI32StringV1AttemptKind::New {
            self.new_attempts.insert(expression.node_id.clone());
        }
    }

    fn collect_block_evidence(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    if let Some(value) = initializer {
                        let result_local = match &pattern.kind {
                            ResolvedPatternKind::Binding { local, .. } => Some(local.clone()),
                            _ => None,
                        };
                        if let ResolvedExprKind::Call(call) = &value.kind {
                            self.record_call_attempt(value, call, result_local);
                            for argument in &call.arguments {
                                self.collect_expr_evidence(&argument.value);
                            }
                        } else {
                            self.collect_expr_evidence(value);
                        }
                    }
                }
                ResolvedStmtKind::Assign { value, .. }
                | ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => self.collect_expr_evidence(value),
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.collect_expr_evidence(value);
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    self.collect_expr_evidence(condition);
                    self.collect_block_evidence(body);
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                }
                | ResolvedStmtKind::For {
                    iterable: initializer,
                    body,
                    ..
                } => {
                    self.collect_expr_evidence(initializer);
                    self.collect_block_evidence(body);
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    self.collect_expr_evidence(initializer);
                    self.collect_block_evidence(then_block);
                    if let Some(block) = else_block {
                        self.collect_block_evidence(block);
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    self.collect_block_evidence(body);
                }
                ResolvedStmtKind::Drop(_) => {}
                ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {}
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.collect_expr_evidence(expression);
                    }
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    self.collect_expr_evidence(value);
                    self.collect_block_evidence(body);
                }
            }
        }
        if let Some(result) = &block.result {
            self.collect_expr_evidence(result);
        }
    }

    fn collect_expr_evidence(&mut self, expression: &ResolvedExpr) {
        match &expression.kind {
            ResolvedExprKind::Load(place) => self
                .all_root_loads
                .push((expression.node_id.clone(), place.base.clone())),
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        self.collect_expr_evidence(value);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.collect_expr_evidence(value);
                if let ResolvedValueProjection::Index(index) = projection {
                    self.collect_expr_evidence(index);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                self.collect_expr_evidence(left);
                self.collect_expr_evidence(right);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.collect_expr_evidence(operand),
            ResolvedExprKind::Call(call) => {
                self.record_call_attempt(expression, call, None);
                for argument in &call.arguments {
                    self.collect_expr_evidence(&argument.value);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.collect_expr_evidence(value);
                }
            }
            ResolvedExprKind::Map(values) => {
                self.operation_attempts
                    .insert(OrdinaryMapOwnedGetI32StringV1Attempt {
                        kind: OrdinaryMapOwnedGetI32StringV1AttemptKind::MapLiteral,
                        point: expression.node_id.clone(),
                        arguments: values
                            .iter()
                            .flat_map(|(key, value)| [key.node_id.clone(), value.node_id.clone()])
                            .collect(),
                        receiver_local: None,
                        result_local: None,
                    });
                for (key, value) in values {
                    self.collect_expr_evidence(key);
                    self.collect_expr_evidence(value);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.collect_expr_evidence(value);
                self.collect_expr_evidence(iterable);
                if let Some(guard) = guard {
                    self.collect_expr_evidence(guard);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => {
                self.collect_expr_evidence(receiver)
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.collect_expr_evidence(&field.value);
                }
                if let Some(rest) = rest {
                    self.collect_expr_evidence(rest);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.collect_block_evidence(block),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.collect_expr_evidence(condition);
                self.collect_block_evidence(then_block);
                self.collect_block_evidence(else_block);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.collect_expr_evidence(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.collect_expr_evidence(guard);
                    }
                    self.collect_expr_evidence(&arm.body);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                self.collect_expr_evidence(start);
                self.collect_expr_evidence(end);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.collect_expr_evidence(target);
                if let Some(start) = start {
                    self.collect_expr_evidence(start);
                }
                if let Some(end) = end {
                    self.collect_expr_evidence(end);
                }
            }
            ResolvedExprKind::Lambda(_) => {
                // Lambda bodies have their own canonical callable identity.
                // This callable's attempt inventory must not absorb them.
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }

    fn visit_block(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    if let Some(initializer) = initializer {
                        let target = match &pattern.kind {
                            ResolvedPatternKind::Binding { local, .. } => Some(local.clone()),
                            _ => None,
                        };
                        self.visit_expr(initializer, target.as_ref());
                    }
                }
                ResolvedStmtKind::Assign { target, value, .. } => {
                    if self.roots.contains_key(&target.base) {
                        self.invalid = true;
                    }
                    self.visit_expr(value, None);
                }
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.visit_expr(value, None);
                    }
                    if matches!(&statement.kind, ResolvedStmtKind::Break(_)) {
                        self.invalid = true;
                    }
                }
                ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => self.visit_expr(value, None),
                ResolvedStmtKind::Drop(places) => self.visit_drop(statement, places),
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.visit_expr(expression, None);
                    }
                }
                ResolvedStmtKind::While { .. }
                | ResolvedStmtKind::WhileLet { .. }
                | ResolvedStmtKind::IfLet { .. }
                | ResolvedStmtKind::Loop(_)
                | ResolvedStmtKind::For { .. }
                | ResolvedStmtKind::Scope { .. }
                | ResolvedStmtKind::Pinned { .. }
                | ResolvedStmtKind::NestedCallable(_)
                | ResolvedStmtKind::Continue => self.invalid = true,
            }
        }
        if let Some(result) = &block.result {
            self.visit_expr(result, None);
        }
    }

    fn visit_drop(&mut self, statement: &ResolvedStmt, places: &[ResolvedPlace]) {
        for place in places {
            let Some(was_dropped) = self.roots.get(&place.base).copied() else {
                continue;
            };
            self.operation_attempts
                .insert(OrdinaryMapOwnedGetI32StringV1Attempt {
                    kind: OrdinaryMapOwnedGetI32StringV1AttemptKind::Drop,
                    point: statement.node_id.clone(),
                    arguments: vec![place.base.0.clone()],
                    receiver_local: None,
                    result_local: None,
                });
            if !place.projections.is_empty() || was_dropped {
                self.invalid = true;
                continue;
            }
            self.roots.insert(place.base.clone(), true);
            self.actions.push(self.action(
                OrdinaryMapOwnedGetI32StringV1ActionKind::Drop,
                statement.node_id.clone(),
                place.base.clone(),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
    }

    fn visit_expr(&mut self, expression: &ResolvedExpr, direct_bind: Option<&ResolvedLocalId>) {
        match &expression.kind {
            ResolvedExprKind::Call(call) => {
                let builtin = match &call.callee {
                    ResolvedCallee::Builtin(name) => name.as_str(),
                    _ => "",
                };
                match builtin {
                    "map_new" => self.visit_new(expression, call, direct_bind),
                    "map_set" => self.visit_set(expression, call, direct_bind),
                    "map_remove" => self.visit_remove(expression, call, direct_bind),
                    "map_size" => self.visit_size(expression, call),
                    "has_key" => self.visit_has_key(expression, call),
                    "map_get_i32" => self.visit_get(expression, call, false),
                    "map_get_string" => self.visit_get(expression, call, true),
                    "map_get" => self.visit_get_any(expression, call),
                    "values" => self.visit_values(expression, call),
                    // `to_list(map)` is the ordinary MapExt snapshot shape,
                    // whose pair-list element owners are not part of this
                    // managed Any profile yet. Keep the attempt visible but
                    // reject the profile rather than silently treating it as
                    // the narrower `values(map)` result.
                    "to_list" => {
                        self.invalid = true;
                    }
                    "keys" | "map_from_list" => {
                        self.invalid = true;
                    }
                    _ => {}
                }
                for argument in &call.arguments {
                    self.visit_expr(&argument.value, None);
                }
            }
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        self.visit_expr(value, None);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.visit_expr(value, None);
                if let ResolvedValueProjection::Index(index) = projection {
                    self.visit_expr(index, None);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                self.visit_expr(left, None);
                self.visit_expr(right, None);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.visit_expr(operand, None),
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.visit_expr(value, None);
                }
            }
            ResolvedExprKind::Map(values) => {
                self.invalid = true;
                for (key, value) in values {
                    self.visit_expr(key, None);
                    self.visit_expr(value, None);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.invalid = true;
                self.visit_expr(value, None);
                self.visit_expr(iterable, None);
                if let Some(guard) = guard {
                    self.visit_expr(guard, None);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => self.visit_expr(receiver, None),
            ResolvedExprKind::Record { fields, rest, .. } => {
                self.invalid = true;
                for field in fields {
                    self.visit_expr(&field.value, None);
                }
                if let Some(rest) = rest {
                    self.visit_expr(rest, None);
                }
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                // Analyze both arms from the same ownership state. The CFG
                // receipt later checks the exact edge facts and replays each
                // arm's Map actions independently; a root may not escape one
                // arm unless the other arm carries the identical obligation
                // to the join.
                let action_prefix_len = self.actions.len();
                let mut branch_base = self.clone();
                branch_base.visit_expr(condition, None);
                let branch_action_prefix_len = branch_base.actions.len();

                let mut then_checker = branch_base.clone();
                then_checker.visit_block(then_block);
                let mut else_checker = branch_base.clone();
                else_checker.visit_block(else_block);

                let live_roots = |checker: &Self| {
                    checker
                        .roots
                        .iter()
                        .filter_map(|(local, dropped)| (!*dropped).then_some(local.clone()))
                        .collect::<BTreeSet<_>>()
                };
                let then_live = live_roots(&then_checker);
                let else_live = live_roots(&else_checker);
                if then_live != else_live {
                    self.invalid = true;
                }

                let mut roots = then_checker.roots.clone();
                for local in else_checker.roots.keys() {
                    roots.entry(local.clone()).or_insert(true);
                }
                for (local, dropped) in &mut roots {
                    *dropped = !then_live.contains(local);
                }
                let mut allowed_root_loads = then_checker.allowed_root_loads.clone();
                allowed_root_loads.extend(else_checker.allowed_root_loads.iter().cloned());

                let mut operation_attempts = then_checker.operation_attempts.clone();
                operation_attempts.extend(else_checker.operation_attempts.iter().cloned());
                let mut actions = branch_base.actions;
                actions.extend(
                    then_checker
                        .actions
                        .into_iter()
                        .skip(branch_action_prefix_len),
                );
                actions.extend(
                    else_checker
                        .actions
                        .into_iter()
                        .skip(branch_action_prefix_len),
                );

                self.invalid = branch_base.invalid
                    || then_checker.invalid
                    || else_checker.invalid
                    || then_live != else_live;
                self.roots = roots;
                self.allowed_root_loads = allowed_root_loads;
                self.operation_attempts = operation_attempts;
                self.actions = actions;
                debug_assert!(self.actions.len() >= action_prefix_len);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                // Match is admitted only as a Map-free control-flow tail. The
                // source CFG/MIR receipt carries the arm and join topology;
                // this checker therefore traverses every guard/body to keep
                // scalar uses visible while rejecting any Map operation that
                // appears in the scrutinee or an arm.
                let base = self.clone();
                let base_actions = base.actions.len();
                let base_attempts = base.operation_attempts.clone();
                if !self.is_bool(&scrutinee.ty)
                    && !matches!(
                        self.types.get(&scrutinee.ty),
                        Some(ResolvedType::DynamicAny { .. })
                    )
                {
                    self.invalid = true;
                }
                self.visit_expr(scrutinee, None);
                if self.actions.len() != base_actions
                    || self.operation_attempts != base_attempts
                    || self.invalid
                {
                    self.invalid = true;
                }
                let after_scrutinee = self.clone();
                for arm in arms {
                    let mut arm_checker = after_scrutinee.clone();
                    if let Some(guard) = &arm.guard {
                        arm_checker.visit_expr(guard, None);
                    }
                    arm_checker.visit_expr(&arm.body, None);
                    if arm_checker.invalid
                        || arm_checker.actions.len() != base_actions
                        || arm_checker.operation_attempts != base_attempts
                    {
                        self.invalid = true;
                    }
                }
                // Keep the pre-match owner state. Any arm-local Map action or
                // root mutation has already made the receipt invalid above.
                self.roots = after_scrutinee.roots;
                self.allowed_root_loads = after_scrutinee.allowed_root_loads;
                self.operation_attempts = after_scrutinee.operation_attempts;
                self.actions = after_scrutinee.actions;
            }
            ResolvedExprKind::Lambda(lambda) => {
                // No-capture closures are checked through their own CFG and
                // receipt. Capturing an ordinary Map root remains outside
                // this profile because it would alias caller ownership.
                if lambda.captures.iter().any(|local| {
                    self.body
                        .locals
                        .get(local)
                        .is_some_and(|binding| checker_is_record(self.types, &binding.ty))
                }) {
                    self.invalid = true;
                }
            }
            ResolvedExprKind::Block(block) => self.visit_block(block),
            ResolvedExprKind::Scope { .. }
            | ResolvedExprKind::Comptime(_)
            | ResolvedExprKind::Quote(_)
            | ResolvedExprKind::Range { .. }
            | ResolvedExprKind::Slice { .. } => self.invalid = true,
            ResolvedExprKind::Load(place) => {
                if let Some(target) = direct_bind {
                    if self.roots.contains_key(&place.base) {
                        self.visit_clone(expression, target);
                    }
                }
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }

    fn is_get_any_tuple(&self, ty: &ResolvedTypeId) -> bool {
        matches!(self.types.get(ty), Some(ResolvedType::Tuple(elements))
            if elements.len() == 2 && self.is_bool(&elements[0])
                && matches!(self.types.get(&elements[1]), Some(ResolvedType::DynamicAny { .. })))
    }

    fn is_values_list(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Nominal { item, arguments, .. })
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && matches!(self.types.get(&arguments[0]), Some(ResolvedType::DynamicAny { .. }))
        )
    }

    fn visit_get_any(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        if call.arguments.len() != 2 || !self.is_get_any_tuple(&expression.ty) {
            self.invalid = true;
            return;
        }
        let root = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let Some(local) = self.live_root_load(root) else {
            self.invalid = true;
            return;
        };
        if !self.is_string(&key.ty) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(root);
        self.actions.push(self.action(
            OrdinaryMapOwnedGetI32StringV1ActionKind::GetAny,
            expression.node_id.clone(),
            local,
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Self::direct_key_local(key),
            None,
            None,
            Some(expression.ty.clone()),
            None,
        ));
    }

    fn visit_values(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        if call.arguments.len() != 1 || !self.is_values_list(&expression.ty) {
            self.invalid = true;
            return;
        }
        let root = &call.arguments[0].value;
        let Some(local) = self.live_root_load(root) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&root.ty) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(root);
        self.actions.push(self.action(
            OrdinaryMapOwnedGetI32StringV1ActionKind::Values,
            expression.node_id.clone(),
            local,
            None,
            None,
            None,
            None,
            None,
            Some(expression.ty.clone()),
            None,
        ));
    }

    fn visit_new(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let Some(local) = direct_bind else {
            self.invalid = true;
            return;
        };
        if !call.arguments.is_empty()
            || !self.is_record(&expression.ty)
            || !self
                .body
                .locals
                .get(local)
                .is_some_and(|binding| binding.ty == expression.ty)
            || self.roots.contains_key(local)
        {
            self.invalid = true;
            return;
        }
        self.roots.insert(local.clone(), false);
        self.actions.push(self.action(
            OrdinaryMapOwnedGetI32StringV1ActionKind::New,
            expression.node_id.clone(),
            local.clone(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ));
    }

    fn visit_set(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let Some(target) = direct_bind else {
            self.invalid = true;
            return;
        };
        if call.arguments.len() != 3 || self.roots.contains_key(target) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let value = &call.arguments[2].value;
        let Some(source_local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&source.ty)
            || !self.is_record(&expression.ty)
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|binding| binding.ty == expression.ty)
            || !self.valid_key(key)
            || !(self.is_i32(&value.ty) || self.is_string(&value.ty))
        {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.roots.insert(target.clone(), false);
        self.actions.push(self.action(
            OrdinaryMapOwnedGetI32StringV1ActionKind::Set,
            expression.node_id.clone(),
            target.clone(),
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Self::direct_key_local(key),
            Some(value.node_id.clone()),
            Some(value.ty.clone()),
            None,
            Some(source_local),
        ));
    }

    fn visit_remove(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
        direct_bind: Option<&ResolvedLocalId>,
    ) {
        let Some(target) = direct_bind else {
            self.invalid = true;
            return;
        };
        if call.arguments.len() != 2 || self.roots.contains_key(target) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let Some(source_local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&source.ty)
            || !self.is_record(&expression.ty)
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|binding| binding.ty == expression.ty)
            || !self.valid_key(key)
        {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.roots.insert(target.clone(), false);
        self.actions.push(self.action(
            OrdinaryMapOwnedGetI32StringV1ActionKind::Remove,
            expression.node_id.clone(),
            target.clone(),
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Self::direct_key_local(key),
            None,
            None,
            None,
            Some(source_local),
        ));
    }

    fn visit_size(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        if call.arguments.len() != 1 || !self.is_i32(&expression.ty) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let Some(local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&source.ty) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.actions.push(self.action(
            OrdinaryMapOwnedGetI32StringV1ActionKind::Size,
            expression.node_id.clone(),
            local,
            None,
            None,
            None,
            None,
            None,
            Some(expression.ty.clone()),
            None,
        ));
    }

    fn visit_has_key(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        if call.arguments.len() != 2 || !self.is_bool(&expression.ty) {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let Some(local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&source.ty) || !self.valid_key(key) {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.actions.push(self.action(
            OrdinaryMapOwnedGetI32StringV1ActionKind::HasKey,
            expression.node_id.clone(),
            local,
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Self::direct_key_local(key),
            None,
            None,
            Some(expression.ty.clone()),
            None,
        ));
    }

    fn visit_get(&mut self, expression: &ResolvedExpr, call: &ResolvedCall, string: bool) {
        if call.arguments.len() != 2 {
            self.invalid = true;
            return;
        }
        let source = &call.arguments[0].value;
        let key = &call.arguments[1].value;
        let Some(local) = self.live_root_load(source) else {
            self.invalid = true;
            return;
        };
        if !self.is_record(&source.ty)
            || !self.valid_key(key)
            || !self.is_result(
                &expression.ty,
                if string {
                    crate::core::PrimitiveType::String
                } else {
                    crate::core::PrimitiveType::I32
                },
                crate::core::PrimitiveType::I32,
            )
        {
            self.invalid = true;
            return;
        }
        self.allow_root_load(source);
        self.actions.push(self.action(
            if string {
                OrdinaryMapOwnedGetI32StringV1ActionKind::GetString
            } else {
                OrdinaryMapOwnedGetI32StringV1ActionKind::GetI32
            },
            expression.node_id.clone(),
            local,
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Self::direct_key_local(key),
            None,
            None,
            Some(expression.ty.clone()),
            None,
        ));
    }

    fn valid_key(&self, expression: &ResolvedExpr) -> bool {
        // Retain the complete resolved String expression. This accepts
        // dynamic values and embedded NUL bytes without C-string truncation
        // or a static-key policy leaking in from older profiles.
        self.is_string(&expression.ty)
    }

    fn direct_key_local(expression: &ResolvedExpr) -> Option<ResolvedLocalId> {
        match &expression.kind {
            ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                Some(place.base.clone())
            }
            _ => None,
        }
    }

    fn live_root_load(&self, expression: &ResolvedExpr) -> Option<ResolvedLocalId> {
        let ResolvedExprKind::Load(place) = &expression.kind else {
            return None;
        };
        if !place.projections.is_empty() || self.roots.get(&place.base) != Some(&false) {
            return None;
        }
        Some(place.base.clone())
    }

    fn allow_root_load(&mut self, expression: &ResolvedExpr) {
        self.allowed_root_loads.insert(expression.node_id.clone());
    }

    #[allow(clippy::too_many_arguments)]
    fn action(
        &self,
        kind: OrdinaryMapOwnedGetI32StringV1ActionKind,
        point: NodeId,
        local: ResolvedLocalId,
        key: Option<NodeId>,
        key_type: Option<ResolvedTypeId>,
        key_local: Option<ResolvedLocalId>,
        value: Option<NodeId>,
        value_type: Option<ResolvedTypeId>,
        result_type: Option<ResolvedTypeId>,
        source_local: Option<ResolvedLocalId>,
    ) -> OrdinaryMapOwnedGetI32StringV1Action {
        let root = Self::resource_identity(&local);
        let source = source_local.as_ref().map(Self::resource_identity);
        OrdinaryMapOwnedGetI32StringV1Action {
            profile_version: OrdinaryMapOwnedGetI32StringV1ProfileVersion::V1,
            kind,
            point,
            local,
            root,
            key,
            key_type,
            key_local,
            value,
            value_type,
            result_type,
            source_local,
            source,
        }
    }

    fn resource_identity(local: &ResolvedLocalId) -> ResourceId {
        ResourceId(NodeId(format!(
            "ordinary-map-owned-get-i32-string-v1:{}",
            local.0 .0
        )))
    }

    fn is_result(
        &self,
        ty: &ResolvedTypeId,
        ok: crate::core::PrimitiveType,
        error: crate::core::PrimitiveType,
    ) -> bool {
        let result_parts = match self.types.get(ty) {
            Some(ResolvedType::Result { ok, error }) => Some((ok, error)),
            Some(ResolvedType::Nominal {
                item, arguments, ..
            }) if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => {
                Some((&arguments[0], &arguments[1]))
            }
            _ => None,
        };
        result_parts.is_some_and(|(ok_type, error_type)| {
            matches!(self.types.get(ok_type), Some(ResolvedType::Primitive(value)) if *value == ok)
                && matches!(self.types.get(error_type), Some(ResolvedType::Primitive(value)) if *value == error)
        })
    }

    fn is_record(&self, ty: &ResolvedTypeId) -> bool {
        checker_is_record(self.types, ty)
    }

    fn is_i32(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
        )
    }

    fn is_string(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
        )
    }

    fn is_bool(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::Bool))
        )
    }

    fn action_has_key(action: &OrdinaryMapOwnedGetI32StringV1Action) -> bool {
        action.key.is_some() || action.key_type.is_some() || action.key_local.is_some()
    }

    fn action_has_value(action: &OrdinaryMapOwnedGetI32StringV1Action) -> bool {
        action.value.is_some() || action.value_type.is_some()
    }
}

fn checker_is_record(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(
        types.get(ty),
        Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrdinaryMapOwnedGetResultUseContext {
    Observe,
    Transfer,
    Escape,
    Return,
}

#[derive(Debug, Clone)]
struct OrdinaryMapOwnedGetResultUseState {
    binding: Option<ResolvedLocalId>,
    observed: bool,
    observed_point: Option<NodeId>,
    settlement: Option<(OrdinaryMapOwnedGetResultSettlementKind, NodeId)>,
}

impl OrdinaryMapOwnedGetResultUseState {
    fn observe(&mut self, point: &NodeId) {
        self.observed = true;
        self.observed_point.get_or_insert_with(|| point.clone());
    }

    fn settle(&mut self, kind: OrdinaryMapOwnedGetResultSettlementKind, point: &NodeId) {
        if self.settlement.is_some() {
            // An owner cannot be consumed twice. Preserve a fail-closed
            // source receipt rather than letting the first apparent transfer
            // hide a later explicit drop or second escape.
            self.settlement = Some((
                OrdinaryMapOwnedGetResultSettlementKind::Unsettled,
                point.clone(),
            ));
        } else {
            self.settlement = Some((kind, point.clone()));
        }
    }
}

/// Build source-level receipts for the aggregate result of `map_get` and
/// `values`. The operation ledger proves the Map root edge; this pass proves
/// the separate result edge. It intentionally records scope-only cleanup as
/// `ScopeDrop` and leaves observation-only uses as `Unsettled`, so a backend
/// cannot turn a synthetic epilogue drop into an ownership receipt.
fn collect_ordinary_map_owned_get_result_receipts(
    body: &ResolvedBody,
    actions: &[OrdinaryMapOwnedGetI32StringV1Action],
    attempts: &BTreeSet<OrdinaryMapOwnedGetI32StringV1Attempt>,
) -> Vec<OrdinaryMapOwnedGetResultReceipt> {
    let mut states = actions
        .iter()
        .filter(|action| {
            matches!(
                action.kind,
                OrdinaryMapOwnedGetI32StringV1ActionKind::GetAny
                    | OrdinaryMapOwnedGetI32StringV1ActionKind::Values
            )
        })
        .map(|action| {
            let binding = attempts
                .iter()
                .find(|attempt| attempt.point == action.point)
                .and_then(|attempt| attempt.result_local.clone());
            (
                action.point.clone(),
                OrdinaryMapOwnedGetResultUseState {
                    binding,
                    observed: false,
                    observed_point: None,
                    settlement: None,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    if states.is_empty() {
        return Vec::new();
    }

    fn owned_get_call(expression: &ResolvedExpr) -> bool {
        matches!(
            &expression.kind,
            ResolvedExprKind::Call(ResolvedCall {
                callee: ResolvedCallee::Builtin(name),
                ..
            }) if matches!(name.as_str(), "map_get" | "values")
        )
    }

    fn call_arg_context(call: &ResolvedCall) -> OrdinaryMapOwnedGetResultUseContext {
        let ResolvedCallee::Builtin(name) = &call.callee else {
            // A user function, method, closure, or FFI boundary is an escape
            // edge until its own owner contract proves a narrower borrow.
            return OrdinaryMapOwnedGetResultUseContext::Transfer;
        };
        // These builtins observe their argument without taking aggregate
        // ownership. Everything else is conservatively treated as a transfer
        // boundary, matching the canonical MIR owner checker.
        if matches!(
            name.as_str(),
            "len"
                | "builtin.method.list.len"
                | "map_size"
                | "has_key"
                | "map_get"
                | "map_get_i32"
                | "map_get_string"
                | "keys"
                | "values"
                | "println"
                | "print_line"
                | "print_raw"
                | "print_format"
        ) {
            OrdinaryMapOwnedGetResultUseContext::Observe
        } else {
            OrdinaryMapOwnedGetResultUseContext::Transfer
        }
    }

    fn mark_expression(
        expression: &ResolvedExpr,
        context: OrdinaryMapOwnedGetResultUseContext,
        states: &mut BTreeMap<NodeId, OrdinaryMapOwnedGetResultUseState>,
    ) {
        if let Some(state) = states.get_mut(&expression.node_id) {
            match context {
                OrdinaryMapOwnedGetResultUseContext::Observe => state.observe(&expression.node_id),
                OrdinaryMapOwnedGetResultUseContext::Transfer => state.settle(
                    OrdinaryMapOwnedGetResultSettlementKind::Transfer,
                    &expression.node_id,
                ),
                OrdinaryMapOwnedGetResultUseContext::Escape => state.settle(
                    OrdinaryMapOwnedGetResultSettlementKind::Escape,
                    &expression.node_id,
                ),
                OrdinaryMapOwnedGetResultUseContext::Return => state.settle(
                    OrdinaryMapOwnedGetResultSettlementKind::Return,
                    &expression.node_id,
                ),
            }
            return;
        }

        if let ResolvedExprKind::Load(place) = &expression.kind {
            if place.projections.is_empty() {
                let matching = states
                    .iter_mut()
                    .find(|(_, state)| state.binding.as_ref() == Some(&place.base));
                if let Some((_, state)) = matching {
                    match context {
                        OrdinaryMapOwnedGetResultUseContext::Observe => {
                            state.observe(&expression.node_id)
                        }
                        OrdinaryMapOwnedGetResultUseContext::Transfer => state.settle(
                            OrdinaryMapOwnedGetResultSettlementKind::Transfer,
                            &expression.node_id,
                        ),
                        OrdinaryMapOwnedGetResultUseContext::Escape => state.settle(
                            OrdinaryMapOwnedGetResultSettlementKind::Escape,
                            &expression.node_id,
                        ),
                        OrdinaryMapOwnedGetResultUseContext::Return => state.settle(
                            OrdinaryMapOwnedGetResultSettlementKind::Return,
                            &expression.node_id,
                        ),
                    }
                }
            }
        }
    }

    fn visit_expr(
        expression: &ResolvedExpr,
        context: OrdinaryMapOwnedGetResultUseContext,
        states: &mut BTreeMap<NodeId, OrdinaryMapOwnedGetResultUseState>,
    ) {
        if owned_get_call(expression) {
            // A direct binding is the result owner introduction point. Its
            // surrounding Bind is handled by visit_block, so do not mark the
            // operation itself as transferred a second time.
            let direct_binding = states
                .get(&expression.node_id)
                .is_some_and(|state| state.binding.is_some());
            if !direct_binding {
                mark_expression(expression, context, states);
            }
        } else {
            mark_expression(expression, context, states);
        }

        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                // A projection observes the aggregate and does not itself
                // discharge its owner. The caller still needs an explicit
                // drop or a separate transfer of the aggregate.
                visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                if let ResolvedValueProjection::Index(index) = projection {
                    visit_expr(index, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                visit_expr(left, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                visit_expr(right, OrdinaryMapOwnedGetResultUseContext::Observe, states);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Await(operand) => visit_expr(
                operand,
                OrdinaryMapOwnedGetResultUseContext::Observe,
                states,
            ),
            ResolvedExprKind::Spawn(operand) => {
                visit_expr(operand, OrdinaryMapOwnedGetResultUseContext::Escape, states)
            }
            ResolvedExprKind::Call(call) => {
                let argument_context = call_arg_context(call);
                for argument in &call.arguments {
                    visit_expr(&argument.value, argument_context, states);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                }
            }
            ResolvedExprKind::Map(values) => {
                for (key, value) in values {
                    visit_expr(key, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                    visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                visit_expr(
                    iterable,
                    OrdinaryMapOwnedGetResultUseContext::Observe,
                    states,
                );
                if let Some(guard) = guard {
                    visit_expr(guard, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => visit_expr(
                receiver,
                OrdinaryMapOwnedGetResultUseContext::Observe,
                states,
            ),
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    visit_expr(
                        &field.value,
                        OrdinaryMapOwnedGetResultUseContext::Escape,
                        states,
                    );
                }
                if let Some(rest) = rest {
                    visit_expr(rest, OrdinaryMapOwnedGetResultUseContext::Escape, states);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => visit_block(block, context, states),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_expr(
                    condition,
                    OrdinaryMapOwnedGetResultUseContext::Observe,
                    states,
                );
                visit_block(then_block, context, states);
                visit_block(else_block, context, states);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                visit_expr(
                    scrutinee,
                    OrdinaryMapOwnedGetResultUseContext::Observe,
                    states,
                );
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit_expr(guard, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                    }
                    visit_expr(&arm.body, context, states);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                visit_expr(start, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                visit_expr(end, OrdinaryMapOwnedGetResultUseContext::Observe, states);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                visit_expr(target, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                if let Some(start) = start {
                    visit_expr(start, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
                if let Some(end) = end {
                    visit_expr(end, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
            }
            ResolvedExprKind::Lambda(lambda) => {
                for capture in &lambda.captures {
                    if let Some((_, state)) = states
                        .iter_mut()
                        .find(|(_, state)| state.binding.as_ref() == Some(capture))
                    {
                        state.settle(
                            OrdinaryMapOwnedGetResultSettlementKind::Escape,
                            &expression.node_id,
                        );
                    }
                }
            }
            ResolvedExprKind::Load(_)
            | ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }

    fn visit_block(
        block: &ResolvedBlock,
        result_context: OrdinaryMapOwnedGetResultUseContext,
        states: &mut BTreeMap<NodeId, OrdinaryMapOwnedGetResultUseState>,
    ) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    let direct_result_binding = initializer.as_ref().is_some_and(|value| {
                        owned_get_call(value)
                            && matches!(pattern.kind, ResolvedPatternKind::Binding { .. })
                    });
                    if let Some(value) = initializer {
                        if !direct_result_binding {
                            visit_expr(
                                value,
                                OrdinaryMapOwnedGetResultUseContext::Transfer,
                                states,
                            );
                        }
                    }
                }
                ResolvedStmtKind::Assign { value, .. } => {
                    visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Transfer, states)
                }
                ResolvedStmtKind::Return { value, .. } => {
                    if let Some(value) = value {
                        visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Return, states)
                    }
                }
                ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Transfer, states)
                    }
                }
                ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Observe, states),
                ResolvedStmtKind::Drop(places) => {
                    for place in places {
                        if place.projections.is_empty() {
                            for state in states.values_mut() {
                                if state.binding.as_ref() == Some(&place.base) {
                                    state.settle(
                                        OrdinaryMapOwnedGetResultSettlementKind::ExplicitDrop,
                                        &statement.node_id,
                                    );
                                }
                            }
                        }
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    visit_expr(
                        condition,
                        OrdinaryMapOwnedGetResultUseContext::Observe,
                        states,
                    );
                    visit_block(body, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                }
                | ResolvedStmtKind::For {
                    iterable: initializer,
                    body,
                    ..
                } => {
                    visit_expr(
                        initializer,
                        OrdinaryMapOwnedGetResultUseContext::Observe,
                        states,
                    );
                    visit_block(body, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    visit_expr(
                        initializer,
                        OrdinaryMapOwnedGetResultUseContext::Observe,
                        states,
                    );
                    visit_block(
                        then_block,
                        OrdinaryMapOwnedGetResultUseContext::Observe,
                        states,
                    );
                    if let Some(else_block) = else_block {
                        visit_block(
                            else_block,
                            OrdinaryMapOwnedGetResultUseContext::Observe,
                            states,
                        );
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    visit_block(body, OrdinaryMapOwnedGetResultUseContext::Observe, states)
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    visit_expr(value, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                    visit_block(body, OrdinaryMapOwnedGetResultUseContext::Observe, states);
                }
                ResolvedStmtKind::Continue
                | ResolvedStmtKind::NestedCallable(_)
                | ResolvedStmtKind::Math(_) => {}
            }
        }
        if let Some(result) = &block.result {
            visit_expr(result, result_context, states);
        }
    }

    visit_block(
        &body.root,
        OrdinaryMapOwnedGetResultUseContext::Return,
        &mut states,
    );

    states
        .into_iter()
        .filter_map(|(operation, state)| {
            let action = actions.iter().find(|action| action.point == operation)?;
            let (settlement, settlement_point) = state.settlement.unwrap_or_else(|| {
                if state.observed {
                    (
                        OrdinaryMapOwnedGetResultSettlementKind::Unsettled,
                        state
                            .observed_point
                            .clone()
                            .unwrap_or_else(|| operation.clone()),
                    )
                } else {
                    (
                        OrdinaryMapOwnedGetResultSettlementKind::ScopeDrop,
                        operation.clone(),
                    )
                }
            });
            Some(OrdinaryMapOwnedGetResultReceipt {
                profile_version: action.profile_version,
                operation: operation.clone(),
                kind: action.kind,
                result: operation,
                binding: state.binding,
                settlement,
                settlement_point: Some(settlement_point),
            })
        })
        .collect()
}

struct ActionEmitter<'a> {
    cfg: &'a CallableCfg,
    body: &'a ResolvedBody,
    signature: &'a ResolvedSignature,
    types: &'a ResolvedTypeTable,
    /// RECORD-LIN-001 (0.1.9, Phase B): qualified names of user records/unions
    /// whose fields contain a linear resource. Computed once at resolved
    /// lowering; `is_linear` consults it so `Plain { data: c }` is tracked
    /// exactly like `List<cap>`.
    linear_record_names: &'a BTreeSet<String>,
    locations: BTreeMap<NodeId, CfgLocation>,
    /// Resource catalog: linear local → the resource identities it currently
    /// owns. A single binding may own SEVERAL resources after an aggregate
    /// merge (`let x = (a, b)` moves both atoms into `x`), so the value is a
    /// vector in construction order. Wave-2 (audit G-1): single-valued
    /// identity stranded every source past the first and could never be
    /// re-keyed by reassignment.
    resources: BTreeMap<ResolvedLocalId, Vec<ResourceId>>,
    actions: Vec<CanonicalResourceAction>,
    loans: Vec<Loan>,
    errors: Vec<Diagnostic>,
    /// Audit 2026-08-05 (wave-2, H-6): anonymous temporary borrows
    /// (`inc(&mut x)`) have no named reference, so loan liveness cannot end
    /// them. They are parked here and receive a synthesized BorrowEnd at the
    /// enclosing statement's terminating CFG point (mirroring named-borrow
    /// NLL). Entries are (loan id, borrowed resource).
    pending_anonymous_loans: Vec<(LoanId, ResourceId)>,
    /// 0.31.22 Drop/Transition IR 防漏网断言：跟踪已消费的资源
    /// 用于 debug_assert 检测二次消费 bug
    /// P2/P1-6: Double-consumption debug assertion infrastructure.
    /// Currently disabled (false positives on alias/branch scenarios).
    /// Re-enabling requires CFG path analysis to distinguish:
    /// - Same resource consumed twice in one basic block (real bug)
    /// - Same resource consumed in different branches (legal)
    /// - Alias-induced duplicate tracking (legal)
    /// Linear-resource double-drop tracker (RESOURCE-LINEAR-001 debug signal).
    /// Referenced by the always-on mimi_assert! in emit_drop — must exist in
    /// ALL build profiles (a previous #[cfg(debug_assertions)] gate broke
    /// `cargo build --release`: mimi_assert! is not compiled out in release).
    consumed_resources: BTreeSet<ResourceId>,
    /// 0.36.43: E0304-rejected element extractions (`v[0]` / `v[1..]` /
    /// `(a, b).0` on non-droppable linear containers). The rejection is
    /// diagnostic-only by design, but the surrounding lowering kept pairing
    /// the extracted place into binds/calls/drops — fabricating a transfer
    /// for a value that never moved, so a later legitimate `drop(v)` hit the
    /// RESOURCE-LINEAR-001 double-drop debug signal. Consumers skip places
    /// in this set (error-path hygiene; the function is already invalid).
    rejected_extraction_places: BTreeSet<Place>,
    /// 0.36.43: set by reject_index_read_extraction whenever THIS visited
    /// expression was rejected; the Bind arm uses it to skip pairing an
    /// initializer whose extraction already failed (the E0304 stands alone).
    last_visit_rejected: bool,
    /// 0.36.46: 已做定向头提取（`xs[0]`）的容器基——元素认领后容器保留余部
    /// 义务，每容器至多一次索引提取（二次认领同一位置 = 超认领 → E0304）。
    extracted_containers: BTreeSet<ResolvedLocalId>,
    /// 0.36.46: 绑定初始化器访问上下文中放行的定向提取基——Bind 臂在
    /// visit_expr 后据此做专门的"元素 Introduce + 容器余部保留"记账。
    directional_extraction_base: Option<ResolvedLocalId>,
    /// 0.36.46: 当前是否位于绑定初始化器访问中——定向提取仅对 let-绑定面开
    /// （调用实参 `sink(xs[0])` / 其他位置保持 fail-closed E0304）。
    in_bind_initializer: bool,
    /// Checker-owned Map root lifecycle facts projected into MIR. These are
    /// intentionally narrower than generic linear-resource actions: they
    /// bind a persistent Map update or a borrowed size observation to one
    /// exact call and root resource.
    map_root_actions: Vec<MapRootAction>,
    /// Stable source points for every recognized MapRoot construction
    /// attempt, including non-local temporary constructions.
    map_root_new_attempts: BTreeSet<NodeId>,
    /// Surface `Record` values become internal Map roots only when this
    /// checker pass proves a closed local lifecycle. This leaves legacy
    /// dynamic Record/JSON uses on their existing type contract.
    map_root_locals: BTreeSet<ResolvedLocalId>,
    live_map_root_locals: BTreeSet<ResolvedLocalId>,
    /// Payload type chosen by the first Set in each persistent root chain.
    /// The current closed native/runtime profile keeps one exact Any payload
    /// type per chain so every consumer can validate a single ownership ABI.
    map_root_payload_types: BTreeMap<ResolvedLocalId, Option<ResolvedTypeId>>,
    permitted_map_root_loads: HashSet<NodeId>,
    map_root_profile_invalid: bool,
    direct_map_root_binding: Option<(NodeId, ResolvedLocalId)>,
}

impl<'a> ActionEmitter<'a> {
    fn new(
        cfg: &'a CallableCfg,
        body: &'a ResolvedBody,
        signature: &'a ResolvedSignature,
        types: &'a ResolvedTypeTable,
        linear_record_names: &'a BTreeSet<String>,
    ) -> Self {
        let locations = cfg
            .blocks
            .iter()
            .flat_map(|(block, value)| {
                value.points.iter().map(move |point| {
                    (
                        point.source.node.clone(),
                        CfgLocation {
                            block: block.clone(),
                            point: point.source.node.clone(),
                            edge: None,
                        },
                    )
                })
            })
            .collect();
        Self {
            cfg,
            body,
            signature,
            types,
            linear_record_names,
            locations,
            resources: BTreeMap::new(),
            actions: Vec::new(),
            loans: Vec::new(),
            errors: Vec::new(),
            pending_anonymous_loans: Vec::new(),
            consumed_resources: BTreeSet::new(),
            rejected_extraction_places: BTreeSet::new(),
            last_visit_rejected: false,
            extracted_containers: BTreeSet::new(),
            directional_extraction_base: None,
            in_bind_initializer: false,
            map_root_actions: Vec::new(),
            map_root_new_attempts: BTreeSet::new(),
            map_root_locals: BTreeSet::new(),
            live_map_root_locals: BTreeSet::new(),
            map_root_payload_types: BTreeMap::new(),
            permitted_map_root_loads: HashSet::new(),
            map_root_profile_invalid: false,
            direct_map_root_binding: None,
        }
    }

    fn emit(mut self) -> Result<ResourceAnalysis, Vec<Diagnostic>> {
        self.build_resource_catalog();
        self.reject_linear_callable_captures();
        self.introduce_parameters();
        self.visit_block(&self.body.root, true);
        if self.errors.is_empty() {
            // Re-run the closed local-root recognizer from the Checker-owned
            // ResolvedBody. This yields an independent MapAnyOwner ledger and
            // construction-attempt set; no MapRoot receipt is used as input.
            let (map_any_owner_actions, map_any_owner_new_attempts) =
                Self::collect_map_any_owner_v1(
                    self.cfg,
                    self.body,
                    self.signature,
                    self.types,
                    self.linear_record_names,
                );
            let (
                ordinary_map_owner_lifecycle_v1_actions,
                ordinary_map_owner_lifecycle_v1_new_attempts,
                ordinary_map_owner_lifecycle_v1_operation_attempts,
            ) = OrdinaryMapOwnerLifecycleChecker::collect(self.cfg, self.body, self.types);
            let (
                ordinary_map_owned_get_i32_string_v1_actions,
                ordinary_map_owned_get_i32_string_v1_new_attempts,
                ordinary_map_owned_get_i32_string_v1_operation_attempts,
                ordinary_map_owned_get_i32_string_v1_cfg_receipt,
            ) = OrdinaryMapOwnedGetI32StringV1Checker::collect(self.cfg, self.body, self.types);
            let (
                ordinary_map_owned_any_v2_actions,
                ordinary_map_owned_any_v2_operation_attempts,
                ordinary_map_owned_any_v2_cfg_receipt,
            ) = OrdinaryMapOwnedAnyV2Checker::collect(self.cfg, self.body, self.types);
            let map_root_new_attempts = self.map_root_new_attempts.clone();
            // 0.31.16: collect flow state resources as auto-droppable.
            // Flow states represent data that can be safely discarded at
            // scope exit, unlike Cap/SessionChan which require explicit
            // consumption.
            // P0-5: containers of flow states (Result/Option/Tuple/Array/Slice)
            // are also droppable iff all linear elements inside are flow states.
            let droppable: BTreeSet<ResourceId> = self
                .resources
                .iter()
                .filter(|(local, _)| {
                    self.body
                        .locals
                        .get(local)
                        .is_some_and(|l| self.is_linear(&l.ty) && self.is_droppable_type(&l.ty))
                })
                .flat_map(|(_, resources)| resources.iter().cloned())
                .collect();
            // Recursive resolved-tree visitation may append an outer call's
            // candidate before a nested argument call. MIR executes the
            // nested call first, so serialize this checker receipt in the
            // single CFG block's point order instead of relying on visitor
            // order. Missing source points withhold the whole candidate.
            if self.cfg.blocks.len() == 1 {
                if let Some(block) = self.cfg.blocks.values().next() {
                    let point_order = block
                        .points
                        .iter()
                        .enumerate()
                        .map(|(index, point)| (point.source.node.clone(), index))
                        .collect::<BTreeMap<_, _>>();
                    self.map_root_actions.sort_by_key(|action| {
                        point_order
                            .get(&action.point)
                            .copied()
                            .unwrap_or(usize::MAX)
                    });
                    if self
                        .map_root_actions
                        .iter()
                        .any(|action| !point_order.contains_key(&action.point))
                    {
                        self.map_root_profile_invalid = true;
                    }
                } else {
                    self.map_root_profile_invalid = true;
                }
            }
            // The initial MIR Map slice is straight-line and local. If any
            // root escapes, remains live at exit, or crosses control flow,
            // withhold the receipt so no MIR/default route can infer it from
            // Record shape alone. The legacy dynamic Record path remains
            // available for those existing programs.
            if self.cfg.blocks.len() != 1
                || !self.cfg.edges.is_empty()
                || self.map_root_profile_invalid
                || !self.live_map_root_locals.is_empty()
                || !self.permitted_map_root_loads.is_empty()
                || !self
                    .map_root_actions
                    .iter()
                    .any(|action| action.kind == MapRootActionKind::New)
            {
                self.map_root_actions.clear();
            }
            let map_root_actions = std::mem::take(&mut self.map_root_actions);
            let mut analysis = analyze_canonical(
                self.cfg,
                self.actions,
                self.loans,
                &BTreeMap::new(),
                &droppable,
            )?;
            analysis.map_root_actions = map_root_actions;
            analysis.map_root_new_attempts = map_root_new_attempts;
            analysis.map_any_owner_actions = map_any_owner_actions;
            analysis.map_any_owner_new_attempts = map_any_owner_new_attempts;
            analysis.ordinary_map_owner_lifecycle_v1_actions =
                ordinary_map_owner_lifecycle_v1_actions;
            analysis.ordinary_map_owner_lifecycle_v1_new_attempts =
                ordinary_map_owner_lifecycle_v1_new_attempts;
            analysis.ordinary_map_owner_lifecycle_v1_operation_attempts =
                ordinary_map_owner_lifecycle_v1_operation_attempts;
            analysis.ordinary_map_owned_get_i32_string_v1_actions =
                ordinary_map_owned_get_i32_string_v1_actions;
            analysis.ordinary_map_owned_get_i32_string_v1_new_attempts =
                ordinary_map_owned_get_i32_string_v1_new_attempts;
            analysis.ordinary_map_owned_get_i32_string_v1_operation_attempts =
                ordinary_map_owned_get_i32_string_v1_operation_attempts;
            analysis.ordinary_map_owned_get_i32_string_v1_cfg_receipt =
                ordinary_map_owned_get_i32_string_v1_cfg_receipt;
            analysis.ordinary_map_owned_any_v2_actions = ordinary_map_owned_any_v2_actions;
            analysis.ordinary_map_owned_any_v2_operation_attempts =
                ordinary_map_owned_any_v2_operation_attempts;
            analysis.ordinary_map_owned_any_v2_cfg_receipt = ordinary_map_owned_any_v2_cfg_receipt;
            analysis.managed_any_cfg_receipt = Some(crate::core::ManagedAnyCfgReceipt::from_cfg(
                self.cfg, self.body,
            ));
            Ok(analysis)
        } else {
            Err(self.errors)
        }
    }

    fn collect_map_any_owner_v1(
        cfg: &'a CallableCfg,
        body: &'a ResolvedBody,
        signature: &'a ResolvedSignature,
        types: &'a ResolvedTypeTable,
        linear_record_names: &'a BTreeSet<String>,
    ) -> (Vec<MapAnyOwnerAction>, BTreeSet<NodeId>) {
        let mut checker = Self::new(cfg, body, signature, types, linear_record_names);
        checker.build_resource_catalog();
        checker.reject_linear_callable_captures();
        checker.introduce_parameters();
        checker.visit_block(&body.root, true);

        let attempts = checker.map_root_new_attempts.clone();
        if cfg.blocks.len() == 1 {
            if let Some(block) = cfg.blocks.values().next() {
                let point_order = block
                    .points
                    .iter()
                    .enumerate()
                    .map(|(index, point)| (point.source.node.clone(), index))
                    .collect::<BTreeMap<_, _>>();
                checker.map_root_actions.sort_by_key(|action| {
                    point_order
                        .get(&action.point)
                        .copied()
                        .unwrap_or(usize::MAX)
                });
                if checker
                    .map_root_actions
                    .iter()
                    .any(|action| !point_order.contains_key(&action.point))
                {
                    checker.map_root_profile_invalid = true;
                }
            } else {
                checker.map_root_profile_invalid = true;
            }
        }
        if cfg.blocks.len() != 1
            || !cfg.edges.is_empty()
            || checker.map_root_profile_invalid
            || !checker.live_map_root_locals.is_empty()
            || !checker.permitted_map_root_loads.is_empty()
        {
            checker.map_root_actions.clear();
        }

        let mut actions = checker
            .map_root_actions
            .into_iter()
            .map(|action| MapAnyOwnerAction {
                profile_version: MapAnyOwnerProfileVersion::V1,
                kind: match action.kind {
                    MapRootActionKind::New => MapAnyOwnerActionKind::New,
                    MapRootActionKind::Set => MapAnyOwnerActionKind::Set,
                    MapRootActionKind::Remove => MapAnyOwnerActionKind::Remove,
                    MapRootActionKind::Size => MapAnyOwnerActionKind::Size,
                    MapRootActionKind::Contains => MapAnyOwnerActionKind::Contains,
                    MapRootActionKind::Drop => MapAnyOwnerActionKind::Drop,
                },
                point: action.point,
                local: action.local.clone(),
                root: Self::map_any_owner_resource_identity(&action.local),
                key: action.key,
                key_expression: action.key_expression,
                value: action.value,
                value_type: action.value_type,
                source_local: action.source_local.clone(),
                source: action
                    .source_local
                    .as_ref()
                    .map(Self::map_any_owner_resource_identity),
            })
            .collect::<Vec<_>>();

        if !Self::map_any_owner_v1_ledger_is_closed(cfg, types, &actions, &attempts) {
            actions.clear();
        }
        (actions, attempts)
    }

    fn map_any_owner_v1_ledger_is_closed(
        cfg: &CallableCfg,
        types: &ResolvedTypeTable,
        actions: &[MapAnyOwnerAction],
        attempts: &BTreeSet<NodeId>,
    ) -> bool {
        if actions.is_empty() || cfg.blocks.len() != 1 || !cfg.edges.is_empty() {
            return false;
        }
        let Some(block) = cfg.blocks.values().next() else {
            return false;
        };
        let point_order = block
            .points
            .iter()
            .enumerate()
            .map(|(index, point)| (point.source.node.clone(), index))
            .collect::<BTreeMap<_, _>>();
        if actions
            .iter()
            .any(|action| !point_order.contains_key(&action.point))
        {
            return false;
        }
        let new_points = actions
            .iter()
            .filter(|action| action.kind == MapAnyOwnerActionKind::New)
            .map(|action| action.point.clone())
            .collect::<BTreeSet<_>>();
        if new_points.is_empty() || &new_points != attempts {
            return false;
        }

        let mut payload_by_root = BTreeMap::<ResourceId, Option<ResolvedTypeId>>::new();
        let mut live_roots = BTreeSet::<ResourceId>::new();
        let mut previous_position = None;
        for action in actions {
            if action.profile_version != MapAnyOwnerProfileVersion::V1
                || action.root != Self::map_any_owner_resource_identity(&action.local)
            {
                return false;
            }
            let position = point_order[&action.point];
            if previous_position.is_some_and(|previous| position < previous) {
                return false;
            }
            previous_position = Some(position);
            match action.kind {
                MapAnyOwnerActionKind::New => {
                    if action.key.is_some()
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || payload_by_root.contains_key(&action.root)
                        || !live_roots.insert(action.root.clone())
                    {
                        return false;
                    }
                    payload_by_root.insert(action.root.clone(), None);
                }
                MapAnyOwnerActionKind::Set => {
                    let (
                        Some(key),
                        Some(_value),
                        Some(value_type),
                        Some(source_local),
                        Some(source),
                    ) = (
                        &action.key,
                        &action.value,
                        &action.value_type,
                        &action.source_local,
                        &action.source,
                    )
                    else {
                        return false;
                    };
                    if key.contains('\0')
                        || source != &Self::map_any_owner_resource_identity(source_local)
                        || !matches!(
                            types.get(value_type),
                            Some(ResolvedType::Primitive(
                                crate::core::PrimitiveType::I32
                                    | crate::core::PrimitiveType::String
                            ))
                        )
                        || payload_by_root.contains_key(&action.root)
                    {
                        return false;
                    }
                    let Some(inherited) = payload_by_root.remove(source) else {
                        return false;
                    };
                    if !live_roots.remove(source)
                        || inherited
                            .as_ref()
                            .is_some_and(|previous| previous != value_type)
                        || !live_roots.insert(action.root.clone())
                    {
                        return false;
                    }
                    payload_by_root.insert(action.root.clone(), Some(value_type.clone()));
                }
                MapAnyOwnerActionKind::Remove => {
                    let (Some(key), Some(source_local), Some(source)) =
                        (&action.key, &action.source_local, &action.source)
                    else {
                        return false;
                    };
                    if key.contains('\0')
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || source != &Self::map_any_owner_resource_identity(source_local)
                        || payload_by_root.contains_key(&action.root)
                    {
                        return false;
                    }
                    let Some(inherited) = payload_by_root.remove(source) else {
                        return false;
                    };
                    if !live_roots.remove(source) || !live_roots.insert(action.root.clone()) {
                        return false;
                    }
                    payload_by_root.insert(action.root.clone(), inherited);
                }
                MapAnyOwnerActionKind::Size => {
                    if action.key.is_some()
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !live_roots.contains(&action.root)
                    {
                        return false;
                    }
                }
                MapAnyOwnerActionKind::Contains => {
                    if action.key.as_ref().is_none_or(|key| key.contains('\0'))
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !live_roots.contains(&action.root)
                    {
                        return false;
                    }
                }
                MapAnyOwnerActionKind::Drop => {
                    if action.key.is_some()
                        || action.value.is_some()
                        || action.value_type.is_some()
                        || action.source_local.is_some()
                        || action.source.is_some()
                        || !live_roots.remove(&action.root)
                        || payload_by_root.remove(&action.root).is_none()
                    {
                        return false;
                    }
                }
            }
        }
        live_roots.is_empty() && payload_by_root.is_empty()
    }

    fn record_map_new_binding(
        &mut self,
        initializer: &ResolvedExpr,
        target: Option<&ResolvedLocalId>,
    ) {
        let (Some(target), ResolvedExprKind::Call(call)) = (target, &initializer.kind) else {
            return;
        };
        if !matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
            || !call.arguments.is_empty()
            || !self
                .body
                .locals
                .get(target)
                .is_some_and(|local| local.ty == initializer.ty)
            || !matches!(
                self.types.get(&initializer.ty),
                Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
            )
        {
            return;
        }
        if !self.map_root_locals.insert(target.clone())
            || !self.live_map_root_locals.insert(target.clone())
        {
            self.map_root_profile_invalid = true;
            return;
        }
        self.map_root_payload_types.insert(target.clone(), None);
        self.map_root_actions.push(MapRootAction {
            kind: MapRootActionKind::New,
            point: initializer.node_id.clone(),
            local: target.clone(),
            root: Self::map_root_resource_identity(target),
            key: None,
            key_expression: None,
            value: None,
            value_type: None,
            source_local: None,
            source: None,
        });
    }

    fn map_root_temporary_local(point: &NodeId) -> ResolvedLocalId {
        ResolvedLocalId(NodeId(format!("map-root-temporary:{}", point.0)))
    }

    fn receipted_temporary_map_root(
        &mut self,
        expression: &ResolvedExpr,
    ) -> Option<ResolvedLocalId> {
        let ResolvedExprKind::Call(call) = &expression.kind else {
            return None;
        };
        if !matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
            || !call.arguments.is_empty()
            || !matches!(
                self.types.get(&expression.ty),
                Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
            )
        {
            return None;
        }

        let local = Self::map_root_temporary_local(&expression.node_id);
        if !self.map_root_locals.insert(local.clone())
            || !self.live_map_root_locals.insert(local.clone())
        {
            self.map_root_profile_invalid = true;
            return None;
        }
        self.map_root_payload_types.insert(local.clone(), None);
        self.map_root_actions.push(MapRootAction {
            kind: MapRootActionKind::New,
            point: expression.node_id.clone(),
            local: local.clone(),
            root: Self::map_root_resource_identity(&local),
            key: None,
            key_expression: None,
            value: None,
            value_type: None,
            source_local: None,
            source: None,
        });
        Some(local)
    }

    fn prepare_map_root_call(&mut self, expression: &ResolvedExpr, call: &ResolvedCall) {
        let builtin = match &call.callee {
            ResolvedCallee::Builtin(builtin) => builtin.as_str(),
            _ => "",
        };
        if builtin == "map_size" && call.arguments.len() == 1 {
            let argument = &call.arguments[0].value;
            let local = match &argument.kind {
                ResolvedExprKind::Load(place)
                    if place.projections.is_empty()
                        && self.live_map_root_locals.contains(&place.base) =>
                {
                    self.permitted_map_root_loads
                        .insert(argument.node_id.clone());
                    place.base.clone()
                }
                ResolvedExprKind::Call(_) => {
                    let Some(local) = self.receipted_temporary_map_root(argument) else {
                        return;
                    };
                    if !matches!(
                        self.types.get(&expression.ty),
                        Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
                    ) {
                        self.map_root_profile_invalid = true;
                        return;
                    }
                    self.live_map_root_locals.remove(&local);
                    self.map_root_payload_types.remove(&local);
                    local
                }
                _ => return,
            };
            if matches!(
                self.types.get(&expression.ty),
                Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
            ) {
                self.map_root_actions.push(MapRootAction {
                    kind: MapRootActionKind::Size,
                    point: expression.node_id.clone(),
                    local: local.clone(),
                    root: Self::map_root_resource_identity(&local),
                    key: None,
                    key_expression: None,
                    value: None,
                    value_type: None,
                    source_local: None,
                    source: None,
                });
                if matches!(&argument.kind, ResolvedExprKind::Call(_)) {
                    self.map_root_actions.push(MapRootAction {
                        kind: MapRootActionKind::Drop,
                        point: expression.node_id.clone(),
                        local: local.clone(),
                        root: Self::map_root_resource_identity(&local),
                        key: None,
                        key_expression: None,
                        value: None,
                        value_type: None,
                        source_local: None,
                        source: None,
                    });
                }
            } else if self.map_root_locals.contains(&local) {
                self.map_root_profile_invalid = true;
            }
            return;
        }

        if builtin == "has_key" && call.arguments.len() == 2 {
            let source = &call.arguments[0].value;
            let key_expr = &call.arguments[1].value;
            let ResolvedExprKind::Literal(ResolvedLiteral::String(key)) = &key_expr.kind else {
                if let ResolvedExprKind::Load(place) = &source.kind {
                    if self.map_root_locals.contains(&place.base) {
                        self.map_root_profile_invalid = true;
                    }
                }
                return;
            };
            if key.contains('\0') {
                self.map_root_profile_invalid = true;
                return;
            }
            let local = match &source.kind {
                ResolvedExprKind::Load(place)
                    if place.projections.is_empty()
                        && self.live_map_root_locals.contains(&place.base) =>
                {
                    place.base.clone()
                }
                ResolvedExprKind::Load(place) if self.map_root_locals.contains(&place.base) => {
                    self.map_root_profile_invalid = true;
                    return;
                }
                _ => return,
            };
            if !matches!(
                self.types.get(&source.ty),
                Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
            ) || !matches!(
                self.types.get(&key_expr.ty),
                Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
            ) || !matches!(
                self.types.get(&expression.ty),
                Some(ResolvedType::Primitive(crate::core::PrimitiveType::Bool))
            ) {
                self.map_root_profile_invalid = true;
                return;
            }
            self.permitted_map_root_loads.insert(source.node_id.clone());
            self.map_root_actions.push(MapRootAction {
                kind: MapRootActionKind::Contains,
                point: expression.node_id.clone(),
                local: local.clone(),
                root: Self::map_root_resource_identity(&local),
                key: Some(key.clone()),
                key_expression: Some(key_expr.node_id.clone()),
                value: None,
                value_type: None,
                source_local: None,
                source: None,
            });
            return;
        }

        if builtin == "map_remove" {
            let Some((initializer, target)) = self.direct_map_root_binding.clone() else {
                return;
            };
            if initializer != expression.node_id || call.arguments.len() != 2 {
                return;
            }
            let source = &call.arguments[0].value;
            let key_expr = &call.arguments[1].value;
            let ResolvedExprKind::Literal(ResolvedLiteral::String(key)) = &key_expr.kind else {
                if let ResolvedExprKind::Load(place) = &source.kind {
                    if self.map_root_locals.contains(&place.base) {
                        self.map_root_profile_invalid = true;
                    }
                }
                return;
            };
            if key.contains('\0') {
                self.map_root_profile_invalid = true;
                return;
            }
            let (source_local, source_is_temporary) = match &source.kind {
                ResolvedExprKind::Load(place)
                    if place.projections.is_empty()
                        && self.live_map_root_locals.contains(&place.base) =>
                {
                    (place.base.clone(), false)
                }
                ResolvedExprKind::Load(place) if self.map_root_locals.contains(&place.base) => {
                    // A previously consumed MapRoot local, or a projection
                    // from one, cannot enter this single-owner profile. The
                    // general expression visitor also catches this, but
                    // poison it here so the Remove recognizer remains closed
                    // when its traversal context changes.
                    self.map_root_profile_invalid = true;
                    return;
                }
                ResolvedExprKind::Call(_) => {
                    let Some(local) = self.receipted_temporary_map_root(source) else {
                        return;
                    };
                    (local, true)
                }
                _ => return,
            };
            if self.map_root_locals.contains(&target)
                || !self
                    .body
                    .locals
                    .get(&target)
                    .is_some_and(|local| local.ty == expression.ty)
                || !matches!(
                    self.types.get(&source.ty),
                    Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
                )
                || !matches!(
                    self.types.get(&key_expr.ty),
                    Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
                )
                || !matches!(
                    self.types.get(&expression.ty),
                    Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
                )
            {
                if self.map_root_locals.contains(&source_local) {
                    self.map_root_profile_invalid = true;
                }
                return;
            }

            if !source_is_temporary {
                let ResolvedExprKind::Load(place) = &source.kind else {
                    unreachable!("non-temporary MapRoot source is a local load")
                };
                self.permitted_map_root_loads.insert(source.node_id.clone());
                debug_assert_eq!(place.base, source_local);
            }
            let inherited_payload_type = self
                .map_root_payload_types
                .get(&source_local)
                .cloned()
                .flatten();
            self.live_map_root_locals.remove(&source_local);
            self.map_root_payload_types.remove(&source_local);
            self.map_root_locals.insert(target.clone());
            self.live_map_root_locals.insert(target.clone());
            self.map_root_payload_types
                .insert(target.clone(), inherited_payload_type);
            self.map_root_actions.push(MapRootAction {
                kind: MapRootActionKind::Remove,
                point: expression.node_id.clone(),
                local: target.clone(),
                root: Self::map_root_resource_identity(&target),
                key: Some(key.clone()),
                key_expression: Some(key_expr.node_id.clone()),
                value: None,
                value_type: None,
                source_local: Some(source_local.clone()),
                source: Some(Self::map_root_resource_identity(&source_local)),
            });
            return;
        }

        if builtin != "map_set" {
            return;
        }

        let Some((initializer, target)) = self.direct_map_root_binding.clone() else {
            return;
        };
        if initializer != expression.node_id || call.arguments.len() != 3 {
            return;
        }
        let source = &call.arguments[0].value;
        let ResolvedExprKind::Literal(ResolvedLiteral::String(key)) = &call.arguments[1].value.kind
        else {
            if let ResolvedExprKind::Load(place) = &source.kind {
                if self.map_root_locals.contains(&place.base) {
                    self.map_root_profile_invalid = true;
                }
            }
            return;
        };
        if key.contains('\0') {
            self.map_root_profile_invalid = true;
            return;
        }
        let (source_local, source_is_temporary) = match &source.kind {
            ResolvedExprKind::Load(place)
                if place.projections.is_empty()
                    && self.live_map_root_locals.contains(&place.base) =>
            {
                (place.base.clone(), false)
            }
            ResolvedExprKind::Call(_) => {
                let Some(local) = self.receipted_temporary_map_root(source) else {
                    return;
                };
                (local, true)
            }
            _ => return,
        };
        let value = &call.arguments[2].value;
        let string_value = matches!(
            self.types.get(&value.ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
        );
        let value_has_owned_temporary_shape = match &value.kind {
            ResolvedExprKind::Literal(ResolvedLiteral::String(_)) => true,
            ResolvedExprKind::Load(place) => place.projections.is_empty(),
            _ => false,
        };
        if string_value && !value_has_owned_temporary_shape {
            // String values are copied into the type-erased Map owner. This
            // closed profile admits only shapes for which MIR materializes a
            // fresh owned temporary (literal or direct local read), allowing
            // every consumer to discharge that temporary after the copy.
            self.map_root_profile_invalid = true;
            return;
        }
        let inherited_payload_type = self
            .map_root_payload_types
            .get(&source_local)
            .cloned()
            .flatten();
        if inherited_payload_type
            .as_ref()
            .is_some_and(|previous| previous != &value.ty)
        {
            // Keep source-checker receipts absent for a root whose successive
            // Sets would change the admitted payload ABI. The legacy Map
            // contract remains available to this mixed Any payload program.
            self.map_root_profile_invalid = true;
            return;
        }
        if self.map_root_locals.contains(&target)
            || !self
                .body
                .locals
                .get(&target)
                .is_some_and(|local| local.ty == expression.ty)
            || !matches!(
                self.types.get(&source.ty),
                Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
            )
            || !matches!(
                self.types.get(&call.arguments[1].value.ty),
                Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
            )
            || !matches!(
                self.types.get(&call.arguments[2].value.ty),
                Some(ResolvedType::Primitive(
                    crate::core::PrimitiveType::I32 | crate::core::PrimitiveType::String
                ))
            )
            || !matches!(
                self.types.get(&expression.ty),
                Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
            )
        {
            if self.map_root_locals.contains(&source_local) {
                self.map_root_profile_invalid = true;
            }
            return;
        }

        if !source_is_temporary {
            let ResolvedExprKind::Load(place) = &source.kind else {
                unreachable!("non-temporary MapRoot source is a local load")
            };
            self.permitted_map_root_loads.insert(source.node_id.clone());
            debug_assert_eq!(place.base, source_local);
        }
        self.live_map_root_locals.remove(&source_local);
        self.map_root_payload_types.remove(&source_local);
        self.map_root_locals.insert(target.clone());
        self.live_map_root_locals.insert(target.clone());
        self.map_root_payload_types
            .insert(target.clone(), Some(value.ty.clone()));
        self.map_root_actions.push(MapRootAction {
            kind: MapRootActionKind::Set,
            point: expression.node_id.clone(),
            local: target.clone(),
            root: Self::map_root_resource_identity(&target),
            key: Some(key.clone()),
            key_expression: Some(call.arguments[1].value.node_id.clone()),
            value: Some(call.arguments[2].value.node_id.clone()),
            value_type: Some(value.ty.clone()),
            source_local: Some(source_local.clone()),
            source: Some(Self::map_root_resource_identity(&source_local)),
        });
    }

    /// v0.34.8 (SD-1 tail): check whether a resolved type is a flow state.
    /// Delegates to `NominalTypeId::nominal_is_flow_state` (types.rs — the
    /// single source of truth) instead of repeating the "state:" prefix
    /// string match. Note: intentionally NOT `is_linear` — SessionChan is
    /// linear but is not a flow state and is not auto-droppable.
    fn is_flow_state_resolved(&self, ty: &ResolvedType) -> bool {
        match ty {
            ResolvedType::FlowStateSet { .. } => true,
            ResolvedType::Nominal { item, .. } => item.nominal_is_flow_state(),
            _ => false,
        }
    }

    /// P0-5: check whether a linear type is auto-droppable at scope exit.
    /// Flow states are droppable; containers (Option/Result/Tuple/Array/Slice)
    /// are droppable iff every linear element inside them is also droppable.
    /// Cap/SessionChan are NOT droppable — they require explicit consumption.
    fn is_droppable_type(&self, ty: &ResolvedTypeId) -> bool {
        match self.types.get(ty) {
            Some(ResolvedType::FlowStateSet { .. }) => true,
            // H2 (audit-type 2026-08-03): builtin container nominals follow
            // the same rule as structural containers — droppable iff every
            // linear element is droppable (e.g. List<flow state> yes,
            // List<cap> no). Keep before the generic Nominal arm.
            Some(ResolvedType::Nominal {
                item, arguments, ..
            }) if matches!(
                item.as_str(),
                "builtin:type:List" | "builtin:type:Map" | "builtin:type:Set"
            ) =>
            {
                arguments
                    .iter()
                    .all(|arg| !self.is_linear(arg) || self.is_droppable_type(arg))
            }
            Some(resolved @ ResolvedType::Nominal { .. }) => self.is_flow_state_resolved(resolved),
            Some(ResolvedType::Option(inner)) => self.is_droppable_type(inner),
            Some(ResolvedType::Result { ok, error }) => {
                // Both branches must be droppable for the whole Result to be.
                (!self.is_linear(ok) || self.is_droppable_type(ok))
                    && (!self.is_linear(error) || self.is_droppable_type(error))
            }
            Some(ResolvedType::Tuple(elements)) => elements
                .iter()
                .all(|e| !self.is_linear(e) || self.is_droppable_type(e)),
            Some(ResolvedType::Array { element, .. }) => self.is_droppable_type(element),
            Some(ResolvedType::Slice(inner)) => self.is_droppable_type(inner),
            Some(ResolvedType::Newtype { inner, .. }) => self.is_droppable_type(inner),
            // Cap, SessionChan, and other non-flow-state linear types are NOT droppable.
            _ => false,
        }
    }

    fn reject_linear_callable_captures(&mut self) {
        for capture in &self.body.captures {
            let Some(local) = self.body.locals.get(capture) else {
                continue;
            };
            if self.is_linear(&local.ty) {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0304,
                        format!(
                            "linear resource '{}' is not owned by the current callable",
                            local.display_name
                        ),
                        local.origin.user_span(),
                    )
                    .with_help(
                        "pass the resource as an explicit parameter or transfer it into a closure",
                    ),
                );
            }
        }
    }

    fn build_resource_catalog(&mut self) {
        // 0.31.13 追加 A: transition `self` (first parameter) is implicitly
        // consumed by the transition mechanism — the source state is transformed
        // into the target state. Don't track it as a linear resource that must
        // be explicitly consumed in the body.
        let is_transition = self.signature.owner.0.starts_with("transition:");
        for (idx, parameter) in self.signature.parameters.iter().enumerate() {
            if is_transition && idx == 0 {
                continue;
            }
            if !self.is_linear(&parameter.ty) {
                continue;
            }
            let local = ResolvedLocalId(NodeId(format!("{}/local", parameter.id.0 .0)));
            if self.body.locals.contains_key(&local) {
                self.resources
                    .insert(local.clone(), vec![ResourceId(local.0.clone())]);
            }
        }
        self.catalog_block(&self.body.root);
    }

    fn catalog_block(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    self.catalog_pattern(pattern, initializer.as_ref());
                    if let Some(initializer) = initializer {
                        self.catalog_expr(initializer);
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    self.catalog_expr(condition);
                    self.catalog_block(body);
                }
                ResolvedStmtKind::WhileLet {
                    pattern,
                    initializer,
                    body,
                }
                | ResolvedStmtKind::For {
                    pattern,
                    iterable: initializer,
                    body,
                } => {
                    self.catalog_pattern(pattern, None);
                    self.catalog_expr(initializer);
                    self.catalog_block(body);
                }
                ResolvedStmtKind::IfLet {
                    pattern,
                    initializer,
                    then_block,
                    else_block,
                } => {
                    self.catalog_pattern(pattern, None);
                    self.catalog_expr(initializer);
                    self.catalog_block(then_block);
                    if let Some(else_block) = else_block {
                        self.catalog_block(else_block);
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    self.catalog_block(body);
                }
                ResolvedStmtKind::Pinned {
                    value,
                    binding,
                    body,
                } => {
                    self.catalog_expr(value);
                    if let Some(binding) = binding {
                        if self
                            .body
                            .locals
                            .get(binding)
                            .is_some_and(|local| self.is_linear(&local.ty))
                        {
                            // Audit 2026-08-05 (wave-1 fix 6): mirror the
                            // Bind/split catalog rule — when the pinned value
                            // is a single linear place, the binding takes over
                            // that place's resource identity so a later
                            // Drop/Move on the binding resolves the same fact
                            // the Move retargeted (P1-10 alignment).
                            let expanded = self.expand_sources(&self.capability_places(value));
                            if expanded.len() == 1 {
                                let resource = expanded.into_iter().next().expect("len == 1");
                                self.resources
                                    .entry(binding.clone())
                                    .or_insert(vec![resource]);
                            } else {
                                self.resources
                                    .entry(binding.clone())
                                    .or_insert_with(|| vec![ResourceId(binding.0.clone())]);
                            }
                        }
                    }
                    self.catalog_block(body);
                }
                ResolvedStmtKind::Assign { value, .. }
                | ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => self.catalog_expr(value),
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.catalog_expr(value);
                    }
                }
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.catalog_expr(expression);
                    }
                }
                ResolvedStmtKind::Continue
                | ResolvedStmtKind::Drop(_)
                | ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            self.catalog_expr(result);
        }
    }

    fn catalog_pattern(&mut self, pattern: &ResolvedPattern, initializer: Option<&ResolvedExpr>) {
        // 0.36.46 定向头提取：`let c = xs[0]`——绑定认领一个元素（fresh 身份），
        // 容器保留余部义务（自身身份不动）。目录在此也跳过继承，否则 c 会拿到
        // xs 的资源身份（幻影），稍后的 drop(xs) 撞 double-consume。
        let sources = if self.is_directional_head_binding(initializer) {
            Vec::new()
        } else {
            initializer
                .map(|value| self.capability_places(value))
                .unwrap_or_default()
        };
        let expanded = self.expand_sources(&sources);
        let mut bindings = Vec::new();
        self.linear_bindings(pattern, &mut bindings);
        // Wave-2 (audit C-2/G-1/G-2 + review §1.3): the catalog mirrors the
        // Move/Introduce actions visit_stmt emits so later Drop/Move resolve
        // the exact ResourceIds the dataflow facts are keyed by.
        //
        // * split() shape — a single-element Tuple([receiver]) with one
        //   linear source and >=2 bindings: the first binding inherits the
        //   receiver's resource (visit_stmt Moves receiver → binding₀); the
        //   remaining bindings are the split-out atoms and are cataloged as
        //   fresh introductions (P1-10 capability decision).
        // * equal counts — positional pairing, one resource per binding.
        // * one binding, several resources — an aggregate merge such as
        //   `let x = (a, b)`: the single binding inherits EVERY source
        //   resource (visit_stmt Moves each into it), so a later `drop(x)`
        //   expands into one Drop per owned resource.
        // * anything else — first binding takes the first resource, the rest
        //   are fresh; visit_stmt fail-closes the mismatch (E0304) whenever
        //   obligations would be stranded or mispaired.
        let split_shape = sources.len() == 1
            && bindings.len() >= 2
            && matches!(initializer, Some(value)
                if matches!(&value.kind, ResolvedExprKind::Tuple(values) if values.len() == 1));
        if split_shape {
            let mut bindings = bindings.into_iter();
            if let Some(first) = bindings.next() {
                let resource = expanded
                    .first()
                    .cloned()
                    .unwrap_or_else(|| self.resource_for_place(&sources[0]));
                self.resources.insert(first, vec![resource]);
            }
            for local in bindings {
                self.resources
                    .insert(local.clone(), vec![ResourceId(local.0.clone())]);
            }
        } else if !expanded.is_empty() && expanded.len() == bindings.len() {
            for (index, local) in bindings.into_iter().enumerate() {
                self.resources.insert(local, vec![expanded[index].clone()]);
            }
        } else if self.single_binding(pattern).is_some() && expanded.len() > 1 {
            let binding = bindings.into_iter().next().expect("bindings.len() == 1");
            self.resources.insert(binding, expanded);
        } else if let Some(resource) = expanded.first() {
            let mut bindings = bindings.into_iter();
            if let Some(first) = bindings.next() {
                self.resources.insert(first, vec![resource.clone()]);
            }
            for local in bindings {
                self.resources
                    .insert(local.clone(), vec![ResourceId(local.0.clone())]);
            }
        } else {
            for local in bindings {
                self.resources
                    .insert(local.clone(), vec![ResourceId(local.0.clone())]);
            }
        }
    }

    fn catalog_expr(&mut self, expression: &ResolvedExpr) {
        match &expression.kind {
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.catalog_expr(condition);
                self.catalog_block(then_block);
                self.catalog_block(else_block);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.catalog_expr(scrutinee);
                for arm in arms {
                    self.catalog_pattern(&arm.pattern, None);
                    if let Some(guard) = &arm.guard {
                        self.catalog_expr(guard);
                    }
                    self.catalog_expr(&arm.body);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.catalog_block(block),
            ResolvedExprKind::Comprehension {
                pattern,
                value,
                iterable,
                guard,
            } => {
                self.catalog_pattern(pattern, None);
                self.catalog_expr(iterable);
                if let Some(guard) = guard {
                    self.catalog_expr(guard);
                }
                self.catalog_expr(value);
            }
            _ => self.for_each_expr_child(expression, |this, child| this.catalog_expr(child)),
        }
    }

    fn introduce_parameters(&mut self) {
        let entry = self.entry_location();
        // 0.31.13 追加 A: transition `self` (first parameter) is implicitly
        // consumed by the transition mechanism — skip Introduce action.
        let is_transition = self.signature.owner.0.starts_with("transition:");
        for (idx, parameter) in self.signature.parameters.iter().enumerate() {
            if is_transition && idx == 0 {
                continue;
            }
            if !self.is_linear(&parameter.ty) {
                continue;
            }
            let local = ResolvedLocalId(NodeId(format!("{}/local", parameter.id.0 .0)));
            let Some(declaration) = self.body.locals.get(&local) else {
                self.errors.push(Diagnostic::error(
                    format!(
                        "linear parameter '{}' has no ResolvedBody local",
                        parameter.name
                    ),
                    self.body.root.origin.user_span(),
                ));
                continue;
            };
            let place = self.place_from_local(&local);
            self.actions.push(CanonicalResourceAction {
                kind: CanonicalActionKind::Introduce,
                resource: self.resource_for_local(&local),
                source: Some(place.clone()),
                target: Some(place),
                loan: None,
                location: entry.clone(),
                span: declaration.origin.user_span(),
                origin: declaration.origin.clone(),
            });
        }
    }

    /// 0.36.45: then 块内首个线性消费的最内层节点——CFG 点序把内层表达式
    /// 点排在外层语句点之前，Introduce 键到语句节点会在首个消费之后应用
    /// （顺序翻转）；键到消费自身所在的点 + 动作秩排序（Introduce=3 <
    /// Move=5）保证先复位后消费。二元/累加形状从右侧下探（`n = n + f(x)`
    /// 的消费在右），构造/调用形状直接落在调用节点。
    fn linear_action_node(&self, expr: &ResolvedExpr) -> (NodeId, crate::core::Origin) {
        let mut current = expr;
        loop {
            match &current.kind {
                ResolvedExprKind::Call(call) => {
                    // 调用实参链下钻：线性消费发生在最内层实参（
                    // `println(sink(x))` 里 x 的 Move 键在 sink 调用节点，
                    // 不在 println 外层）——点序内层在前，Introduce 必须 ≤
                    // 首个消费点；Nominal 通常只走首实参（接收者/首个值），
                    // 首实参及其嵌套链已覆盖标准形状。
                    if let Some(first) = call.arguments.first().map(|a| &a.value) {
                        current = first;
                        continue;
                    }
                    return (current.node_id.clone(), current.origin.clone());
                }
                ResolvedExprKind::Load(_) => {
                    return (current.node_id.clone(), current.origin.clone());
                }
                ResolvedExprKind::Binary { right, .. } => current = right,
                ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                    // 臂体/块体常被 block 包裹：下钻首语句的表达式
                    //（点序内层在前，块节点点 = 块出口，落在其后会顺序翻转）。
                    let head: Option<&ResolvedExpr> = block
                        .statements
                        .first()
                        .and_then(|s| match &s.kind {
                            ResolvedStmtKind::Expr(e) => Some(e),
                            ResolvedStmtKind::Return { value: Some(e), .. } => Some(e),
                            ResolvedStmtKind::Bind {
                                initializer: Some(e),
                                ..
                            } => Some(e),
                            ResolvedStmtKind::Assign { value, .. } => Some(value),
                            _ => None,
                        })
                        .or_else(|| block.result.as_ref().map(|v| &**v));
                    match head {
                        Some(e) => current = e,
                        None => {
                            // 首语句是 Drop 等非表达式语句：其动作键在语句节点
                            // 本身——以语句节点为 Introduce 锚（同点 + 动作秩
                            // Introduce=3 < Drop=5 保证先复位后消费）。
                            if let Some(st) = block.statements.first() {
                                return (st.node_id.clone(), st.origin.clone());
                            }
                            return (current.node_id.clone(), current.origin.clone());
                        }
                    }
                }
                _ => return (current.node_id.clone(), current.origin.clone()),
            }
        }
    }

    fn visit_block(&mut self, block: &ResolvedBlock, return_result: bool) {
        // v0.34.8 (golden §6.2): reset the double-consume tracker at each
        // block boundary — a resource consumed in one branch is legitimately
        // consumed in a sibling branch. The assertion now catches only a
        // genuine second consumption within one straight-line block.
        self.consumed_resources.clear();
        for statement in &block.statements {
            self.visit_stmt(statement);
            // Audit 2026-08-05 (wave-2, H-6): an anonymous temporary borrow
            // created anywhere in this statement (`inc(&mut x)`, including
            // nested call arguments) ends at the statement boundary — mirror
            // of named-borrow NLL. The flush resolves against the statement's
            // terminating CFG point; statements without their own point keep
            // the loans pending until the next flush site.
            self.flush_pending_loans(&statement.node_id, &statement.origin);
        }
        if let Some(result) = &block.result {
            self.visit_expr(result, None);
            if return_result {
                // Audit 2026-08-05 (wave-2, C-2): a function whose RESULT is
                // a branch expression over distinct linear resources leaks
                // every arm not taken at runtime — reject like every other
                // consumption position.
                if let Some(distinct) = self.xor_branch_violation(result) {
                    self.push_xor_diagnostic(&distinct, &result.origin);
                } else {
                    self.emit_consumes(
                        CanonicalActionKind::Return,
                        self.capability_places(result),
                        &result.node_id,
                        &result.origin,
                    );
                }
            }
        }
    }

    fn visit_stmt(&mut self, statement: &ResolvedStmt) {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                if let Some(initializer) = initializer {
                    let reference = self.single_binding(pattern);
                    self.last_visit_rejected = false;
                    let direct_map_root_binding = reference.as_ref().and_then(|local| {
                        matches!(&initializer.kind, ResolvedExprKind::Call(_))
                            .then(|| (initializer.node_id.clone(), local.clone()))
                    });
                    let previous_map_root_binding = std::mem::replace(
                        &mut self.direct_map_root_binding,
                        direct_map_root_binding,
                    );
                    self.record_map_new_binding(initializer, reference.as_ref());
                    self.in_bind_initializer = true;
                    self.visit_expr(initializer, reference.as_ref());
                    self.in_bind_initializer = false;
                    self.direct_map_root_binding = previous_map_root_binding;
                    // 0.36.46: 定向头提取 `let c = xs[0]`——c 认领一个元素义务
                    //（fresh Introduce），容器保留余部义务（既有 fact 不动；
                    // 后续 drop(xs) = 释放余部；不触 xs → 返回门禁 E0256）。
                    // 与普通配对不同：绝不把容器整体 Move 进 c。
                    if let Some(base) = self.directional_extraction_base.take() {
                        let mut bindings = Vec::new();
                        self.linear_bindings(pattern, &mut bindings);
                        if bindings.len() != 1 {
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0304,
                                    format!(
                                        "head extraction `{}[0]` binds {} linear name(s);                                          exactly one element is released",
                                        self.local_name(&base),
                                        bindings.len()
                                    ),
                                    statement.origin.user_span(),
                                )
                                .with_help(&format!(
                                    "bind the extracted element with a single name (`let c = {}[0]`)",
                                    self.local_name(&base)
                                )),
                            );
                            return;
                        }
                        let target = self.place_from_local(&bindings[0]);
                        self.push_action(
                            &statement.node_id,
                            &statement.origin,
                            ActionDraft {
                                kind: CanonicalActionKind::Introduce,
                                resource: self.resource_for_local(&bindings[0]),
                                source: Some(target.clone()),
                                target: Some(target),
                                loan: None,
                            },
                        );
                        return;
                    }
                    // 0.36.43: an E0304-rejected extraction (v[0] — index/slice/
                    // tuple projection of a non-droppable linear container) must
                    // not pair its place into the binding — the extracted value
                    // never moved, and fabricating the Move made x own the
                    // container's resource, so a later legitimate drop(v) hit
                    // the RESOURCE-LINEAR-001 double-drop signal. The E0304 is
                    // the whole story; skip the pairing entirely.
                    if self.last_visit_rejected {
                        // The binding was never established (the E0304 stands
                        // alone); a preceding materialization already attached
                        // the container's resource identity to the binding's
                        // local, so without this a later `drop(x)` would
                        // discharge the container's identity and the next
                        // `drop(v)` would trip RESOURCE-LINEAR-001. Clear the
                        // phantom ownership.
                        let mut bindings = Vec::new();
                        self.linear_bindings(pattern, &mut bindings);
                        for binding in bindings {
                            self.resources.remove(&binding);
                        }
                        return;
                    }
                    // Audit 2026-08-05 (wave-2, C-2/G-2): If/Match are XOR —
                    // exactly one arm's value flows at runtime. Consuming a
                    // branch expression that carries SEVERAL distinct linear
                    // resources (into a binding here, into a call/return/
                    // break below) can discharge at most one obligation; the
                    // rest leak on every path that does not take their arm.
                    // Fail-closed with E0840 at every consumption point so
                    // the rule is position-invariant. Design call: a single
                    // binding is one consumer; XOR of distinct resources
                    // needs one consumer per resource, which straight-line
                    // ownership facts cannot grant (the surviving resource
                    // differs per path). A branch duplicating ONE place
                    // (`if f { t } else { t }`) stays legal — one distinct
                    // resource, one obligation.
                    if let Some(distinct) = self.xor_branch_violation(initializer) {
                        self.push_xor_diagnostic(&distinct, &statement.origin);
                        return;
                    }
                    let sources = self.capability_places(initializer);
                    let pairs = self.expand_source_pairs(&sources);
                    let mut bindings = Vec::new();
                    self.linear_bindings(pattern, &mut bindings);
                    // Audit 2026-08-05 (wave-1 fix 1, extended wave-2 review
                    // §1.3): positional pairing is only sound when every
                    // linear source is matched by a linear binding. Wildcards
                    // and length mismatches mispair or strand sources:
                    // `let (_, y) = (a, b)` would Move a → y while untouched
                    // b stays Available → `drop(b); drop(y)` accepted
                    // (verified use-after-move + silent leak of a).
                    // Fail-closed: reject every mismatch EXCEPT:
                    // * one binding receiving several resources — a legal
                    //   aggregate merge (`let x = (a, b)`); the binding
                    //   inherits every resource and a later drop expands to
                    //   all of them;
                    // * the sanctioned split() lowering — a single-element
                    //   Tuple([receiver]) with one source and >=2 bindings —
                    //   but ONLY when the pattern binds every position: a
                    //   wildcard in a split pattern silently discards a
                    //   capability atom (review §1.3 fifth hole:
                    //   `let (_, w) = c.split()` checked green while the
                    //   read atom leaked), so any split shape with a wildcard
                    //   is rejected;
                    // * sources.len() == 0 — the initializer contributes no
                    //   linear place (e.g. a call result), so every binding
                    //   is a fresh introduction and nothing can be mispaired.
                    // RED LINE (wave1-review §1.3, verified PoC): the split
                    // shape is identified by the pattern's SLOT count, not
                    // the surviving linear-binding count. Counting bindings
                    // lets `_` eat one slot: `(_, w)` compresses to one
                    // binding, balances the single source 1==1 in the generic
                    // pairing arm below, and the discarded read atom escapes
                    // with zero obligation (`mimi check` returned Ok). The
                    // slot count stays 2, so the wildcard rejection below
                    // cannot be vacated by binding compression.
                    let split_shape = sources.len() == 1
                        && matches!(&initializer.kind, ResolvedExprKind::Tuple(values) if values.len() == 1)
                        && self.pattern_slot_count(pattern) >= 2;
                    if split_shape && self.pattern_has_wildcard(pattern) {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0304,
                                "split() capability atoms cannot be discarded with `_`: every atom \
                                     of a split must be bound and consumed explicitly"
                                    .to_string(),
                                statement.origin.user_span(),
                            )
                            .with_help(
                                "bind every split atom (e.g. `let (r, w) = c.split()`) and \
                                     consume each one; `_` silently leaks the unbound atom",
                            ),
                        );
                        return;
                    }
                    if !split_shape
                        && !pairs.is_empty()
                        && pairs.len() != bindings.len()
                        && self.single_binding(pattern).is_none()
                    {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0304,
                                format!(
                                    "destructuring consumes linear values ambiguously: \
                                     {} linear source(s) cannot be paired positionally with {} linear binding(s)",
                                    pairs.len(),
                                    bindings.len()
                                ),
                                statement.origin.user_span(),
                            )
                            .with_help(
                                "bind every linear element explicitly; wildcard `_` positions and \
                                 shortened patterns strand or mispair linear sources \
                                 (only `split()` tuples and single-binding aggregates are exempt)",
                            ),
                        );
                        return;
                    }
                    if split_shape {
                        // Sanctioned split() lowering: binding₀ MOVES the
                        // receiver (consuming the receiver's fact); the rest
                        // are fresh atom introductions.
                        let mut bindings = bindings.into_iter();
                        if let (Some(first), Some(source)) = (bindings.next(), sources.first()) {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource: self.resource_for_place(source),
                                    source: Some(source.clone()),
                                    target: Some(self.place_from_local(&first)),
                                    loan: None,
                                },
                            );
                        }
                        for local in bindings {
                            let target = self.place_from_local(&local);
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Introduce,
                                    resource: self.resource_for_local(&local),
                                    source: Some(target.clone()),
                                    target: Some(target),
                                    loan: None,
                                },
                            );
                        }
                    } else if !pairs.is_empty() && pairs.len() == bindings.len() {
                        for (binding, (resource, source)) in
                            bindings.into_iter().zip(pairs.into_iter())
                        {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource,
                                    source: Some(source),
                                    target: Some(self.place_from_local(&binding)),
                                    loan: None,
                                },
                            );
                        }
                    } else if bindings.len() == 1 && !pairs.is_empty() {
                        // Aggregate merge into one binding (`let x = (a,b)`):
                        // every source resource moves into the binding, which
                        // becomes the owner of all of them.
                        let binding = bindings.into_iter().next().expect("bindings.len() == 1");
                        let target = self.place_from_local(&binding);
                        for (resource, source) in pairs {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource,
                                    source: Some(source),
                                    target: Some(target.clone()),
                                    loan: None,
                                },
                            );
                        }
                    } else {
                        for binding in bindings {
                            let target = self.place_from_local(&binding);
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Introduce,
                                    resource: self.resource_for_local(&binding),
                                    source: Some(target.clone()),
                                    target: Some(target),
                                    loan: None,
                                },
                            );
                        }
                    }
                }
            }
            ResolvedStmtKind::Assign { target, value, .. } => {
                if self.map_root_locals.contains(&target.base) {
                    // Rebinding or mutating a root place has no receipt in
                    // this first local profile.
                    self.map_root_profile_invalid = true;
                }
                self.visit_expr(value, None);
                if self.place_is_linear(target) {
                    // Audit 2026-08-05 (wave-2, C-2): assigning a branch
                    // expression with distinct linear resources into one
                    // place is the same XOR violation as consuming it.
                    if let Some(distinct) = self.xor_branch_violation(value) {
                        self.push_xor_diagnostic(&distinct, &statement.origin);
                        return;
                    }
                    let pairs = self.expand_source_pairs(&self.capability_places(value));
                    let target_place = self.canonical_place(target);
                    if pairs.is_empty() {
                        // Audit 2026-08-05 (wave-2, review §5.5): a linear
                        // RESULT (typically a call) assigned into a linear
                        // place establishes a fresh obligation owned by the
                        // target — previously no action was emitted at all,
                        // so `x = make_token()` created no fact and the
                        // obligation vanished.
                        if self.is_linear(&value.ty) && !self.is_droppable_type(&value.ty) {
                            let resource =
                                ResourceId(NodeId(format!("{}/assigned", statement.node_id.0)));
                            if target.projections.is_empty() {
                                self.resources
                                    .insert(target.base.clone(), vec![resource.clone()]);
                            }
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Introduce,
                                    resource,
                                    source: Some(target_place.clone()),
                                    target: Some(target_place),
                                    loan: None,
                                },
                            );
                        }
                    } else {
                        // Audit 2026-08-05 (wave-2, G-1): move EVERY linear
                        // source into the target (the old `.first()` stranded
                        // every element past the first: `x = (c, d)` parked
                        // d → false E0256), then re-establish the target's
                        // resource identity so a later `drop(x)` resolves the
                        // facts the assignment just retargeted. Without the
                        // re-key, `drop(x); x = b; drop(x)` reported the
                        // second drop as a double-consume of x's ORIGINAL
                        // (already consumed) identity — a false E0304.
                        // Re-keying only happens for root targets; projected
                        // targets (`x.field = a`) keep the base's catalog.
                        for (resource, source) in &pairs {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource: resource.clone(),
                                    source: Some(source.clone()),
                                    target: Some(target_place.clone()),
                                    loan: None,
                                },
                            );
                        }
                        if target.projections.is_empty() {
                            self.resources.insert(
                                target.base.clone(),
                                pairs.into_iter().map(|(resource, _)| resource).collect(),
                            );
                        }
                    }
                }
            }
            ResolvedStmtKind::Return { value, .. } => {
                if let Some(value) = value {
                    self.visit_expr(value, None);
                    // Audit 2026-08-05 (wave-2, C-2): `return if/match` with
                    // distinct linear arms leaks every arm that is not taken
                    // at runtime while the analysis consumed all of them.
                    if let Some(distinct) = self.xor_branch_violation(value) {
                        self.push_xor_diagnostic(&distinct, &statement.origin);
                        return;
                    }
                    self.emit_consumes(
                        CanonicalActionKind::Return,
                        self.capability_places(value),
                        &statement.node_id,
                        &statement.origin,
                    );
                }
            }
            ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    self.visit_expr(value, None);
                    // P1-4: break value must emit consume actions, symmetric
                    // with Return. `loop { break token }` must track token
                    // as consumed. Audit 2026-08-05 (wave-2, C-2): with the
                    // same XOR guard — `break if f { a } else { b }` leaks
                    // the arm not taken.
                    if let Some(distinct) = self.xor_branch_violation(value) {
                        self.push_xor_diagnostic(&distinct, &statement.origin);
                        return;
                    }
                    self.emit_consumes(
                        CanonicalActionKind::Move,
                        self.capability_places(value),
                        &statement.node_id,
                        &statement.origin,
                    );
                }
            }
            ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {}
            ResolvedStmtKind::Expr(expression) => {
                self.visit_expr(expression, None);
                // Audit 2026-08-05 (wave-2, review §5.5): statement-style
                // discard of a call returning a linear value used to
                // establish NO obligation — the result was never introduced,
                // so `make_token();` leaked silently (no fact, no E0256).
                // Introduce the discarded result as a fresh obligation at the
                // call site; the return gate then reports it like any other
                // unconsumed resource. Droppable results (flow states) are
                // exempt, mirroring the return gate.
                if let Some(call_expr) = self.discarded_linear_call(expression) {
                    let resource = ResourceId(NodeId(format!("{}/discarded", call_expr.node_id.0)));
                    let place =
                        Place::root(LocalId(resource.0.clone()), "<discarded linear result>");
                    self.push_action(
                        &statement.node_id,
                        &statement.origin,
                        ActionDraft {
                            kind: CanonicalActionKind::Introduce,
                            resource,
                            source: Some(place.clone()),
                            target: Some(place),
                            loan: None,
                        },
                    );
                }
            }
            ResolvedStmtKind::While { condition, body } => {
                self.visit_expr(condition, None);
                self.visit_block(body, false);
            }
            ResolvedStmtKind::WhileLet {
                pattern,
                initializer,
                body,
                ..
            }
            | ResolvedStmtKind::For {
                pattern,
                iterable: initializer,
                body,
                ..
            } => {
                self.visit_expr(initializer, None);
                // 0.36.37: a for/while-let loop over a linear container is an
                // exhaustive, element-wise deconstruction — candidate (1)
                // extended from match/if-let (0.36.36). Per-iteration element
                // Introductions at the pattern Binding point (loop-carried, so
                // the backedge fixed-point pass sees the element FRESH — the
                // body's consumption never trips the E0304 "moved after
                // consumed" artifact), and the container obligation dissolves
                // at the loop statement. Fail-closed guards mirror
                // match/if-let: a stranding wildcard (`for _ in v` over a
                // linear element) and any early exit in the body
                // (`break`/`return` — at runtime the not-yet-iterated
                // elements are abandoned) keep the container obligation
                // unsolved → E0256.
                if let Some(container_place) = self.linear_loop_container(initializer, pattern) {
                    // Container dissolve: ONLY builtin sequence containers
                    // (List/Map/Set) in for-loops are exhaustive element-wise
                    // deconstructions — the VM iterates the container and
                    // every element is visited exactly once (when the body
                    // consumes it; the per-iteration obligation enforces
                    // that). A while-let over Option/Result re-evaluates its
                    // initializer every round and NEVER consumes the
                    // container binding (runtime semantics — `while let
                    // Some(x) = o` re-reads o), so dissolving the container
                    // there would falsely accept a runtime-infinite loop:
                    // the container obligation stays unsolved (E0256).
                    // Early exits (`break`/`return`) also block the dissolve:
                    // at runtime the not-yet-iterated elements are abandoned.
                    if self.linear_sequence_container(&initializer.ty)
                        && !self.block_has_early_exit(body, true)
                    {
                        self.emit_consumes(
                            CanonicalActionKind::Drop,
                            vec![container_place],
                            &statement.node_id,
                            &statement.origin,
                        );
                    }
                    // 0.36.37: per-iteration element obligation — the loop
                    // variable binds a FRESH element every iteration, so the
                    // body must consume each one. Keyed on the pattern
                    // Binding point inside the loop body — loop-carried, so
                    // the Introduce resets the fact before the body runs on
                    // every fixed-point pass and a body consumption
                    // (`sink(x)`) never trips the E0304 backedge
                    // double-consume artifact. A body that skips the
                    // consumption (continue/conditionals/early break) leaves
                    // the element Available at the loop-carried diverging
                    // sink or the return path → E0256. Emitted even when the
                    // container dissolve is blocked (early exits): the
                    // container obligation then stays unsolved (E0256 on the
                    // container — the not-yet-iterated elements leak at
                    // runtime), and the element obligation still reports the
                    // current element independently.
                    let mut bindings = Vec::new();
                    self.linear_bindings(pattern, &mut bindings);
                    for local in bindings {
                        let target = self.place_from_local(&local);
                        self.push_action(
                            &pattern.node_id,
                            &pattern.origin,
                            ActionDraft {
                                kind: CanonicalActionKind::Introduce,
                                resource: self.resource_for_local(&local),
                                source: Some(target.clone()),
                                target: Some(target),
                                loan: None,
                            },
                        );
                    }
                }
                // Audit 2026-08-05 (wave-2, G-4): the for/while-let iterable
                // is evaluated EXACTLY ONCE before the loop (hoisted into the
                // CFG pre-header), so its anonymous temporary borrows end at
                // the loop statement's terminating point — BEFORE the body
                // runs. Without this explicit flush, the body's FIRST
                // statement boundary grabs the iterable's pending loan and
                // ends it at ITS point inside the body, co-located with the
                // first write (which ranks before the BorrowEnd) — a false
                // E0415. Keying on the loop statement's node resolves the
                // pre-header location added by resolved_lower's hoist.
                self.flush_pending_loans(&statement.node_id, &statement.origin);
                self.visit_block(body, false);
            }
            ResolvedStmtKind::IfLet {
                pattern,
                initializer,
                then_block,
                else_block,
                ..
            } => {
                self.visit_expr(initializer, None);
                // 0.36.36: same container-obligation dissolve as the Match arm
                // — `if let Some(x) = o` exhaustively handles o (bind or no
                // payload); a stranding wildcard keeps fail-closed behavior.
                if let Some(container_place) = self.linear_match_container(initializer, &[pattern])
                {
                    // resolved_lower hoists the initializer into the CFG
                    // pre-header, so the STATEMENT node has no CFG point —
                    // key the dissolve on the initializer expression itself.
                    self.emit_consumes(
                        CanonicalActionKind::Drop,
                        vec![container_place],
                        &initializer.node_id,
                        &initializer.origin,
                    );
                }
                // 0.36.45: then 块绑定名 Introduce（for 体内 if-let 的循环
                // 载入修正）。绑定是逐迭代临时资源——每轮循环重新 Introduce
                // 复位 availability；否则上一迭代的 Consumed 事实被背边携带，
                // 下一迭代的 Move 撞 "moved after it was consumed"
                // （基线非循环单遍不受影响）。
                let mut bindings = Vec::new();
                self.linear_bindings(pattern, &mut bindings);
                // 0.36.45: 绑定 Introduce 必须落在 then 块入口（仅 then 路径
                // 执行 + 每迭代复位）——若落在头部分支前，fall-through 也
                // 会看到 Available 的 y → 汇合撞 "consumed on only some
                // paths"；若键到语句节点，CFG 点序把内层表达式点排在前面
                //（stmt 点 = 语句出口），Introduce 会应用在首个消费之后。
                // 键到首语句的表达式/初始化器节点 = 块的入口点——与消费
                // 同点，dataflow 按动作秩排序（Introduce=3 < Move=5）保证
                // 先复位后消费。空 then 块（弃置 y）退化为头部键：两路径
                // 均匀看到 Available → 返回门禁 E0256（弃置即拒绝）。
                let (then_head_node, then_head_origin) = match then_block.statements.first() {
                    Some(stmt) => match stmt.kind {
                        ResolvedStmtKind::Expr(ref e)
                        | ResolvedStmtKind::Return {
                            value: Some(ref e), ..
                        } => self.linear_action_node(e),
                        ResolvedStmtKind::Bind {
                            initializer: Some(ref init),
                            ..
                        } => self.linear_action_node(init),
                        ResolvedStmtKind::Assign { ref value, .. } => {
                            self.linear_action_node(value)
                        }
                        _ => (stmt.node_id.clone(), stmt.origin.clone()),
                    },
                    None => (initializer.node_id.clone(), initializer.origin.clone()),
                };
                for binding in bindings {
                    let target = self.place_from_local(&binding);
                    self.push_action(
                        &then_head_node,
                        &then_head_origin,
                        ActionDraft {
                            kind: CanonicalActionKind::Introduce,
                            resource: self.resource_for_local(&binding),
                            source: Some(target.clone()),
                            target: Some(target),
                            loan: None,
                        },
                    );
                }
                self.visit_block(then_block, false);
                if let Some(else_block) = else_block {
                    self.visit_block(else_block, false);
                }
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                self.visit_block(body, false);
            }
            ResolvedStmtKind::Drop(places) => {
                for place in places {
                    if place.projections.is_empty()
                        && self.map_root_locals.contains(&place.base)
                        && self.live_map_root_locals.remove(&place.base)
                    {
                        let local = place.base.clone();
                        self.map_root_payload_types.remove(&local);
                        self.map_root_actions.push(MapRootAction {
                            kind: MapRootActionKind::Drop,
                            point: statement.node_id.clone(),
                            local: local.clone(),
                            root: Self::map_root_resource_identity(&local),
                            key: None,
                            key_expression: None,
                            value: None,
                            value_type: None,
                            source_local: None,
                            source: None,
                        });
                    } else if self.map_root_locals.contains(&place.base) {
                        self.map_root_profile_invalid = true;
                    }
                }
                // 0.36.43: `drop(v[0])` — the Drop arm carries resolved PLACES
                // (no expression visit), so the M9 reject never ran for it.
                // Dropping one element releases it and leaks every unextracted
                // sibling — the same element-extraction hole, rejected
                // identically (and neutered, so a later `drop(v)` cannot
                // double-consume the container's identity).
                for place in places.iter() {
                    let has_element_projection = place.projections.iter().any(|projection| {
                        matches!(
                            projection,
                            ResolvedProjection::Index { .. } | ResolvedProjection::Tuple { .. }
                        )
                    });
                    if !has_element_projection {
                        continue;
                    }
                    let Some(local) = self.body.locals.get(&place.base) else {
                        continue;
                    };
                    if self.is_linear(&local.ty) && !self.is_droppable_type(&local.ty) {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0304,
                                format!(
                                    "'{}' cannot be dropped by index or slice: element-level                                      extraction from a linear container is not tracked and                                      leaks every unextracted element",
                                    self.local_name(&place.base)
                                ),
                                statement.origin.user_span(),
                            )
                            .with_help(
                                "move or drop the whole container (e.g. drop(v)) instead of                                  indexing or slicing into it",
                            ),
                        );
                        self.rejected_extraction_places
                            .insert(self.canonical_place(place));
                    }
                }
                let places = places
                    .iter()
                    .filter(|place| self.place_is_linear(place))
                    .map(|place| self.canonical_place(place))
                    .filter(|place| !self.rejected_extraction_places.contains(place))
                    .collect::<Vec<_>>();
                for place in places {
                    // Audit 2026-08-05 (wave-2, G-1): dropping an aggregate
                    // owner (`let x = (a, b); drop(x)`) consumes EVERY
                    // resource the place currently owns — one Drop per
                    // identity. Dropping a single-identity place is unchanged.
                    let mut seen = BTreeSet::new();
                    for resource in self.resources_for_place(&place) {
                        if !seen.insert(resource.clone()) {
                            continue;
                        }
                        self.push_action(
                            &statement.node_id,
                            &statement.origin,
                            ActionDraft {
                                kind: CanonicalActionKind::Drop,
                                resource,
                                source: Some(place.clone()),
                                target: None,
                                loan: None,
                            },
                        );
                    }
                }
            }
            ResolvedStmtKind::Contract { condition, .. } => self.visit_expr(condition, None),
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    self.visit_expr(expression, None);
                }
            }
            ResolvedStmtKind::Pinned {
                value,
                binding,
                body,
            } => {
                self.visit_expr(value, None);
                // Audit 2026-08-05 (wave-1 fix 6): a pinned binding of linear
                // type used to get a catalog entry but NO action — the pinned
                // resource was invisible to dataflow and could escape without
                // consumption. Mirror the Bind arm: a single linear source
                // MOVES into the binding (consuming the source's fact), a
                // source-free value INTRODUCES the binding as a fresh
                // obligation, and multiple sources are ambiguous → fail-closed.
                // Not Introduce-only: introducing without consuming the source
                // would double-count (`pinned(cap_val) |p| drop(p)` leaking
                // cap_val) — the option that cannot regress is Move semantics.
                if let Some(binding) = binding {
                    if self
                        .body
                        .locals
                        .get(binding)
                        .is_some_and(|local| self.is_linear(&local.ty))
                    {
                        let target = self.place_from_local(binding);
                        let pairs = self.expand_source_pairs(&self.capability_places(value));
                        if pairs.len() == 1 {
                            let (resource, source) = pairs.into_iter().next().expect("len == 1");
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource,
                                    source: Some(source),
                                    target: Some(target),
                                    loan: None,
                                },
                            );
                        } else if pairs.is_empty() {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Introduce,
                                    resource: self.resource_for_local(binding),
                                    source: Some(target.clone()),
                                    target: Some(target),
                                    loan: None,
                                },
                            );
                        } else {
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0304,
                                    format!(
                                        "pinned binding consumes linear values ambiguously: \
                                         {} linear sources cannot be paired with one binding",
                                        pairs.len()
                                    ),
                                    statement.origin.user_span(),
                                )
                                .with_help(
                                    "pin a single linear place; multiple linear sources cannot \
                                     be positionally assigned to one pinned binding",
                                ),
                            );
                        }
                    }
                }
                self.visit_block(body, false);
            }
        }
    }

    fn visit_expr(
        &mut self,
        expression: &ResolvedExpr,
        borrow_reference: Option<&ResolvedLocalId>,
    ) {
        if matches!(
            &expression.kind,
            ResolvedExprKind::Call(call)
                if matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "map_new")
                    && call.arguments.is_empty()
        ) {
            // Keep every construction visible, including nested/temporary
            // calls. When any lifecycle action is receipted, admission checks
            // the complete callable set so one valid root cannot mask another.
            self.map_root_new_attempts
                .insert(expression.node_id.clone());
        }
        if let ResolvedExprKind::Load(place) = &expression.kind {
            if self.map_root_locals.contains(&place.base)
                && !self.permitted_map_root_loads.remove(&expression.node_id)
            {
                // Aliases, unsupported operations, aggregate escape, and
                // use-after-update/drop withhold the MIR lifecycle receipt.
                self.map_root_profile_invalid = true;
            }
        }
        self.reject_index_read_extraction(expression);
        match &expression.kind {
            ResolvedExprKind::Unary {
                op: ResolvedUnaryOp::BorrowShared | ResolvedUnaryOp::BorrowMutable,
                operand,
            } => {
                self.visit_expr(operand, None);
                let mut source = self.borrow_place(operand).unwrap_or_else(|| {
                    Place::root(
                        LocalId(NodeId(format!("{}/temporary", expression.node_id.0))),
                        "<temporary>",
                    )
                });
                let kind = match &expression.kind {
                    ResolvedExprKind::Unary {
                        op: ResolvedUnaryOp::BorrowMutable,
                        ..
                    } => LoanKind::Mutable,
                    _ => LoanKind::Shared,
                };
                let loan_id = LoanId(NodeId(format!("{}/loan", expression.node_id.0)));
                let parent = source
                    .projections
                    .first()
                    .filter(|projection| **projection == PlaceProjection::Deref)
                    .and_then(|_| {
                        self.loans
                            .iter()
                            .rev()
                            .find(|loan| loan.reference.as_ref() == Some(&source.base))
                            .map(|loan| (loan.id.clone(), loan.place.clone()))
                    });
                let parent_id = parent.as_ref().map(|(id, _)| id.clone());
                if let Some((_, parent_place)) = parent {
                    source = parent_place;
                }
                let reference = borrow_reference.map(|local| LocalId(local.0.clone()));
                let reference_name = borrow_reference.map(|local| self.local_name(local));
                let location = self.location(&expression.node_id, &expression.origin);
                let resource = self.resource_for_place(&source);
                self.loans.push(Loan {
                    id: loan_id.clone(),
                    parent: parent_id,
                    kind,
                    place: source.clone(),
                    reference,
                    reference_name: reference_name.clone(),
                    start: location.clone(),
                    end_edges: Vec::new(),
                    span: expression.origin.user_span(),
                });
                self.actions.push(CanonicalResourceAction {
                    kind: match kind {
                        LoanKind::Shared => CanonicalActionKind::BorrowShared,
                        LoanKind::Mutable => CanonicalActionKind::BorrowMut,
                    },
                    resource: resource.clone(),
                    source: Some(source),
                    target: None,
                    loan: Some(loan_id.clone()),
                    location,
                    span: expression.origin.user_span(),
                    origin: expression.origin.clone(),
                });
                // Audit 2026-08-05 (wave-2, H-6): anonymous borrows have no
                // reference binding whose liveness could end the loan — park
                // them for statement-boundary termination in visit_block.
                if reference_name.is_none() {
                    self.pending_anonymous_loans.push((loan_id, resource));
                }
            }
            ResolvedExprKind::Call(call) => {
                self.prepare_map_root_call(expression, call);
                for argument in &call.arguments {
                    self.visit_expr(&argument.value, None);
                }
                // 0.36.47 容器方法变换面（Phase C"容器方法余面"）：线性接收者的
                // 变换方法（Mutate 借用标记；结果 = List/Map/Set/Tuple 且携带
                // 线性义务）降为消费语义——接收者容器整体转出（Move），义务
                // 转移到结果（`let ys = xs.reverse()`：xs 移入 reverse，ys 携带
                // 元素义务、drop(ys) 结算——与 for 迭代同构；此前 Mutate 借用
                // 不解体容器 → 用户被迫额外 drop(xs) = 不可达语义）。
                // 读取/提取面（len/is_empty/count/find/first/last/find_map——
                // 结果标量/裸元素/Option）保持借用：接收者仍需整体 drop。
                if matches!(call.permission, Some(Permission::Mutate))
                    && self.method_transform_result(call)
                {
                    // 0.36.48：变换面 ALL 线性参数整体转出（不只 receiver）。
                    // 义务守恒：结果容器的义务 = 每个线性实参的义务并集。
                    //   - receiver（xs.reverse()）：xs 移入方法（4u 已开）；
                    //   - 容器参数（xs.concat(ys)：ys 元素义务并入结果 zs，
                    //     用户只 drop(zs)——此前 ys 义务原处 → E0256 死锁）；
                    //   - 线性值参数（xs.remove(v)/xs.intersperse(sep)：元素
                    //     义务进方法，由方法体结算/并入结果恰一次）。
                    // 读/提取面（len/first/find_map——结果标量/裸元素/Option）
                    // 不在此列：参数保持借用（容器义务原处）。
                    for argument in &call.arguments {
                        if let ResolvedExprKind::Load(place) = &argument.value.kind {
                            if self.place_is_linear(place) {
                                let canonical = self.canonical_place(place);
                                self.push_action(
                                    &expression.node_id,
                                    &expression.origin,
                                    ActionDraft {
                                        kind: CanonicalActionKind::Move,
                                        resource: self.resource_for_place(&canonical),
                                        source: Some(canonical.clone()),
                                        target: None,
                                        loan: None,
                                    },
                                );
                            }
                        }
                    }
                }
                // M-ARG-001 (0.1.9, audit-found): for a View/Mutate METHOD call the
                // receiver (self, arguments[0]) is borrowed and must NOT be moved
                // in this general path — but the remaining non-self arguments
                // STILL move into the callee. Previously the whole loop was
                // skipped for View/Mutate, so `r.take(c)` (take: cap param) never
                // consumed `c` at the caller → false E0256. Free calls have
                // permission None/Consume, so `receiver_borrowed` is false and
                // every argument is consumed as before. The Mutate-transform
                // receiver move (0.36.47) is handled in its own special case
                // above; skipping arguments[0] here avoids double-consuming it.
                // For a Mutate-transform call, that special case already moves
                // EVERY linear `Load` argument (receiver + value/container args),
                // so they are skipped here to avoid E0304 double-consume.
                //
                // LEN-READ-001 (0.1.9, audit-found): free read-only metric builtins
                // (`len(fs)` / `is_empty(fs)` / `map_size(m)` / `keys(m)`) borrow
                // their container exactly like the View methods (`fs.len()`), so
                // their linear container argument must NOT be moved here — else
                // `let n = len(fs); drop(fs)` is a false E0304 double-consume.
                let read_metric_builtin = matches!(
                    &call.callee,
                    ResolvedCallee::Builtin(b)
                        if matches!(
                            b.as_str(),
                            "len" | "is_empty" | "map_size" | "keys"
                        )
                );
                // Phase E (0.39.81): MutexGuard 恰一次 unlock——mutex_get/mutex_set
                // BORROW 线性 guard（arg 0 不消费：读取/写入后仍需解锁）；只有
                // mutex_unlock 消费 guard（双重解锁 E0304、泄漏 E0256）。
                let mutex_guard_borrows_receiver = matches!(
                    &call.callee,
                    ResolvedCallee::Builtin(b)
                        if matches!(b.as_str(), "mutex_get" | "mutex_set")
                );
                // Phase D (0.39.73): TokenChannel 是可 Copy 通道（非线性）——
                // 只有 SystemToken 载荷线性，跨任务 move 由 token 承担；通道
                // 共享无需借用特殊处理。
                let receiver_borrowed =
                    matches!(call.permission, Some(Permission::View | Permission::Mutate));
                let transform_handled = matches!(call.permission, Some(Permission::Mutate))
                    && self.method_transform_result(call);
                for (arg_index, argument) in call.arguments.iter().enumerate() {
                    if read_metric_builtin {
                        continue;
                    }
                    if mutex_guard_borrows_receiver && arg_index == 0 {
                        continue;
                    }
                    if receiver_borrowed && arg_index == 0 {
                        continue;
                    }
                    if transform_handled {
                        if let ResolvedExprKind::Load(place) = &argument.value.kind {
                            if self.place_is_linear(place) {
                                continue;
                            }
                        }
                    }
                    let transferred_endpoint = match &argument.value.kind {
                        ResolvedExprKind::Load(place) if place.projections.is_empty() => call
                            .session
                            .iter()
                            .any(|transition| transition.endpoint == place.base),
                        _ => false,
                    };
                    if transferred_endpoint {
                        continue;
                    }
                    // Audit 2026-08-05 (wave-2, C-2):
                    // `sink(if flag { a } else { b })` consumed BOTH arms
                    // under AND semantics while XOR runtime moves exactly
                    // one — the other arm's resource leaks on every run.
                    // Reject branch arguments carrying distinct resources.
                    if let Some(distinct) = self.xor_branch_violation(&argument.value) {
                        self.push_xor_diagnostic(&distinct, &expression.origin);
                        continue;
                    }
                    self.emit_consumes(
                        CanonicalActionKind::Move,
                        self.capability_places(&argument.value),
                        &expression.node_id,
                        &expression.origin,
                    );
                }
                for transition in &call.session {
                    let place = self.place_from_local(&transition.endpoint);
                    self.push_action(
                        &expression.node_id,
                        &expression.origin,
                        ActionDraft {
                            kind: if transition.terminal {
                                CanonicalActionKind::Drop
                            } else {
                                CanonicalActionKind::TransferSession
                            },
                            resource: self.resource_for_local(&transition.endpoint),
                            source: Some(place.clone()),
                            target: (!transition.terminal).then_some(place),
                            loan: None,
                        },
                    );
                }
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.visit_expr(condition, None);
                self.visit_block(then_block, false);
                self.visit_block(else_block, false);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee, None);
                // 0.36.36 candidate (1): an EXHAUSTIVE match over a linear
                // aggregate container (Option/Result) dissolves the CONTAINER
                // obligation — every arm either binds the payload (its own
                // resource chain continues) or has none. Fail-closed guards:
                // single linear source, aggregate container type, and no
                // wildcard anywhere in any arm pattern (a wildcard position
                // could strand linear payload atoms).
                let arm_patterns: Vec<&ResolvedPattern> =
                    arms.iter().map(|arm| &arm.pattern).collect();
                if let Some(container_place) = self.linear_match_container(scrutinee, &arm_patterns)
                {
                    self.emit_consumes(
                        CanonicalActionKind::Drop,
                        vec![container_place],
                        &expression.node_id,
                        &expression.origin,
                    );
                }
                for arm in arms {
                    self.visit_arm(arm);
                }
            }
            ResolvedExprKind::Lambda(lambda) => {
                if lambda
                    .captures
                    .iter()
                    .any(|capture| self.map_root_locals.contains(capture))
                {
                    self.map_root_profile_invalid = true;
                }
                let captures = lambda
                    .captures
                    .iter()
                    .filter(|capture| {
                        self.body
                            .locals
                            .get(capture)
                            .is_some_and(|local| self.is_linear(&local.ty))
                    })
                    .map(|capture| self.place_from_local(capture))
                    .collect();
                self.emit_consumes(
                    CanonicalActionKind::TransferChild,
                    captures,
                    &expression.node_id,
                    &expression.origin,
                );
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.visit_block(block, false),
            _ => self.for_each_expr_child(expression, |this, child| this.visit_expr(child, None)),
        }
    }

    /// 0.36.36 candidate (1): is this match professor an exhaustive
    /// destructure of ONE linear aggregate container (Option/Result) with no
    /// wildcard patterns? When yes, the container's obligation is discharged
    /// at the match (payload bindings keep their own chains).
    fn linear_match_container(
        &self,
        scrutinee: &ResolvedExpr,
        patterns: &[&ResolvedPattern],
    ) -> Option<Place> {
        let ty = &scrutinee.ty;
        if !self.is_linear(ty) || self.is_droppable_type(ty) {
            return None;
        }
        let is_aggregate = matches!(
            self.types.get(ty),
            Some(ResolvedType::Option(_)) | Some(ResolvedType::Result { .. })
        );
        if !is_aggregate {
            return None;
        }
        if patterns.is_empty() || patterns.iter().any(|p| self.pattern_strands_linear(p)) {
            return None;
        }
        let places = self.capability_places(scrutinee);
        if places.len() != 1 {
            return None;
        }
        if self.resources_for_place(&places[0]).len() != 1 {
            return None;
        }
        Some(places[0].clone())
    }

    /// 0.36.36: a wildcard position strands a LINEAR atom when the covered
    /// field/pattern slot is linear (Some(_) over Option<cap>); wildcards
    /// over non-linear slots (Err(_) over a string payload) are harmless.
    fn pattern_strands_linear(&self, pattern: &ResolvedPattern) -> bool {
        match &pattern.kind {
            ResolvedPatternKind::Wildcard => self.is_linear(&pattern.ty),
            ResolvedPatternKind::Constructor { fields, .. } => fields
                .iter()
                .any(|(_, pattern)| self.pattern_strands_linear(pattern)),
            ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => patterns
                .iter()
                .any(|pattern| self.pattern_strands_linear(pattern)),
            ResolvedPatternKind::Slice { prefix, rest } => {
                prefix
                    .iter()
                    .any(|pattern| self.pattern_strands_linear(pattern))
                    || rest
                        .as_deref()
                        .is_some_and(|pattern| self.pattern_strands_linear(pattern))
            }
            ResolvedPatternKind::Binding { .. } | ResolvedPatternKind::Literal(_) => false,
        }
    }

    /// 0.36.37: a for/while-let loop over a linear container is an
    /// exhaustive, element-wise deconstruction — the container obligation can
    /// dissolve at the loop statement (candidate (1) extended from
    /// match/if-let). The guard mirrors `linear_match_container` and adds the
    /// builtin container nominals (List/Map/Set): single linear source,
    /// single owned identity, no linear-stranding wildcard in the pattern.
    fn linear_loop_container(
        &self,
        iterable: &ResolvedExpr,
        pattern: &ResolvedPattern,
    ) -> Option<Place> {
        let ty = &iterable.ty;
        if !self.is_linear(ty) || self.is_droppable_type(ty) {
            return None;
        }
        let is_aggregate = match self.types.get(ty) {
            Some(ResolvedType::Option(_)) | Some(ResolvedType::Result { .. }) => true,
            // builtin container nominals are linear exactly when an argument
            // is (H2 recursion) — kept here for shape affinity with the
            // 0.36.36 match guard.
            Some(ResolvedType::Nominal {
                item, arguments, ..
            }) if matches!(
                item.as_str(),
                "builtin:type:List" | "builtin:type:Map" | "builtin:type:Set"
            ) && arguments.iter().any(|argument| self.is_linear(argument)) =>
            {
                true
            }
            _ => false,
        };
        if !is_aggregate {
            return None;
        }
        if self.pattern_strands_linear(pattern) {
            return None;
        }
        let places = self.capability_places(iterable);
        if places.len() != 1 {
            return None;
        }
        if self.resources_for_place(&places[0]).len() != 1 {
            return None;
        }
        Some(places[0].clone())
    }

    /// 0.36.37: builtin sequence container nominals (List/Map/Set) with linear
    /// arguments — the only shapes whose for-loops exhaustively deconstruct
    /// the container at runtime (VM iterates the container once, taking each
    /// element). Option/Result while-let re-evaluates its initializer without
    /// consuming the binding, so it is NOT dissolvable.
    fn linear_sequence_container(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Nominal { item, arguments, .. })
                if matches!(
                    item.as_str(),
                    "builtin:type:List" | "builtin:type:Map" | "builtin:type:Set"
                ) && arguments.iter().any(|argument| self.is_linear(argument))
        )
    }

    /// 0.36.37: does the loop body contain an early exit that would abandon
    /// not-yet-iterated elements? `break` exits the loop mid-iteration (the
    /// remaining elements are never consumed at runtime); `return` exits the
    /// function. `continue` is safe — the loop still visits every element and
    /// a skipped element consumption is caught by the per-iteration element
    /// obligation (E0256). A `break` inside a NESTED loop targets the nested
    /// loop only (`count_breaks = false` there), but a `return` escapes
    /// everything.
    fn block_has_early_exit(&mut self, block: &ResolvedBlock, count_breaks: bool) -> bool {
        block
            .statements
            .iter()
            .any(|statement| self.stmt_has_early_exit(statement, count_breaks))
    }

    fn stmt_has_early_exit(&mut self, statement: &ResolvedStmt, count_breaks: bool) -> bool {
        match &statement.kind {
            ResolvedStmtKind::Break(_) => count_breaks,
            ResolvedStmtKind::Return { .. } => true,
            ResolvedStmtKind::Continue => false,
            // Nested loops own their breaks; only a return inside their
            // bodies escapes THIS loop. Their condition/iterable expressions
            // still evaluate in this loop's context — an expression-embedded
            // break there targets this loop.
            ResolvedStmtKind::While {
                condition, body, ..
            } => {
                self.expr_has_early_exit(condition, count_breaks)
                    || self.block_has_early_exit(body, false)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                self.expr_has_early_exit(initializer, count_breaks)
                    || self.block_has_early_exit(body, false)
            }
            ResolvedStmtKind::Loop(body) => self.block_has_early_exit(body, false),
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                self.expr_has_early_exit(initializer, count_breaks)
                    || self.block_has_early_exit(then_block, count_breaks)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| self.block_has_early_exit(block, count_breaks))
            }
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|value| self.expr_has_early_exit(value, count_breaks)),
            ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => self.expr_has_early_exit(value, count_breaks),
            ResolvedStmtKind::Assign { value, .. } => self.expr_has_early_exit(value, count_breaks),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                self.expr_has_early_exit(value, count_breaks)
                    || self.block_has_early_exit(body, count_breaks)
            }
            ResolvedStmtKind::Scope { body, .. } => self.block_has_early_exit(body, count_breaks),
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Math(_)
            | ResolvedStmtKind::NestedCallable(_) => false,
        }
    }

    fn expr_has_early_exit(&mut self, expression: &ResolvedExpr, count_breaks: bool) -> bool {
        match &expression.kind {
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expr_has_early_exit(condition, count_breaks)
                    || self.block_has_early_exit(then_block, count_breaks)
                    || self.block_has_early_exit(else_block, count_breaks)
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.expr_has_early_exit(scrutinee, count_breaks)
                    || arms.iter().any(|arm| {
                        arm.guard
                            .as_ref()
                            .is_some_and(|guard| self.expr_has_early_exit(guard, count_breaks))
                            || self.expr_has_early_exit(&arm.body, count_breaks)
                    })
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.block_has_early_exit(block, count_breaks),
            ResolvedExprKind::Comprehension {
                iterable,
                guard,
                value,
                ..
            } => {
                self.expr_has_early_exit(iterable, count_breaks)
                    || guard
                        .as_ref()
                        .is_some_and(|guard| self.expr_has_early_exit(guard, count_breaks))
                    || self.expr_has_early_exit(value, count_breaks)
            }
            _ => {
                let mut found = false;
                self.for_each_expr_child(expression, |this, child| {
                    if this.expr_has_early_exit(child, count_breaks) {
                        found = true;
                    }
                });
                found
            }
        }
    }

    fn visit_arm(&mut self, arm: &MatchArm) {
        if let Some(guard) = &arm.guard {
            self.visit_expr(guard, None);
        }
        // 0.36.45: 臂模式绑定 Introduce——与 IfLet 臂同款循环载入修正：臂绑定
        // 是逐迭代临时资源（非循环单遍 or_insert 即够）；循环内需每迭代复位，
        // 否则上一迭代的 Consumed 事实被背边携带 → "moved after consumed"。
        // 键在臂体首动作内层节点（0.36.45 then 头键同理由：点序内层在前，
        // Introduce 秩 3 < Move 秩 5 保证先复位后消费）。
        let mut bindings = Vec::new();
        self.linear_bindings(&arm.pattern, &mut bindings);
        if !bindings.is_empty() {
            let (node, origin) = self.linear_action_node(&arm.body);
            for binding in bindings {
                let target = self.place_from_local(&binding);
                self.push_action(
                    &node,
                    &origin,
                    ActionDraft {
                        kind: CanonicalActionKind::Introduce,
                        resource: self.resource_for_local(&binding),
                        source: Some(target.clone()),
                        target: Some(target),
                        loan: None,
                    },
                );
            }
        }
        self.visit_expr(&arm.body, None);
    }

    fn emit_consumes(
        &mut self,
        kind: CanonicalActionKind,
        places: Vec<Place>,
        node: &NodeId,
        origin: &crate::core::Origin,
    ) {
        let mut seen = BTreeSet::new();
        for place in places {
            // Audit 2026-08-05 (wave-2, G-1): a place may own several
            // resource identities after an aggregate merge (`let x = (a, b);
            // return x`); consuming it discharges every one of them.
            for resource in self.resources_for_place(&place) {
                if !seen.insert(resource.clone()) {
                    continue;
                }
                self.push_action(
                    node,
                    origin,
                    ActionDraft {
                        kind,
                        resource,
                        source: Some(place.clone()),
                        target: None,
                        loan: None,
                    },
                );
            }
        }
    }

    fn push_action(&mut self, node: &NodeId, origin: &crate::core::Origin, draft: ActionDraft) {
        // v0.34.8 (golden §6.2): restore the double-consume debug assertion.
        // Only `Drop` triggers — Move is a transfer (aggregate move-through
        // legitimately emits Move → Move chains for one resource).
        //
        // v0.34.8 偏差：实现为 warn-only 而非 panic。用户错误（如
        // session_double_close_rejected 的 E0304 场景）也会触发第二次
        // Drop——panic 会把合法 L2 负测试炸成 ICE。warn 保留检测信号，
        // checker 诊断（E0304/E0425）正常完成。精确路径分析排 1.x。
        #[cfg(debug_assertions)]
        {
            use crate::core::CanonicalActionKind;
            if draft.kind == CanonicalActionKind::Drop {
                mimi_assert!(
                    !self.consumed_resources.contains(&draft.resource),
                    "RESOURCE-LINEAR-001: resource {:?} dropped twice (debug signal) — \
                     genuine double-drop indicates an ownership-analysis bug; \
                     user-level double-consume is caught by E0304/E0425 diagnostics.",
                    draft.resource
                );
                self.consumed_resources.insert(draft.resource.clone());
            }
        }
        let location = self.location(node, origin);
        self.actions.push(CanonicalResourceAction {
            kind: draft.kind,
            resource: draft.resource,
            source: draft.source,
            target: draft.target,
            loan: draft.loan,
            location,
            span: origin.user_span(),
            origin: origin.clone(),
        });
    }

    fn entry_location(&self) -> CfgLocation {
        let block = self
            .cfg
            .block(&self.cfg.entry)
            .expect("validated CFG has an entry block");
        CfgLocation {
            block: self.cfg.entry.clone(),
            point: block.source.node.clone(),
            edge: None,
        }
    }

    fn location(&mut self, node: &NodeId, origin: &crate::core::Origin) -> CfgLocation {
        self.locations.get(node).cloned().unwrap_or_else(|| {
            self.errors.push(Diagnostic::error(
                format!("resource action '{}' has no CFG point", node.0),
                origin.user_span(),
            ));
            self.entry_location()
        })
    }

    fn is_linear(&self, ty: &ResolvedTypeId) -> bool {
        match self.types.get(ty) {
            Some(ResolvedType::Capability(_)) => true,
            // 0.31.16: Flow state sets (multi-target transition results)
            // are linear — each state value can only be consumed once.
            Some(ResolvedType::FlowStateSet { .. }) => true,
            // SD-1: read structural flag set at interning time.
            // Replaces `starts_with("state:")` / `ends_with("SessionChan")`
            // string matching. Single source of truth: NominalTypeId::nominal_is_linear().
            Some(ResolvedType::Nominal { is_linear, .. }) if *is_linear => true,
            // H2 (audit-type 2026-08-03): builtin container nominals (List/
            // Map/Set intern as Nominal, unlike the structural Option/Result/
            // Array/Slice variants above) are linear when any type argument
            // is linear. Without this, `func sink(v: List<cap>) { }` could
            // discard the container — and every element — without consuming
            // anything, defeating exactly-once in concrete signatures.
            Some(ResolvedType::Nominal {
                item, arguments, ..
            }) => {
                // H2 + RECORD-LIN-001: a nominal is linear when (a) it is a
                // container (List/Map/Set) with any linear argument, (b) a
                // user record/union declared with a linear field, or (c) a
                // generic record instantiated with a linear argument.
                // linear_record_names stores the record's qualified name
                // ("Plain"); the nominal identity carries a "type:" prefix.
                let record_key = item.as_str().strip_prefix("type:").unwrap_or(item.as_str());
                self.linear_record_names.contains(record_key)
                    || arguments.iter().any(|arg| self.is_linear(arg))
            }
            Some(ResolvedType::Newtype { inner, .. }) => self.is_linear(inner),
            Some(ResolvedType::Tuple(elements)) => {
                elements.iter().any(|element| self.is_linear(element))
            }
            // P0-5: Recurse through container types. A linear resource
            // inside Option/Result/Array/Slice is still linear and must
            // be tracked (Introduce, Move, return check). Without this,
            // `Option<cap Token>` is invisible to the analysis.
            Some(ResolvedType::Option(inner)) => self.is_linear(inner),
            Some(ResolvedType::Result { ok, error }) => self.is_linear(ok) || self.is_linear(error),
            Some(ResolvedType::Array { element, .. }) => self.is_linear(element),
            Some(ResolvedType::Slice(inner)) => self.is_linear(inner),
            Some(ResolvedType::CBuffer(inner)) => self.is_linear(inner),
            // P0-6: GenericParameter returns false (conservative). Without
            // monomorphization, we cannot know if T will be instantiated
            // with a linear type. This is a documented analysis limitation:
            // generic functions may miss linear resource tracking.
            _ => false,
        }
    }

    fn place_is_linear(&self, place: &ResolvedPlace) -> bool {
        self.place_type(place).is_some_and(|ty| self.is_linear(&ty))
    }

    /// 0.36.47 容器方法变换面判定：结果 = 名义容器（List/Map/Set）或元组。
    /// 变换方法与 for 迭代同构——容器整体转出、义务移至结果；读取/提取方法
    /// （标量/裸元素/Option/Result 结果）保持借用面。
    fn method_transform_result(&self, call: &ResolvedCall) -> bool {
        match self.types.get(&call.result) {
            Some(ResolvedType::Nominal { item, .. }) => matches!(
                item.as_str(),
                "builtin:type:List" | "builtin:type:Map" | "builtin:type:Set"
            ),
            Some(ResolvedType::Tuple(_)) => true,
            _ => false,
        }
    }

    /// 0.36.46 定向头提取（绑定初始化器形状）：初始化为 `Load(xs[0])` 的
    /// 单投影字面量 0 + 非可弃线性容器基——目录与 Bind 臂共用的放行判定。
    fn is_directional_head_binding(&self, initializer: Option<&ResolvedExpr>) -> bool {
        let Some(value) = initializer else {
            return false;
        };
        let base = match &value.kind {
            // Legacy/borrow-place form: the index is already part of the
            // resolved place projection.
            ResolvedExprKind::Load(place) if self.is_directional_head_index(place) => {
                Some(&place.base)
            }
            // Canonical value-projection form: lowering keeps the typed index
            // child explicit as `Project { value: Load(v), Index(0) }`.
            // This must use the same catalog rule as the place form; otherwise
            // `catalog_pattern` aliases the extracted binding to the whole
            // container before `visit_stmt` can install its fresh element
            // identity, producing false double-consume diagnostics.
            ResolvedExprKind::Project {
                value: projected,
                projection: ResolvedValueProjection::Index(index),
            } => {
                let ResolvedExprKind::Load(place) = &projected.kind else {
                    return false;
                };
                let literal_zero = matches!(
                    &index.kind,
                    ResolvedExprKind::Literal(crate::core::ResolvedLiteral::Int(index))
                        if *index == 0
                );
                (place.projections.is_empty() && literal_zero).then_some(&place.base)
            }
            _ => None,
        };
        base.and_then(|base| self.body.locals.get(base))
            .is_some_and(|l| self.is_linear(&l.ty) && !self.is_droppable_type(&l.ty))
    }

    /// 0.36.46 定向头提取形状：`xs[0]`——单一投影、字面量常量 0 的 Index。
    /// 定向 = 只开头部位置；`xs[1]` / 动态索引 / 多级投影保持 fail-closed。
    fn is_directional_head_index(&self, place: &ResolvedPlace) -> bool {
        place.projections.len() == 1
            && matches!(
                &place.projections[0],
                ResolvedProjection::Index {
                    index: ResolvedIndex::Constant(0),
                    ..
                }
            )
    }

    /// M9 (0.36.22): element extraction by INDEX READ from a linear container
    /// was the fail-open member of the element-consumption gap — the ledger
    /// attributed the whole container as consumed by the read, but only the
    /// extracted handle was released, silently leaking every unextracted
    /// element (inconsistent with match/for extraction, which are fail-closed
    /// E0256/E0304). 0.36.25: the SLICE sibling (`v[1..]`) copies the same
    /// handle values while consuming the container obligation — identical
    /// leak, closed identically. Reject uniformly: a linear container must
    /// be moved or dropped as a whole. 0.36.46: 定向头提取面（let 绑定 +
    /// 字面量 0）例外——见 is_directional_head_index 与 Bind 臂配对。
    fn reject_index_read_extraction(&mut self, expression: &ResolvedExpr) {
        // Non-droppable linear element containers (Cap/SessionChan) leak
        // every unextracted element on element-level reads (M9/slice).
        // Flow-state-element containers are auto-droppable at scope exit
        // (0.31.16 P0-5), so element reads there are a sanctioned pattern
        // and stay legal.
        let non_droppable_linear_container = |local: &ResolvedLocal| -> bool {
            self.is_linear(&local.ty) && !self.is_droppable_type(&local.ty)
        };
        let mut emitted = false;
        match &expression.kind {
            ResolvedExprKind::Load(place) => {
                let has_index = place
                    .projections
                    .iter()
                    .any(|projection| matches!(projection, ResolvedProjection::Index { .. }));
                // 0.36.26: tuple field access `t.0` — extracting one atom from
                // a linear tuple leaks the sibling atoms (destructure instead).
                let has_tuple = place
                    .projections
                    .iter()
                    .any(|projection| matches!(projection, ResolvedProjection::Tuple { .. }));
                if has_index || has_tuple {
                    if let Some(local) = self.body.locals.get(&place.base) {
                        if non_droppable_linear_container(local) {
                            if has_index {
                                if self.in_bind_initializer && self.is_directional_head_index(place)
                                {
                                    // 0.36.46 定向头提取：`let c = xs[0]`——
                                    // 字面量 0、单一投影、直接局部基、非可弃
                                    // 线性容器。c 认领一个元素义务（Bind 臂做
                                    // fresh Introduce），容器保留余部义务（须
                                    // 整体消费一次：drop = 释放余部）。每容器
                                    // 至多一次索引提取——重复认领 → E0304。
                                    if !self.extracted_containers.insert(place.base.clone()) {
                                        self.errors.push(
                                            Diagnostic::error_code(
                                                crate::diagnostic::codes::E0304,
                                                format!(
                                                    "resource '{}' head element is claimed more than once",
                                                    self.local_name(&place.base)
                                                ),
                                                expression.origin.user_span(),
                                            )
                                            .with_help(&format!(
                                                "extract `{}[0]` at most once per container; consume the remainder with a whole-container drop/move/return",
                                                self.local_name(&place.base)
                                            )),
                                        );
                                        self.last_visit_rejected = true;
                                        emitted = true;
                                    } else {
                                        self.directional_extraction_base = Some(place.base.clone());
                                    }
                                    // 放行（不 reject）：Bind 臂专门配对。
                                } else {
                                    self.push_index_read_error(&place.base, expression);
                                    emitted = true;
                                }
                            } else {
                                self.push_element_leak_error(expression);
                                emitted = true;
                            }
                        }
                    }
                }
            }
            // 0.36.25: `v[1..]` on a linear container — the slice copies the
            // handle values (alias!) and only the slice's copies are dropped;
            // the container's own handles leak. Same fail-closed rule.
            ResolvedExprKind::Slice { target, .. } => {
                if let ResolvedExprKind::Load(place) = &target.kind {
                    if place.projections.is_empty() {
                        if let Some(local) = self.body.locals.get(&place.base) {
                            if non_droppable_linear_container(local) {
                                self.push_index_read_error(&place.base, expression);
                                emitted = true;
                            }
                        }
                    }
                }
            }
            // 0.36.26: non-place element extraction `[a, b][0]` / `(a, b).0` — the
            // collect side selects only the indexed element, so the pairing
            // balances and the unextracted linear elements leak silently.
            // Judged by the container's TYPE (list literals hold cap
            // constants, not places — per-element probing misses them).
            ResolvedExprKind::Project { value, projection } => {
                let is_element_extraction = matches!(
                    projection,
                    ResolvedValueProjection::Index(_) | ResolvedValueProjection::Tuple(_)
                );
                if is_element_extraction
                    && self.is_linear(&value.ty)
                    && !self.is_droppable_type(&value.ty)
                {
                    // 0.36.46: resolved lowering represents a local index
                    // read as `Project { value: Load(v), Index(0) }`, while
                    // the older place path above sees `Load(v[0])`. Keep the
                    // directional-head contract identical for both forms:
                    // only a direct local base, literal zero, and let-binding
                    // may claim one element while retaining the container's
                    // remainder obligation. Missing this canonical Project
                    // form regressed every `let x = v[0]` test to the generic
                    // extraction-not-tracked diagnostic.
                    let mut handled_directional = false;
                    if self.in_bind_initializer {
                        if let (
                            ResolvedValueProjection::Index(index),
                            ResolvedExprKind::Load(place),
                        ) = (projection, &value.kind)
                        {
                            let direct_local = place.projections.is_empty();
                            let literal_zero = matches!(
                                &index.kind,
                                ResolvedExprKind::Literal(
                                    crate::core::ResolvedLiteral::Int(index),
                                ) if *index == 0
                            );
                            if direct_local && literal_zero {
                                handled_directional = true;
                                if !self.extracted_containers.insert(place.base.clone()) {
                                    self.errors.push(
                                        Diagnostic::error_code(
                                            crate::diagnostic::codes::E0304,
                                            format!(
                                                "resource '{}' head element is claimed more than once",
                                                self.local_name(&place.base)
                                            ),
                                            expression.origin.user_span(),
                                        )
                                        .with_help(&format!(
                                            "extract `{}[0]` at most once per container; consume the remainder with a whole-container drop/move/return",
                                            self.local_name(&place.base)
                                        )),
                                    );
                                    self.last_visit_rejected = true;
                                    emitted = true;
                                } else {
                                    self.directional_extraction_base = Some(place.base.clone());
                                }
                                // The Bind arm below pairs the extracted
                                // element with a fresh resource and leaves the
                                // container identity in place for the rest.
                            }
                        }
                    }
                    if !handled_directional {
                        // Extracting the ONLY literal element is a whole
                        // consumption (no leak) — stay legal.
                        let single_literal = match &value.kind {
                            ResolvedExprKind::List(elements) => elements.len() == 1,
                            ResolvedExprKind::Tuple(elements) => elements.len() == 1,
                            _ => false,
                        };
                        if !single_literal {
                            // Preserve the more precise index-read diagnostic
                            // for a direct local value projection outside the
                            // let-binding exception (`sink(v[0])`, `v[1]`).
                            // The canonical Project form must retain the same
                            // fail-closed surface as the legacy Load(place)
                            // form; only literal aggregate projections use
                            // the generic element-leak wording.
                            if let (
                                ResolvedValueProjection::Index(_),
                                ResolvedExprKind::Load(place),
                            ) = (projection, &value.kind)
                            {
                                if place.projections.is_empty() {
                                    self.push_index_read_error(&place.base, expression);
                                } else {
                                    self.push_element_leak_error(expression);
                                }
                            } else {
                                self.push_element_leak_error(expression);
                            }
                            emitted = true;
                        }
                    }
                }
            }
            _ => {}
        }
        // 0.36.43: neuter the rejected extraction's sources — later consumers
        // (binds/calls/drops) must not fabricate transfers for a value that
        // never moved (the E0304 already fails the function; the fabricated
        // transfer used to double-drop the container on a later drop(v)).
        if emitted {
            self.last_visit_rejected = true;
            for place in self.capability_places(expression) {
                self.rejected_extraction_places.insert(place);
            }
        }
    }

    fn push_element_leak_error(&mut self, expression: &ResolvedExpr) {
        self.errors.push(
            Diagnostic::error_code(
                crate::diagnostic::codes::E0304,
                "element-level extraction from a linear container is not tracked and \
                     leaks every unextracted element"
                    .to_string(),
                expression.origin.user_span(),
            )
            .with_help(
                "move or drop the whole container, or destructure it to bind every \
                 element explicitly",
            ),
        );
    }

    fn push_index_read_error(&mut self, base: &ResolvedLocalId, expression: &ResolvedExpr) {
        self.errors.push(
            Diagnostic::error_code(
                crate::diagnostic::codes::E0304,
                format!(
                    "'{}' cannot be read by index or slice: element-level extraction \
                     from a linear container is not tracked and leaks every \
                     unextracted element",
                    self.local_name(base)
                ),
                expression.origin.user_span(),
            )
            .with_help(
                "move or drop the whole container (e.g. drop(v)) instead of \
                 indexing or slicing into it",
            ),
        );
    }

    fn place_type(&self, place: &ResolvedPlace) -> Option<ResolvedTypeId> {
        self.body
            .locals
            .get(&place.base)
            .map(|local| place.projected_type(local).clone())
    }

    fn linear_bindings(&self, pattern: &ResolvedPattern, bindings: &mut Vec<ResolvedLocalId>) {
        match &pattern.kind {
            ResolvedPatternKind::Binding { local, .. } if self.is_linear(&pattern.ty) => {
                bindings.push(local.clone());
            }
            ResolvedPatternKind::Constructor { fields, .. } => {
                for (_, pattern) in fields {
                    self.linear_bindings(pattern, bindings);
                }
            }
            ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => {
                for pattern in patterns {
                    self.linear_bindings(pattern, bindings);
                }
            }
            ResolvedPatternKind::Slice { prefix, rest } => {
                for pattern in prefix {
                    self.linear_bindings(pattern, bindings);
                }
                if let Some(rest) = rest {
                    self.linear_bindings(rest, bindings);
                }
            }
            ResolvedPatternKind::Wildcard
            | ResolvedPatternKind::Literal(_)
            | ResolvedPatternKind::Binding { .. } => {}
        }
    }

    fn single_binding(&self, pattern: &ResolvedPattern) -> Option<ResolvedLocalId> {
        match &pattern.kind {
            ResolvedPatternKind::Binding { local, .. } => Some(local.clone()),
            _ => None,
        }
    }

    /// True when the pattern contains a wildcard anywhere (nested included).
    /// Audit 2026-08-05 (wave-2, review §1.3): wildcard positions in a
    /// split() pattern silently discard capability atoms.
    fn pattern_has_wildcard(&self, pattern: &ResolvedPattern) -> bool {
        match &pattern.kind {
            ResolvedPatternKind::Wildcard => true,
            ResolvedPatternKind::Constructor { fields, .. } => fields
                .iter()
                .any(|(_, pattern)| self.pattern_has_wildcard(pattern)),
            ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => patterns
                .iter()
                .any(|pattern| self.pattern_has_wildcard(pattern)),
            ResolvedPatternKind::Slice { prefix, rest } => {
                prefix
                    .iter()
                    .any(|pattern| self.pattern_has_wildcard(pattern))
                    || rest
                        .as_deref()
                        .is_some_and(|pattern| self.pattern_has_wildcard(pattern))
            }
            ResolvedPatternKind::Literal(_) | ResolvedPatternKind::Binding { .. } => false,
        }
    }

    /// Number of top-level pattern positions. A split() destructure reports
    /// its tuple arity even when a wildcard ate one binding — the slot
    /// count, not the surviving linear-binding count, identifies the split
    /// shape so the wildcard rejection cannot be vacated by compression
    /// (`let (_, w) = c.split()` still counts 2 slots, 1 binding).
    fn pattern_slot_count(&self, pattern: &ResolvedPattern) -> usize {
        match &pattern.kind {
            ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => {
                patterns.len()
            }
            ResolvedPatternKind::Constructor { fields, .. } => fields.len(),
            _ => 1,
        }
    }

    /// Audit 2026-08-05 (wave-2, G-1): every resource identity currently
    /// owned by the source places, in construction order, deduplicated.
    /// Aggregate owners (`let x = (a, b)`) expand to all owned identities.
    fn resources_for_place(&self, place: &Place) -> Vec<ResourceId> {
        if place.projections.is_empty() {
            if let Some(resources) = self.resources.get(&ResolvedLocalId(place.base.0.clone())) {
                return resources.clone();
            }
        }
        vec![self.resource_for_place(place)]
    }

    fn expand_sources(&self, sources: &[Place]) -> Vec<ResourceId> {
        self.expand_source_pairs(sources)
            .into_iter()
            .map(|(resource, _)| resource)
            .collect()
    }

    /// (resource, originating place) pairs for the sources, deduplicated by
    /// resource with the first originating place kept. The place is the
    /// action source — the owner-validation target of dataflow (H-5).
    fn expand_source_pairs(&self, sources: &[Place]) -> Vec<(ResourceId, Place)> {
        let mut pairs = Vec::new();
        let mut seen = BTreeSet::new();
        for source in sources {
            for resource in self.resources_for_place(source) {
                if seen.insert(resource.clone()) {
                    pairs.push((resource, source.clone()));
                }
            }
        }
        pairs
    }

    /// Audit 2026-08-05 (wave-2, C-2): If/Match are XOR — exactly one arm's
    /// value flows at runtime. When a consumed value is (or contains, in
    /// value position) a branch expression whose arms carry SEVERAL distinct
    /// linear resources, consuming it can discharge at most one obligation;
    /// the others leak on every path not taking their arm. Returns one
    /// representative place per distinct resource when the value violates.
    /// AND aggregates are NOT violations: `(a, b)` moves every element.
    fn xor_branch_violation(&self, value: &ResolvedExpr) -> Option<Vec<Place>> {
        match &value.kind {
            ResolvedExprKind::If { .. } | ResolvedExprKind::Match { .. } => {
                let mut representatives: Vec<Place> = Vec::new();
                let mut seen = BTreeSet::new();
                for place in self.capability_places(value) {
                    for resource in self.resources_for_place(&place) {
                        if seen.insert(resource) {
                            representatives.push(place.clone());
                        }
                    }
                }
                (representatives.len() > 1).then_some(representatives)
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => block
                .result
                .as_ref()
                .and_then(|result| self.xor_branch_violation(result)),
            ResolvedExprKind::Scope { body, .. } => body
                .result
                .as_ref()
                .and_then(|result| self.xor_branch_violation(result)),
            ResolvedExprKind::Cast { value, .. }
            | ResolvedExprKind::Try { value, .. }
            | ResolvedExprKind::Spawn(value)
            | ResolvedExprKind::Await(value) => self.xor_branch_violation(value),
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => values
                .iter()
                .find_map(|value| self.xor_branch_violation(value)),
            ResolvedExprKind::Record { fields, .. } => fields
                .iter()
                .find_map(|field| self.xor_branch_violation(&field.value)),
            ResolvedExprKind::Map(pairs) => pairs.iter().find_map(|(key, value)| {
                self.xor_branch_violation(key)
                    .or_else(|| self.xor_branch_violation(value))
            }),
            ResolvedExprKind::Project { value, .. } => self.xor_branch_violation(value),
            _ => None,
        }
    }

    fn push_xor_diagnostic(&mut self, representatives: &[Place], origin: &crate::core::Origin) {
        let names = representatives
            .iter()
            .map(Place::display)
            .collect::<Vec<_>>()
            .join("', '");
        self.errors.push(
            Diagnostic::error_code(
                crate::diagnostic::codes::E0840,
                format!(
                    "branch expression carries {} distinct linear resources ('{}') but exactly \
                     one flows at runtime — consuming it leaks every arm that is not taken",
                    representatives.len(),
                    names
                ),
                origin.user_span(),
            )
            .with_help(
                "consume each capability on its own control-flow path, or bind the branches to \
                 distinct places and consume them separately",
            ),
        );
    }

    /// Audit 2026-08-05 (wave-2, review §5.5): the call whose linear result a
    /// statement-style expression discards (through any number of plain
    /// block/scope wrappers). Used to establish the dropped obligation.
    fn discarded_linear_call<'b>(
        &self,
        mut expression: &'b ResolvedExpr,
    ) -> Option<&'b ResolvedExpr> {
        loop {
            match &expression.kind {
                ResolvedExprKind::Call(_) => {
                    return (self.is_linear(&expression.ty)
                        && !self.is_droppable_type(&expression.ty))
                    .then_some(expression);
                }
                ResolvedExprKind::Block(block)
                | ResolvedExprKind::Comptime(block)
                | ResolvedExprKind::Quote(block) => {
                    expression = block.result.as_ref()?;
                }
                ResolvedExprKind::Scope { body, .. } => {
                    expression = body.result.as_ref()?;
                }
                _ => return None,
            }
        }
    }

    /// Audit 2026-08-05 (wave-2, H-6): terminate anonymous loans parked
    /// during statement visits at the statement's terminating CFG point.
    fn flush_pending_loans(&mut self, node: &NodeId, origin: &crate::core::Origin) {
        if self.pending_anonymous_loans.is_empty() {
            return;
        }
        let Some(location) = self.locations.get(node).cloned() else {
            return; // no CFG point here — keep them for the next flush site
        };
        let pending = std::mem::take(&mut self.pending_anonymous_loans);
        for (loan_id, resource) in pending {
            self.actions.push(CanonicalResourceAction {
                kind: CanonicalActionKind::BorrowEnd,
                resource,
                source: None,
                target: None,
                loan: Some(loan_id),
                location: location.clone(),
                span: origin.user_span(),
                origin: origin.clone(),
            });
        }
    }

    fn capability_places(&self, expression: &ResolvedExpr) -> Vec<Place> {
        let mut places = Vec::new();
        self.collect_capability_places(expression, &mut places);
        places
    }

    fn collect_capability_places(&self, expression: &ResolvedExpr, places: &mut Vec<Place>) {
        match &expression.kind {
            ResolvedExprKind::Load(place) if self.place_is_linear(place) => {
                let canonical = self.canonical_place(place);
                if !self.rejected_extraction_places.contains(&canonical) {
                    places.push(canonical);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.collect_capability_places(value, places);
                }
            }
            ResolvedExprKind::Record { fields, .. } => {
                for field in fields {
                    self.collect_capability_places(&field.value, places);
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                let selected = match (projection, &value.kind) {
                    (
                        ResolvedValueProjection::Field(projected),
                        ResolvedExprKind::Record { fields, .. },
                    ) => fields
                        .iter()
                        .find(|field| &field.field == projected)
                        .map(|field| &field.value),
                    (ResolvedValueProjection::Tuple(index), ResolvedExprKind::Tuple(values)) => {
                        values.get(*index)
                    }
                    (ResolvedValueProjection::Index(index), ResolvedExprKind::List(values)) => {
                        match &index.kind {
                            ResolvedExprKind::Literal(crate::core::ResolvedLiteral::Int(index))
                                if *index >= 0 =>
                            {
                                values.get(*index as usize)
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(selected) = selected {
                    self.collect_capability_places(selected, places);
                } else {
                    // The typed projection is closed but not statically
                    // separable. Conservatively consume all candidate linear
                    // inputs instead of inventing a partial-move identity.
                    self.collect_capability_places(value, places);
                }
            }
            ResolvedExprKind::Cast { value, .. } => self.collect_capability_places(value, places),
            ResolvedExprKind::Block(block) => {
                if let Some(value) = &block.result {
                    self.collect_capability_places(value, places);
                }
            }
            ResolvedExprKind::If {
                then_block,
                else_block,
                ..
            } => {
                if let Some(result) = &then_block.result {
                    self.collect_capability_places(result, places);
                }
                if let Some(result) = &else_block.result {
                    self.collect_capability_places(result, places);
                }
            }
            ResolvedExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.collect_capability_places(&arm.body, places);
                }
            }
            // P0-7: Previously silently ignored by `_ => {}`. Linear resources
            // inside these expressions were invisible to the analysis — no
            // Move/TransferChild actions generated, no E0304 double-consume
            // diagnostics.
            ResolvedExprKind::Spawn(inner) | ResolvedExprKind::Await(inner) => {
                self.collect_capability_places(inner, places);
            }
            // NOTE: Call arguments are NOT collected here — they are already
            // handled at the call-site level (visit_expr's Call arm above),
            // which calls capability_places on each argument directly. Adding
            // Call here would double-count and cause false E0304 diagnostics.
            ResolvedExprKind::Map(pairs) => {
                for (key, value) in pairs {
                    self.collect_capability_places(key, places);
                    self.collect_capability_places(value, places);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.collect_capability_places(value, places);
                self.collect_capability_places(iterable, places);
                if let Some(guard) = guard {
                    self.collect_capability_places(guard, places);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => {
                self.collect_capability_places(receiver, places);
            }
            ResolvedExprKind::Range { start, end } => {
                self.collect_capability_places(start, places);
                self.collect_capability_places(end, places);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.collect_capability_places(target, places);
                if let Some(start) = start {
                    self.collect_capability_places(start, places);
                }
                if let Some(end) = end {
                    self.collect_capability_places(end, places);
                }
            }
            ResolvedExprKind::Unary { operand, .. } => {
                self.collect_capability_places(operand, places);
            }
            ResolvedExprKind::Try { value, .. }
            | ResolvedExprKind::TypeOf(value)
            | ResolvedExprKind::Old(value) => {
                self.collect_capability_places(value, places);
            }
            ResolvedExprKind::Scope { body, .. } | ResolvedExprKind::Comptime(body) => {
                if let Some(result) = &body.result {
                    self.collect_capability_places(result, places);
                }
            }
            // Leaf / non-place expressions: no linear resources to track.
            // Call is handled at the call-site level (visit_expr's Call arm),
            // NOT here — recursing into arguments would double-count.
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::FString(_)
            | ResolvedExprKind::Lambda(_)
            | ResolvedExprKind::Quote(_)
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::Binary { .. }
            | ResolvedExprKind::Call(_)
            | ResolvedExprKind::Load(_) => {}
        }
    }

    /// Primary resource identity of a local (the first owned identity).
    /// Introduced obligations and session endpoints always carry a single
    /// identity; aggregate owners are consumed through
    /// `resources_for_place`, which expands every owned identity.
    fn resource_for_local(&self, local: &ResolvedLocalId) -> ResourceId {
        self.resources
            .get(local)
            .and_then(|resources| resources.first().cloned())
            .unwrap_or_else(|| ResourceId(local.0.clone()))
    }

    /// Give the closed MapRoot profile a stable resource identity directly
    /// from the Checker local. Map handles are not ordinary CFG-linear values
    /// yet, so their receipt identity must not depend on the general resource
    /// ledger's incidental contents.
    fn map_root_resource_identity(local: &ResolvedLocalId) -> ResourceId {
        ResourceId(local.0.clone())
    }

    /// Give MapAnyOwner v1 a disjoint identity namespace. Matching local ids
    /// in the MapRoot and MapAnyOwner ledgers therefore cannot be confused by
    /// a downstream adapter.
    fn map_any_owner_resource_identity(local: &ResolvedLocalId) -> ResourceId {
        ResourceId(NodeId(format!("map-any-owner-v1:{}", local.0 .0)))
    }

    fn resource_for_place(&self, place: &Place) -> ResourceId {
        self.resources
            .get(&ResolvedLocalId(place.base.0.clone()))
            .and_then(|resources| resources.first().cloned())
            .unwrap_or_else(|| ResourceId(place.base.0.clone()))
    }

    fn place_from_local(&self, local: &ResolvedLocalId) -> Place {
        Place::root(LocalId(local.0.clone()), self.local_name(local))
    }

    fn local_name(&self, local: &ResolvedLocalId) -> String {
        self.body
            .locals
            .get(local)
            .map(|local| local.display_name.clone())
            .unwrap_or_else(|| local.0 .0.clone())
    }

    fn canonical_place(&self, place: &ResolvedPlace) -> Place {
        let projections = place
            .projections
            .iter()
            .map(|projection| match projection {
                ResolvedProjection::Field { field, name, .. } => PlaceProjection::Field {
                    field: field.clone(),
                    name: name.clone(),
                },
                ResolvedProjection::Tuple { index, .. } => PlaceProjection::Tuple(*index),
                ResolvedProjection::Index { index, .. } => PlaceProjection::Index(match index {
                    ResolvedIndex::Constant(index) => IndexProjection::Constant(*index),
                    ResolvedIndex::Dynamic(_) => IndexProjection::Dynamic,
                }),
                ResolvedProjection::Deref { .. } => PlaceProjection::Deref,
            })
            .collect();
        Place {
            base: LocalId(place.base.0.clone()),
            base_name: self.local_name(&place.base),
            projections,
        }
    }

    /// Recover the stable place behind a typed read projection used as a
    /// borrow operand. The resolved body keeps indexed reads as value
    /// projections so the index remains an explicit typed child; ownership
    /// analysis must nevertheless retain `xs[0]`/`xs[*]` rather than inventing
    /// a temporary resource.
    fn borrow_place(&self, expression: &ResolvedExpr) -> Option<Place> {
        match &expression.kind {
            ResolvedExprKind::Load(place) => Some(self.canonical_place(place)),
            ResolvedExprKind::Project {
                value,
                projection: ResolvedValueProjection::Index(index),
            } => {
                let mut place = self.borrow_place(value)?;
                let index = match &index.kind {
                    ResolvedExprKind::Literal(crate::core::ir::ResolvedLiteral::Int(index)) => {
                        IndexProjection::Constant(*index)
                    }
                    _ => IndexProjection::Dynamic,
                };
                place.projections.push(PlaceProjection::Index(index));
                Some(place)
            }
            _ => None,
        }
    }

    fn for_each_expr_child(
        &mut self,
        expression: &ResolvedExpr,
        mut visit: impl FnMut(&mut Self, &ResolvedExpr),
    ) {
        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        visit(self, value);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                visit(self, value);
                if let ResolvedValueProjection::Index(index) = projection {
                    visit(self, index);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                visit(self, left);
                visit(self, right);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => visit(self, operand),
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    visit(self, &argument.value);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    visit(self, value);
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    visit(self, key);
                    visit(self, value);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                visit(self, iterable);
                if let Some(guard) = guard {
                    visit(self, guard);
                }
                visit(self, value);
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => visit(self, receiver),
            ResolvedExprKind::Record { fields, .. } => {
                for field in fields {
                    visit(self, &field.value);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                visit(self, start);
                visit(self, end);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                visit(self, target);
                if let Some(start) = start {
                    visit(self, start);
                }
                if let Some(end) = end {
                    visit(self, end);
                }
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::Lambda(_)
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_)
            | ResolvedExprKind::Block(_)
            | ResolvedExprKind::Scope { .. }
            | ResolvedExprKind::Comptime(_)
            | ResolvedExprKind::If { .. }
            | ResolvedExprKind::Match { .. }
            | ResolvedExprKind::Quote(_) => {}
        }
    }
}

pub fn analyze_resolved_bodies(
    cfgs: &BTreeMap<NodeId, CallableCfg>,
    bodies: &BTreeMap<NodeId, ResolvedBody>,
    signatures: &BTreeMap<NodeId, ResolvedSignature>,
    types: &ResolvedTypeTable,
    linear_record_names: &BTreeSet<String>,
) -> Result<BTreeMap<NodeId, ResourceAnalysis>, Vec<Diagnostic>> {
    let mut analyses = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, cfg) in cfgs {
        let Some(body) = bodies.get(owner) else {
            errors.push(Diagnostic::error(
                format!("CFG '{}' has no ResolvedBody", owner.0),
                cfg.block(&cfg.entry)
                    .map(|block| block.source.span)
                    .unwrap_or(crate::span::Span::UNKNOWN),
            ));
            continue;
        };
        let Some(signature) = signatures.get(owner) else {
            errors.push(Diagnostic::error(
                format!("CFG '{}' has no ResolvedSignature", owner.0),
                body.root.origin.user_span(),
            ));
            continue;
        };
        let analysis = ActionEmitter::new(cfg, body, signature, types, linear_record_names).emit();
        match analysis {
            Ok(analysis) => {
                analyses.insert(owner.clone(), analysis);
            }
            Err(mut action_errors) => errors.append(&mut action_errors),
        }
    }
    if errors.is_empty() {
        Ok(analyses)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::OrdinaryMapOwnedAnyV2RootSettlementKind;

    fn parse(source: &str) -> crate::ast::File {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse")
    }

    fn action_location_exists(cfg: &CallableCfg, action: &CanonicalResourceAction) -> bool {
        let Some(block) = cfg.block(&action.location.block) else {
            return false;
        };
        if let Some(edge_id) = &action.location.edge {
            return cfg
                .edge(edge_id)
                .is_some_and(|edge| edge.from == action.location.block);
        }
        block.source.node == action.location.point
            || block
                .points
                .iter()
                .any(|point| point.source.node == action.location.point)
    }

    #[test]
    fn typed_binding_move_preserves_resource_identity_and_only_root_result_returns() {
        // RESOURCE-LINEAR-001: a binding move changes the owner place, not the
        // logical resource identity; nested block results are not callable returns.
        let file = parse(
            r#"
cap Token
func forward(token: cap Token) -> cap Token {
    let moved = { token }
    moved
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("typed binding move checks");
        let owner = NodeId("function:forward".into());
        let body = program.resolved_body(&owner).expect("forward body");
        let token = body
            .locals
            .values()
            .find(|local| local.display_name == "token")
            .expect("token local");
        let expected = ResourceId(token.id.0.clone());
        let analysis = program
            .resource_analysis(&owner)
            .expect("resource analysis");

        let introduce = analysis
            .actions
            .iter()
            .find(|action| action.kind == CanonicalActionKind::Introduce)
            .expect("parameter introduction");
        let binding_move = analysis
            .actions
            .iter()
            .find(|action| {
                action.kind == CanonicalActionKind::Move
                    && action
                        .target
                        .as_ref()
                        .is_some_and(|place| place.display() == "moved")
            })
            .expect("binding move");
        let returns = analysis
            .actions
            .iter()
            .filter(|action| action.kind == CanonicalActionKind::Return)
            .collect::<Vec<_>>();

        assert_eq!(introduce.resource, expected);
        assert_eq!(binding_move.resource, expected);
        assert_eq!(returns.len(), 1, "nested block result must not return");
        assert_eq!(returns[0].resource, expected);
        assert_eq!(
            returns[0].source.as_ref().map(Place::display).as_deref(),
            Some("moved")
        );
    }

    #[test]
    fn typed_loans_keep_node_identity_place_precision_and_cfg_location() {
        // RESOURCE-LINEAR-001: canonical loans and places come from typed
        // nodes, retaining the distinction between constant and dynamic index.
        let file = parse(
            r#"
func inspect(xs: List<i32>, index: i32) -> i32 {
    let fixed = &xs[0]
    let dynamic = &xs[index]
    *fixed + *dynamic
}
func main() -> i32 { inspect([1, 2], 1) }
"#,
        );
        let program = crate::core::check_program(&file).expect("indexed loans check");
        let owner = NodeId("function:inspect".into());
        let analysis = program
            .resource_analysis(&owner)
            .expect("resource analysis");
        let cfg = program.callable_cfg(&owner).expect("callable CFG");
        let places = analysis
            .loans
            .iter()
            .map(|loan| loan.place.display())
            .collect::<BTreeSet<_>>();

        assert!(places.contains("xs[0]"));
        assert!(places.contains("xs[*]"));
        for loan in &analysis.loans {
            let action = analysis
                .actions
                .iter()
                .find(|action| action.loan.as_ref() == Some(&loan.id))
                .expect("loan action");
            assert_eq!(loan.id.0 .0, format!("{}/loan", action.location.point.0));
        }
        assert!(analysis
            .actions
            .iter()
            .all(|action| action_location_exists(cfg, action)));
    }

    #[test]
    fn canonical_return_gate_rejects_available_linear_resource() {
        // RESOURCE-LINEAR-001: return-path completeness belongs to the CFG
        // fixed point, not the legacy checker scope snapshots.
        let file = parse(
            r#"
cap Token
func leak(token: cap Token) -> i32 { 0 }
func main() -> i32 { 0 }
"#,
        );
        let errors = crate::core::check_program(&file)
            .expect_err("canonical return gate must reject the leak");
        assert!(errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::diagnostic::codes::E0256)
                && error.message
                    == "linear resource 'token' must be consumed before this return path"
        }));
    }

    #[test]
    fn map_root_checker_receipts_bind_new_set_size_and_drop_to_one_root_chain() {
        let file = parse(
            r#"
func main() -> i32 {
    let first = map_new()
    let updated = map_set(first, "answer", 42)
    let size = map_size(updated)
    drop(updated)
    size
}
"#,
        );
        let program = crate::core::check_program(&file).expect("closed Map-root lifecycle");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Map-root resource analysis");
        assert_eq!(
            analysis
                .map_root_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                MapRootActionKind::New,
                MapRootActionKind::Set,
                MapRootActionKind::Size,
                MapRootActionKind::Drop,
            ]
        );
        assert_eq!(
            analysis.map_root_actions[0].root,
            analysis.map_root_actions[1].source.clone().unwrap()
        );
        assert_eq!(
            analysis.map_root_actions[1].root,
            analysis.map_root_actions[2].root
        );
        assert_eq!(
            analysis.map_root_actions[2].root,
            analysis.map_root_actions[3].root
        );
        assert_eq!(
            analysis.map_root_actions[0].local,
            analysis.map_root_actions[1].source_local.clone().unwrap()
        );
        assert_eq!(analysis.map_root_actions[1].key.as_deref(), Some("answer"));
        assert_eq!(
            analysis.map_root_actions[1].local,
            analysis.map_root_actions[2].local
        );
        assert_eq!(
            analysis.map_root_actions[2].local,
            analysis.map_root_actions[3].local
        );
    }

    #[test]
    fn map_any_owner_v1_has_an_independent_receipt_for_the_same_closed_source() {
        let file = parse(
            r#"
func main() -> i32 {
    let first = map_new()
    let updated = map_set(first, "answer", 42)
    let size = map_size(updated)
    drop(updated)
    size
}
"#,
        );
        let program = crate::core::check_program(&file).expect("closed local Map lifecycle");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");

        assert_eq!(
            analysis
                .map_root_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                MapRootActionKind::New,
                MapRootActionKind::Set,
                MapRootActionKind::Size,
                MapRootActionKind::Drop,
            ]
        );
        assert_eq!(
            analysis
                .map_any_owner_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                MapAnyOwnerActionKind::New,
                MapAnyOwnerActionKind::Set,
                MapAnyOwnerActionKind::Size,
                MapAnyOwnerActionKind::Drop,
            ]
        );
        assert!(analysis
            .map_any_owner_actions
            .iter()
            .all(|action| { action.profile_version == MapAnyOwnerProfileVersion::V1 }));
        assert_eq!(analysis.map_root_new_attempts.len(), 1);
        assert_eq!(analysis.map_any_owner_new_attempts.len(), 1);
        assert_eq!(
            analysis.map_any_owner_new_attempts, analysis.map_root_new_attempts,
            "both independent passes must cover the same source construction"
        );

        let old_set = &analysis.map_root_actions[1];
        let new_set = &analysis.map_any_owner_actions[1];
        assert_eq!(old_set.point, new_set.point);
        assert_eq!(old_set.value_type, new_set.value_type);
        let payload_type = new_set.value_type.as_ref().expect("Set payload type");
        assert!(matches!(
            program.resolved_types().get(payload_type),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
        ));
        assert_ne!(old_set.root, new_set.root);
        assert!(new_set.root.0 .0.starts_with("map-any-owner-v1:"));
        assert_eq!(
            new_set.source.as_ref().unwrap().0 .0,
            format!(
                "map-any-owner-v1:{}",
                new_set.source_local.as_ref().unwrap().0 .0
            )
        );
    }

    #[test]
    fn map_any_owner_v1_withholds_dynamic_any_aggregate_and_mixed_payloads() {
        let aggregate = parse(
            r#"
type Point { x: i32 }
func main() -> i32 {
    let root = map_new()
    let updated = map_set(root, "point", Point { x: 7 })
    drop(updated)
    0
}
"#,
        );
        let program = crate::core::check_program(&aggregate)
            .expect("legacy DynamicAny aggregate Map remains checkable");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        assert_eq!(analysis.map_any_owner_new_attempts.len(), 1);
        assert!(analysis.map_any_owner_actions.is_empty());

        let mixed = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let integer_value = map_set(root, "answer", 42)
    let mixed_value = map_set(integer_value, "label", "forty-two")
    drop(mixed_value)
    0
}
"#,
        );
        let program = crate::core::check_program(&mixed)
            .expect("legacy mixed DynamicAny Map remains checkable");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        assert_eq!(analysis.map_any_owner_new_attempts.len(), 1);
        assert!(analysis.map_any_owner_actions.is_empty());
    }

    #[test]
    fn map_root_checker_contains_receipts_are_static_borrowing_and_fail_closed() {
        let accepted = parse(
            r#"
func main() -> i32 {
    let first = map_new()
    let root = map_set(first, "answer", 42)
    let present = has_key(root, "answer")
    let missing = has_key(root, "missing")
    drop(root)
    0
}
"#,
        );
        let program = crate::core::check_program(&accepted).expect("static MapRoot contains");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("MapRoot resource analysis");
        assert_eq!(
            analysis
                .map_root_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                MapRootActionKind::New,
                MapRootActionKind::Set,
                MapRootActionKind::Contains,
                MapRootActionKind::Contains,
                MapRootActionKind::Drop,
            ]
        );
        let set = &analysis.map_root_actions[1];
        let contains_present = &analysis.map_root_actions[2];
        let contains_missing = &analysis.map_root_actions[3];
        let drop = &analysis.map_root_actions[4];
        assert_eq!(contains_present.key.as_deref(), Some("answer"));
        assert_eq!(contains_missing.key.as_deref(), Some("missing"));
        assert_eq!(contains_present.root, set.root);
        assert_eq!(contains_missing.root, set.root);
        assert_eq!(drop.root, set.root);
        assert_eq!(contains_present.local, set.local);
        assert_eq!(contains_missing.local, set.local);
        assert_eq!(drop.local, set.local);
        assert!(contains_present.value.is_none());
        assert!(contains_present.source.is_none());
        assert!(contains_present.value_type.is_none());

        let dynamic_key = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let key = "answer"
    let present = has_key(root, key)
    drop(root)
    0
}
"#,
        );
        let program = crate::core::check_program(&dynamic_key).expect("legacy Map stays checkable");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("MapRoot resource analysis");
        assert!(analysis.map_root_actions.is_empty());
        assert_eq!(analysis.map_root_new_attempts.len(), 1);

        let nul_key = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let present = has_key(root, "bad\0key")
    drop(root)
    0
}
"#,
        );
        match crate::core::check_program(&nul_key) {
            Ok(program) => {
                let analysis = program
                    .resource_analysis(&NodeId("function:main".into()))
                    .expect("MapRoot resource analysis");
                assert!(analysis
                    .map_root_actions
                    .iter()
                    .all(|action| action.kind != MapRootActionKind::Contains));
            }
            Err(errors) => assert!(!errors.is_empty(), "NUL key is rejected during checking"),
        }
    }

    #[test]
    fn map_root_remove_checker_receipts_consume_and_reissue_one_root_chain() {
        let file = parse(
            r#"
func main() -> i32 {
    let first = map_new()
    let inserted = map_set(first, "remove", "payload")
    let with_keep = map_set(inserted, "keep", "stay")
    let removed = map_remove(with_keep, "remove")
    let size = map_size(removed)
    drop(removed)
    size
}
"#,
        );
        let program = crate::core::check_program(&file).expect("closed Map-root Remove lifecycle");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Map-root resource analysis");
        let actions = &analysis.map_root_actions;
        assert_eq!(
            actions.iter().map(|action| action.kind).collect::<Vec<_>>(),
            [
                MapRootActionKind::New,
                MapRootActionKind::Set,
                MapRootActionKind::Set,
                MapRootActionKind::Remove,
                MapRootActionKind::Size,
                MapRootActionKind::Drop,
            ]
        );
        assert_eq!(actions[3].key.as_deref(), Some("remove"));
        assert_eq!(actions[3].source.as_ref(), Some(&actions[2].root));
        assert_eq!(actions[3].source_local.as_ref(), Some(&actions[2].local));
        assert_eq!(actions[3].root, actions[4].root);
        assert_eq!(actions[3].local, actions[4].local);
        assert_eq!(actions[4].root, actions[5].root);

        let temporary = parse(
            r#"
func main() -> i32 {
    let removed = map_remove(map_new(), "missing")
    let size = map_size(removed)
    drop(removed)
    size
}
"#,
        );
        let program = crate::core::check_program(&temporary)
            .expect("temporary MapRoot source is consumed by Remove");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("temporary Map-root analysis");
        assert_eq!(
            analysis
                .map_root_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                MapRootActionKind::New,
                MapRootActionKind::Remove,
                MapRootActionKind::Size,
                MapRootActionKind::Drop,
            ]
        );
        assert_eq!(
            analysis.map_root_actions[0].root,
            analysis.map_root_actions[1].source.clone().unwrap()
        );
    }

    #[test]
    fn map_root_remove_dynamic_key_and_reuse_withhold_canonical_receipts() {
        for source in [
            r#"
func main() -> i32 {
    let root = map_new()
    let inserted = map_set(root, "answer", 42)
    let key = "answer"
    let removed = map_remove(inserted, key)
    drop(removed)
    0
}
"#,
            r#"
func main() -> i32 {
    let root = map_new()
    let inserted = map_set(root, "answer", 42)
    let removed = map_remove(inserted, "answer")
    let again = map_remove(inserted, "answer")
    let size = map_size(again)
    drop(removed)
    drop(again)
    size
}
"#,
        ] {
            let file = parse(source);
            let program = crate::core::check_program(&file)
                .expect("unsupported persistent Map source remains type-correct");
            let analysis = program
                .resource_analysis(&NodeId("function:main".into()))
                .expect("Map-root analysis");
            assert!(
                analysis.map_root_actions.is_empty(),
                "unsupported Remove shape must withhold the entire ownership receipt"
            );
            assert_eq!(
                crate::core::mir::classify_canonical_mir_route_admission(&program).map_root,
                crate::core::mir::MapRootAdmission::OutsideProfile,
                "unsupported persistent Map usage must remain on the compatibility route"
            );
        }
    }

    #[test]
    fn map_root_checker_receipts_keep_interleaved_roots_and_utf8_keys_distinct() {
        let file = parse(
            r#"
func main() -> i32 {
    let left = map_new()
    let right = map_new()
    let left_updated = map_set(left, "café🧪", 1 + 41)
    let right_updated = map_set(right, "🗺️", map_size(left_updated))
    let right_size = map_size(right_updated)
    drop(left_updated)
    drop(right_updated)
    right_size
}
"#,
        );
        let program = crate::core::check_program(&file).expect("closed Map-root lifecycles");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Map-root resource analysis");
        let actions = &analysis.map_root_actions;
        assert_eq!(
            actions.iter().map(|action| action.kind).collect::<Vec<_>>(),
            [
                MapRootActionKind::New,
                MapRootActionKind::New,
                MapRootActionKind::Set,
                MapRootActionKind::Size,
                MapRootActionKind::Set,
                MapRootActionKind::Size,
                MapRootActionKind::Drop,
                MapRootActionKind::Drop,
            ]
        );

        let left_root = &actions[0].root;
        let right_root = &actions[1].root;
        assert_ne!(left_root, right_root);
        assert_eq!(actions[2].source.as_ref(), Some(left_root));
        assert_eq!(actions[2].key.as_deref(), Some("café🧪"));
        assert_eq!(actions[3].root, actions[2].root);
        assert_eq!(actions[4].source.as_ref(), Some(right_root));
        assert_eq!(actions[4].key.as_deref(), Some("🗺️"));
        assert_eq!(actions[5].root, actions[4].root);
        assert_eq!(actions[6].root, actions[2].root);
        assert_eq!(actions[7].root, actions[4].root);
        assert_eq!(actions[2].local, actions[3].local);
        assert_eq!(actions[4].local, actions[5].local);
    }

    #[test]
    fn map_root_checker_withholds_mixed_payload_type_chain() {
        for source in [
            r#"
func main() -> i32 {
    let root = map_new()
    let integer_value = map_set(root, "answer", 42)
    let mixed_value = map_set(integer_value, "label", "forty-two")
    drop(mixed_value)
    0
}
"#,
            r#"
func main() -> i32 {
    let root = map_new()
    let string_value = map_set(root, "label", "forty-two")
    let mixed_value = map_set(string_value, "answer", 42)
    drop(mixed_value)
    0
}
"#,
        ] {
            let file = parse(source);
            let program = crate::core::check_program(&file)
                .expect("legacy Any Map semantics remain available for mixed payloads");
            let analysis = program
                .resource_analysis(&NodeId("function:main".into()))
                .expect("MapRoot resource analysis");
            assert_eq!(analysis.map_root_new_attempts.len(), 1);
            assert!(
                analysis.map_root_actions.is_empty(),
                "a root chain that changes payload TypeDesc must not receive partial MapRoot authority"
            );
            assert!(
                analysis.map_any_owner_actions.is_empty(),
                "a root chain that changes payload TypeDesc must not receive partial MapAnyOwner v1 authority"
            );
        }
    }

    #[test]
    fn map_root_checker_receipts_cover_and_consume_temporary_roots() {
        let immediate_observation = parse(
            r#"
func main() -> i32 {
    map_size(map_new())
}
"#,
        );
        let program = crate::core::check_program(&immediate_observation)
            .expect("MapRoot temporary observation checks");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("MapRoot resource analysis");
        assert_eq!(analysis.map_root_new_attempts.len(), 1);
        assert_eq!(
            analysis
                .map_root_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                MapRootActionKind::New,
                MapRootActionKind::Size,
                MapRootActionKind::Drop,
            ]
        );
        assert_eq!(
            analysis.map_root_actions[0].root,
            analysis.map_root_actions[1].root
        );
        assert_eq!(
            analysis.map_root_actions[1].root,
            analysis.map_root_actions[2].root
        );

        let temporary_update = parse(
            r#"
func main() -> i32 {
    let updated = map_set(map_new(), "answer", 42)
    let size = map_size(updated)
    drop(updated)
    size
}
"#,
        );
        let program = crate::core::check_program(&temporary_update)
            .expect("temporary MapRoot source is consumed by Set");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("MapRoot resource analysis");
        assert_eq!(analysis.map_root_new_attempts.len(), 1);
        assert_eq!(
            analysis
                .map_root_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                MapRootActionKind::New,
                MapRootActionKind::Set,
                MapRootActionKind::Size,
                MapRootActionKind::Drop,
            ]
        );
        assert_eq!(
            analysis.map_root_actions[0].root,
            analysis.map_root_actions[1].source.clone().unwrap()
        );
        assert_eq!(
            analysis.map_root_actions[2].root,
            analysis.map_root_actions[3].root
        );
    }

    #[test]
    fn map_root_checker_withholds_receipt_for_missing_drop_and_alias_copy() {
        let missing_drop = parse(
            r#"
func main() -> i32 {
    let map = map_new()
    map_size(map)
}
"#,
        );
        let program = crate::core::check_program(&missing_drop)
            .expect("legacy Record semantics remain available");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Map-root resource analysis");
        assert_eq!(analysis.map_root_new_attempts.len(), 1);
        assert!(analysis.map_root_actions.is_empty());

        let nested_temporary = parse(
            r#"
func main() -> i32 {
    let size = map_size(map_new())
    size
}
"#,
        );
        let program = crate::core::check_program(&nested_temporary)
            .expect("legacy temporary Map semantics remain available to checking");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Map-root resource analysis");
        assert_eq!(analysis.map_root_new_attempts.len(), 1);
        assert_eq!(
            analysis
                .map_root_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                MapRootActionKind::New,
                MapRootActionKind::Size,
                MapRootActionKind::Drop,
            ]
        );

        let mixed_lifecycle_and_temporary = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let updated = map_set(root, "answer", 42)
    let size = map_size(updated)
    let temporary_size = map_size(map_new())
    let unreceipted = map_get(map_new(), "missing")
    drop(updated)
    size + temporary_size
}
"#,
        );
        let program = crate::core::check_program(&mixed_lifecycle_and_temporary)
            .expect("legacy temporary Map semantics remain available to checking");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Map-root resource analysis");
        assert_eq!(analysis.map_root_new_attempts.len(), 3);
        assert!(
            analysis
                .map_root_actions
                .iter()
                .any(|action| action.kind == MapRootActionKind::New),
            "the complete lifecycle must not mask the second temporary MapRoot"
        );
        assert_eq!(
            analysis
                .map_root_actions
                .iter()
                .filter(|action| action.kind == MapRootActionKind::New)
                .count(),
            2,
            "the third map_new passed to borrowed map_get must remain unreceipted"
        );

        let alias_copy = parse(
            r#"
func main() -> i32 {
    let map = map_new()
    let alias = map
    drop(alias)
    0
}
"#,
        );
        let program = crate::core::check_program(&alias_copy)
            .expect("legacy Record aliases remain available");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Map-root resource analysis");
        assert!(analysis.map_root_actions.is_empty());
    }

    #[test]
    fn map_root_checker_withholds_receipt_for_unreceipted_operations_and_stale_reads() {
        let file = parse(
            r#"
func main() -> i32 {
    let map = map_new()
    let value = map_get(map, "answer")
    drop(map)
    0
}
"#,
        );
        let program = crate::core::check_program(&file).expect("legacy Map API remains available");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Map-root resource analysis");
        assert!(analysis.map_root_actions.is_empty());

        for source in [
            r#"
func main() -> i32 {
    let old = map_new()
    let updated = map_set(old, "answer", 42)
    let stale_size = map_size(old)
    drop(updated)
    stale_size
}
"#,
            r#"
func payload() -> string { "computed" }
func main() -> i32 {
    let map = map_new()
    let updated = map_set(map, "answer", payload())
    drop(updated)
    0
}
"#,
            r#"
func main() -> i32 {
    let map = map_new()
    drop(map)
    map_size(map)
}
"#,
            r#"
func main() -> i32 {
    let map = map_new()
    let pair = (map, 0)
    drop(map)
    0
}
"#,
            r#"
func main() -> i32 {
    let map = map_new()
    let size_of_map = fn() -> i32 { map_size(map) }
    drop(map)
    0
}
"#,
            r#"
func main() -> i32 {
    let map = map_new()
    let updated = map_set(map, "answer", "not-" + "i32")
    drop(updated)
    0
}
"#,
            r#"
func main() -> i32 {
    let map = map_new()
    let key = "answer"
    let updated = map_set(map, key, 42)
    drop(updated)
    0
}
"#,
            r#"
func main() -> i32 {
    let map = map_new()
    let updated = map_set(map, "before\0after", 42)
    drop(updated)
    0
}
"#,
            r#"
func main() -> i32 {
    let map = map_new()
    map_set(map, "answer", 42)
    drop(map)
    0
}
"#,
            r#"
func main() -> i32 {
    let map = map_new()
    if true {
        let n = map_size(map)
    } else {
    }
    drop(map)
    0
}
"#,
            r#"
func main() -> i32 {
    let map = map_new()
    drop(map)
    drop(map)
    0
}
"#,
        ] {
            let file = parse(source);
            let program = crate::core::check_program(&file)
                .expect("legacy Record behavior remains available");
            let analysis = program
                .resource_analysis(&NodeId("function:main".into()))
                .expect("Map-root resource analysis");
            assert!(analysis.map_root_actions.is_empty());
        }
    }

    #[test]
    fn linear_lambda_capture_transfers_resource_to_child() {
        // RESOURCE-LINEAR-001 + T-2 (audit 2026-08-05 wave-2): closure
        // capture of a LINEAR resource (cap/session/flow state) is rejected
        // at the checker with E0427 — a closure can be invoked more than
        // once, so a captured capability would be consumed on EVERY call,
        // escaping exactly-once enforcement. The old (pre-T-2) behavior
        // TransferChild'd the token at construction time and let the child
        // drop it, which double-consumes on a twice-called lambda.
        // The resource analysis still supports TransferChild for non-linear
        // captures; this test now pins the checker-level rejection.
        let file = parse(
            r#"
cap Token
func capture(token: cap Token) -> i32 {
    let child = fn() -> i32 { drop(token); 0 }
    0
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file)
            .expect_err("closure capture of a linear capability must be rejected (E0427)");
        assert!(
            program
                .iter()
                .any(|error| error.code.as_deref() == Some(crate::diagnostic::codes::E0427)),
            "expected E0427 for linear capture, got: {program:?}"
        );
    }

    #[test]
    fn sd1_flow_state_tracked_via_structural_flag() {
        // SD-1: flow state types are linear via the structural is_linear flag
        // on ResolvedType::Nominal, not via starts_with("state:") string matching.
        // Verify that a capability parameter is tracked as a linear resource
        // through the full check → resolve → resource_lower pipeline.
        let file = parse(
            r#"
cap Token
func process(token: cap Token) -> cap Token { token }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("cap program checks");
        let owner = NodeId("function:process".into());

        // Verify the resolved type has is_linear set structurally
        let body = program.resolved_body(&owner).expect("process body");
        let token_local = body
            .locals
            .values()
            .find(|local| local.display_name == "token")
            .expect("token local");
        let token_ty = program.resolved_types().get(&token_local.ty);
        // Capability types are tracked as linear (separate variant)
        assert!(
            matches!(token_ty, Some(crate::core::ResolvedType::Capability(_))),
            "cap Token must resolve to Capability variant"
        );

        // Verify resource analysis tracks it
        let analysis = program
            .resource_analysis(&owner)
            .expect("process resource analysis");
        assert!(
            analysis
                .actions
                .iter()
                .any(|action| action.kind == CanonicalActionKind::Introduce),
            "capability parameter must be tracked as linear resource"
        );
        assert!(
            analysis
                .actions
                .iter()
                .any(|action| action.kind == CanonicalActionKind::Return),
            "capability parameter must be returned"
        );
    }

    #[test]
    fn ordinary_map_owner_v1_keeps_mixed_source_roots_live_for_borrow_and_drop() {
        let file = parse(
            r#"
func main() -> i32 {
    let source = map_new()
    let number = 42
    let first = map_set(source, "number", number)
    let text = "forty-two"
    let second = map_set(first, "text", text)
    let size = map_size(source)
    let contains = has_key(second, "text")
    drop(source)
    drop(first)
    drop(second)
    size
}
"#,
        );
        let program = crate::core::check_program(&file).expect("mixed ordinary Map lifecycle");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");

        assert!(analysis.has_ordinary_map_owner_lifecycle_v1_receipt());
        assert_eq!(
            analysis.ordinary_map_owner_lifecycle_v1_new_attempts.len(),
            1
        );
        assert_eq!(
            analysis
                .ordinary_map_owner_lifecycle_v1_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                OrdinaryMapOwnerLifecycleActionKind::New,
                OrdinaryMapOwnerLifecycleActionKind::Set,
                OrdinaryMapOwnerLifecycleActionKind::Set,
                OrdinaryMapOwnerLifecycleActionKind::Size,
                OrdinaryMapOwnerLifecycleActionKind::HasKey,
                OrdinaryMapOwnerLifecycleActionKind::Drop,
                OrdinaryMapOwnerLifecycleActionKind::Drop,
                OrdinaryMapOwnerLifecycleActionKind::Drop,
            ]
        );

        let actions = &analysis.ordinary_map_owner_lifecycle_v1_actions;
        assert!(actions.iter().all(|action| {
            action.profile_version == OrdinaryMapOwnerLifecycleProfileVersion::V1
                && action
                    .root
                    .0
                     .0
                    .starts_with("ordinary-map-owner-lifecycle-i32-string-v1:")
        }));
        assert_eq!(actions[1].source_local.as_ref(), Some(&actions[0].local));
        assert_eq!(actions[1].source.as_ref(), Some(&actions[0].root));
        assert_eq!(actions[2].source_local.as_ref(), Some(&actions[1].local));
        assert_eq!(actions[3].local, actions[0].local);
        assert_eq!(
            actions[1]
                .key_type
                .as_ref()
                .and_then(|ty| program.resolved_types().get(ty)),
            Some(&ResolvedType::Primitive(crate::core::PrimitiveType::String))
        );
        assert!(matches!(
            actions[1]
                .value_type
                .as_ref()
                .and_then(|ty| program.resolved_types().get(ty)),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
        ));
        assert!(matches!(
            actions[2]
                .value_type
                .as_ref()
                .and_then(|ty| program.resolved_types().get(ty)),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
        ));
        assert!(actions[1].key.is_some() && actions[2].key.is_some());
    }

    #[test]
    fn ordinary_map_owner_v1_does_not_record_list_map_as_map_attempt() {
        let list_only = parse(
            r#"
func main() -> i32 {
    let mapped = map([1, 2], fn(x: i32) -> i32 { x + 1 })
    mapped[0]
}
"#,
        );
        let list_program = crate::core::check_program(&list_only)
            .expect("list map intrinsic without ordinary Map must check");
        let list_analysis = list_program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("list-only resource analysis");
        assert!(
            list_analysis
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .is_empty(),
            "the list `map([..], f)` intrinsic alone must not create ordinary Map attempts"
        );
        assert!(list_analysis
            .ordinary_map_owned_get_i32_string_v1_operation_attempts
            .is_empty());
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&list_program)
                .ordinary_map_owner_lifecycle_i32_string_v1,
            crate::core::mir::OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile,
            "the list `map([..], f)` intrinsic must not select an ordinary Map route"
        );
        assert!(
            !crate::core::mir::classify_canonical_mir_route_admission(&list_program)
                .ordinary_map_owner_lifecycle_i32_string_v1
                .has_profile_signal(),
            "a list-only `map([..], f)` intrinsic must not create an ordinary Map profile signal"
        );

        let file = parse(
            r#"
func main() -> i32 {
    let mapped = map([1, 2], fn(x: i32) -> i32 { x + 1 })
    let root = map_new()
    let updated = map_set(root, "mapped", 42)
    drop(root)
    drop(updated)
    mapped[0]
}
"#,
        );
        let program = crate::core::check_program(&file)
            .expect("list map intrinsic and ordinary Map lifecycle both check");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        let attempts = &analysis.ordinary_map_owner_lifecycle_v1_operation_attempts;

        assert_eq!(
            attempts.len(),
            4,
            "only map_new, map_set, and their two drops are ordinary Map attempts; the list `map([..], f)` intrinsic must not be recorded: {attempts:?}"
        );
        assert_eq!(
            list_analysis
                .ordinary_map_owned_get_i32_string_v1_operation_attempts
                .len(),
            0,
            "the owned-get scanner must also exclude the bare List.map builtin"
        );
        assert!(
            attempts
                .iter()
                .any(|attempt| { attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::New }),
            "ordinary map_new must remain visible in the attempt ledger"
        );
        assert!(
            attempts
                .iter()
                .any(|attempt| { attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::Set }),
            "ordinary map_set must remain visible in the attempt ledger"
        );
        assert!(
            attempts
                .iter()
                .any(|attempt| { attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::Drop }),
            "ordinary Map drops must remain visible in the attempt ledger"
        );
        assert!(
            analysis.has_ordinary_map_owner_lifecycle_v1_receipt(),
            "an unrelated list `map([..], f)` intrinsic must not invalidate a complete ordinary Map receipt"
        );
        assert!(
            crate::core::mir::classify_canonical_mir_route_admission(&program)
                .ordinary_map_owner_lifecycle_i32_string_v1_complete(),
            "the mixed list/ordinary Map callable must retain complete ordinary Map admission"
        );
    }

    #[test]
    fn ordinary_map_owner_v1_rejects_list_map_callback_capturing_map_root() {
        let file = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let mapped = map([1, 2], fn(x: i32) -> i32 { map_size(root) + x })
    drop(root)
    mapped[0]
}
"#,
        );
        let program = crate::core::check_program(&file)
            .expect("list-map callback capturing a Map root must remain type-correct");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        assert!(
            analysis.ordinary_map_owner_lifecycle_v1_actions.is_empty(),
            "a list-map callback that captures and queries an ordinary Map root must withhold the receipt"
        );
        assert!(analysis
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::Size));
        assert!(
            !crate::core::mir::classify_canonical_mir_route_admission(&program)
                .ordinary_map_owner_lifecycle_i32_string_v1_complete(),
            "a captured Map operation inside a list-map callback must not receive complete ordinary Map admission"
        );
    }

    #[test]
    fn ordinary_map_owner_v1_withholds_same_source_branch_forks() {
        let file = parse(
            r#"
func forked(flag: bool) -> i32 {
    let source = map_new()
    if flag {
        let left = map_set(source, "left", 1)
        drop(left)
    } else {
        let right = map_set(source, "right", 2)
        drop(right)
    }
    drop(source)
    0
}
func main() -> i32 { forked(true) }
"#,
        );
        let program = crate::core::check_program(&file).expect("branch Map program checks");
        let analysis = program
            .resource_analysis(&NodeId("function:forked".into()))
            .expect("resource analysis");
        assert!(analysis.ordinary_map_owner_lifecycle_v1_actions.is_empty());
        assert_eq!(
            analysis.ordinary_map_owner_lifecycle_v1_new_attempts.len(),
            1
        );
        assert_eq!(
            analysis
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .iter()
                .filter(|attempt| { attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::Set })
                .count(),
            2,
            "the independent attempt ledger must preserve both branch updates"
        );
    }

    #[test]
    fn ordinary_map_owner_v1_accepts_branch_read_only_size_with_join_drop() {
        let file = parse(
            r#"
func branch_size(flag: bool) -> i32 {
    let root = map_new()
    if flag {
        let size = map_size(root)
    } else {
        let size = map_size(root)
    }
    drop(root)
    0
}
func main() -> i32 { branch_size(true) }
"#,
        );
        let program = crate::core::check_program(&file).expect("branch Map size program checks");
        let analysis = program
            .resource_analysis(&NodeId("function:branch_size".into()))
            .expect("resource analysis");
        assert!(
            analysis.has_ordinary_map_owner_lifecycle_v1_receipt(),
            "read-only branch observations keep one shared root receipt"
        );
        assert_eq!(
            analysis
                .ordinary_map_owner_lifecycle_v1_actions
                .iter()
                .filter(|action| action.kind == OrdinaryMapOwnerLifecycleActionKind::Size)
                .count(),
            2,
            "both mutually exclusive branch sizes must be receipted"
        );
        assert!(
            crate::core::mir::classify_canonical_mir_route_admission(&program)
                .ordinary_map_owner_lifecycle_i32_string_v1_complete(),
            "branch read-only Map size must be complete canonical admission"
        );
        let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
            .expect("branch read-only Map size lowers to canonical MIR");
        mir.functions()
            .values()
            .find(|function| function.owner == NodeId("function:branch_size".into()))
            .expect("branch_size MIR")
            .validate()
            .expect("branch read-only Map lifecycle validates path-independent receipts");
    }

    #[test]
    fn ordinary_map_owner_v1_accepts_iteration_local_new_drop_loop() {
        let file = parse(
            r#"
func loop_local(flag: bool) -> i32 {
    while flag {
        let root = map_new()
        drop(root)
    }
    0
}
func main() -> i32 { loop_local(false) }
"#,
        );
        let program = crate::core::check_program(&file).expect("loop-local Map program checks");
        let owner = NodeId("function:loop_local".into());
        let analysis = program
            .resource_analysis(&owner)
            .expect("loop-local resource analysis");
        assert_eq!(
            analysis
                .ordinary_map_owner_lifecycle_v1_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                OrdinaryMapOwnerLifecycleActionKind::New,
                OrdinaryMapOwnerLifecycleActionKind::Drop,
            ]
        );
        assert!(
            crate::core::mir::classify_canonical_mir_route_admission(&program)
                .ordinary_map_owner_lifecycle_i32_string_v1_complete(),
            "an iteration-local New/Drop loop must retain complete ordinary Map admission"
        );
        let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
            .expect("loop-local ordinary Map lowers to canonical MIR");
        mir.functions()
            .values()
            .find(|function| function.owner == owner)
            .expect("loop_local MIR")
            .validate()
            .expect("loop-local ordinary Map lifecycle validates its back-edge");
    }

    #[test]
    fn ordinary_map_owner_v1_accepts_iteration_local_new_drop_break_loop() {
        let file = parse(
            r#"
func loop_local_break() -> i32 {
    loop {
        let root = map_new()
        drop(root)
        break
    }
    0
}
func main() -> i32 { loop_local_break() }
"#,
        );
        let program = crate::core::check_program(&file).expect("finite loop Map program checks");
        let owner = NodeId("function:loop_local_break".into());
        let analysis = program
            .resource_analysis(&owner)
            .expect("finite loop resource analysis");
        assert_eq!(
            analysis
                .ordinary_map_owner_lifecycle_v1_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                OrdinaryMapOwnerLifecycleActionKind::New,
                OrdinaryMapOwnerLifecycleActionKind::Drop,
            ]
        );
        assert!(
            crate::core::mir::classify_canonical_mir_route_admission(&program)
                .ordinary_map_owner_lifecycle_i32_string_v1_complete(),
            "finite loop New/Drop must retain complete ordinary Map admission"
        );
        let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
            .expect("finite loop ordinary Map lowers to canonical MIR");
        mir.functions()
            .values()
            .find(|function| function.owner == owner)
            .expect("loop_local_break MIR")
            .validate()
            .expect("finite loop ordinary Map lifecycle validates its break edge");
    }

    #[test]
    fn ordinary_map_owner_v1_rejects_roots_crossing_loop_boundaries() {
        let file = parse(
            r#"
func loop_escape(flag: bool) -> i32 {
    let root = map_new()
    while flag {
        let size = map_size(root)
    }
    drop(root)
    0
}
func main() -> i32 { loop_escape(false) }
"#,
        );
        let program = crate::core::check_program(&file).expect("loop escape remains type-correct");
        let owner = NodeId("function:loop_escape".into());
        let analysis = program
            .resource_analysis(&owner)
            .expect("loop escape resource analysis");
        assert!(analysis.ordinary_map_owner_lifecycle_v1_actions.is_empty());
        assert!(analysis
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::Size));
        assert!(
            !crate::core::mir::classify_canonical_mir_route_admission(&program)
                .ordinary_map_owner_lifecycle_i32_string_v1_complete()
        );
    }

    #[test]
    fn ordinary_map_owner_v1_accepts_dynamic_typed_string_keys() {
        let file = parse(
            r#"
func main() -> i32 {
    let key = "pre" + "fix"
    let source = map_new()
    let updated = map_set(source, key, 7)
    let removed = map_remove(updated, key)
    let present = has_key(source, key)
    drop(source)
    drop(updated)
    drop(removed)
    0
}
"#,
        );
        let program = crate::core::check_program(&file).expect("dynamic key Map lifecycle");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        assert!(analysis.has_ordinary_map_owner_lifecycle_v1_receipt());
        let actions = &analysis.ordinary_map_owner_lifecycle_v1_actions;
        let set = actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnerLifecycleActionKind::Set)
            .expect("Set action");
        let remove = actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnerLifecycleActionKind::Remove)
            .expect("Remove action");
        assert_eq!(set.key_type, remove.key_type);
        assert_ne!(
            set.key.as_ref(),
            None,
            "the key expression identity must be retained"
        );
        assert!(matches!(
            set.key_type
                .as_ref()
                .and_then(|ty| program.resolved_types().get(ty)),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
        ));
    }

    #[test]
    fn ordinary_map_owner_v1_get_and_values_use_the_owned_get_profile() {
        for (operation, body, expected_kind, owned_kind) in [
            (
                "get",
                r#"let result = map_get(root, "missing"); drop(result)"#,
                OrdinaryMapOwnerLifecycleAttemptKind::Get,
                OrdinaryMapOwnedGetI32StringV1ActionKind::GetAny,
            ),
            (
                "values",
                r#"let result = values(root); drop(result)"#,
                OrdinaryMapOwnerLifecycleAttemptKind::Values,
                OrdinaryMapOwnedGetI32StringV1ActionKind::Values,
            ),
        ] {
            let source =
                format!("func main() -> i32 {{ let root = map_new(); {body}; drop(root); 0 }}");
            let file = parse(&source);
            let program = crate::core::check_program(&file)
                .unwrap_or_else(|errors| panic!("{operation} Map example must check: {errors:?}"));
            let analysis = program
                .resource_analysis(&NodeId("function:main".into()))
                .expect("resource analysis");
            assert!(
                analysis.ordinary_map_owner_lifecycle_v1_actions.is_empty(),
                "{operation} belongs to the independent owned-get profile"
            );
            assert!(analysis
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .iter()
                .any(|attempt| attempt.kind == expected_kind));
            assert!(analysis
                .ordinary_map_owned_get_i32_string_v1_actions
                .iter()
                .any(|action| action.kind == owned_kind));
            assert!(
                analysis.has_ordinary_map_owned_get_i32_string_v1_receipt(),
                "{operation} must retain an owned-get receipt"
            );
        }
    }

    #[test]
    fn ordinary_map_owner_v1_requires_each_root_to_be_dropped_exactly_once() {
        let file = parse("func main() -> i32 { let root = map_new(); 0 }");
        let checked =
            crate::core::check_program(&file).expect("unused ordinary root is scope-owned");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("analysis");
        assert!(analysis.has_ordinary_map_owner_lifecycle_v1_receipt());
        assert_eq!(
            analysis
                .ordinary_map_owner_lifecycle_v1_actions
                .iter()
                .filter(|action| action.kind == OrdinaryMapOwnerLifecycleActionKind::Drop)
                .count(),
            1
        );
        assert!(analysis
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::ScopeDrop));
        {
            let (name, body) = (
                "duplicate",
                "let root = map_new(); drop(root); drop(root); 0",
            );
            let source = format!("func main() -> i32 {{ {body} }}");
            let file = parse(&source);
            let program = crate::core::check_program(&file).unwrap_or_else(|errors| {
                panic!("{name} Drop example must remain type-correct: {errors:?}")
            });
            let analysis = program
                .resource_analysis(&NodeId("function:main".into()))
                .expect("resource analysis");
            assert!(analysis.ordinary_map_owner_lifecycle_v1_actions.is_empty());
            assert_eq!(
                analysis.ordinary_map_owner_lifecycle_v1_new_attempts.len(),
                1
            );
        }
    }

    #[test]
    fn ordinary_map_owner_v1_keeps_all_map_new_attempts_visible() {
        let file = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let size = map_size(root)
    let temporary_size = map_size(map_new())
    drop(root)
    size + temporary_size
}
"#,
        );
        let program = crate::core::check_program(&file).expect("temporary Map program checks");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        assert_eq!(
            analysis.ordinary_map_owner_lifecycle_v1_new_attempts.len(),
            2
        );
        assert_eq!(
            analysis
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .iter()
                .filter(|attempt| attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::New)
                .count(),
            2
        );
        assert!(analysis.ordinary_map_owner_lifecycle_v1_actions.is_empty());
    }

    #[test]
    fn ordinary_map_owned_get_i32_string_v1_records_typed_getters_and_dynamic_nul_key() {
        let file = parse(
            r#"
func main() -> i32 {
    let key = "prefix" + "\0suffix"
    let source = map_new()
    let inserted = map_set(source, key, 17)
    let string_key = key + ":text"
    let inserted_string = map_set(inserted, string_key, "owned")
    let removed = map_remove(inserted_string, key)
    let size = map_size(source)
    let present = has_key(inserted_string, string_key)
    let integer = map_get_i32(source, key)
    let text = map_get_string(removed, string_key)
    drop(source)
    drop(inserted)
    drop(inserted_string)
    drop(removed)
    size
}
"#,
        );
        let program = crate::core::check_program(&file)
            .expect("typed owned-get Map lifecycle with a dynamic NUL key");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");

        assert!(analysis.has_ordinary_map_owned_get_i32_string_v1_receipt());
        assert!(!analysis
            .ordinary_map_owned_get_i32_string_v1_operation_attempts
            .is_empty());
        assert_eq!(
            analysis
                .ordinary_map_owned_get_i32_string_v1_actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            [
                OrdinaryMapOwnedGetI32StringV1ActionKind::New,
                OrdinaryMapOwnedGetI32StringV1ActionKind::Set,
                OrdinaryMapOwnedGetI32StringV1ActionKind::Set,
                OrdinaryMapOwnedGetI32StringV1ActionKind::Remove,
                OrdinaryMapOwnedGetI32StringV1ActionKind::Size,
                OrdinaryMapOwnedGetI32StringV1ActionKind::HasKey,
                OrdinaryMapOwnedGetI32StringV1ActionKind::GetI32,
                OrdinaryMapOwnedGetI32StringV1ActionKind::GetString,
                OrdinaryMapOwnedGetI32StringV1ActionKind::Drop,
                OrdinaryMapOwnedGetI32StringV1ActionKind::Drop,
                OrdinaryMapOwnedGetI32StringV1ActionKind::Drop,
                OrdinaryMapOwnedGetI32StringV1ActionKind::Drop,
            ]
        );
        let actions = &analysis.ordinary_map_owned_get_i32_string_v1_actions;
        assert!(actions.iter().all(|action| {
            action.profile_version == OrdinaryMapOwnedGetI32StringV1ProfileVersion::V1
                && action
                    .root
                    .0
                     .0
                    .starts_with("ordinary-map-owned-get-i32-string-v1:")
        }));
        assert_eq!(actions[1].source_local.as_ref(), Some(&actions[0].local));
        assert_eq!(actions[2].source_local.as_ref(), Some(&actions[1].local));
        assert_eq!(actions[3].source_local.as_ref(), Some(&actions[2].local));
        assert_eq!(actions[4].local, actions[0].local);
        assert_eq!(actions[5].local, actions[2].local);
        for action in [
            &actions[1],
            &actions[2],
            &actions[3],
            &actions[5],
            &actions[6],
            &actions[7],
        ] {
            assert!(
                action.key.is_some(),
                "the complete key expression id is retained"
            );
            assert!(matches!(
                action
                    .key_type
                    .as_ref()
                    .and_then(|ty| program.resolved_types().get(ty)),
                Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
            ));
        }
        let get_i32 = actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedGetI32StringV1ActionKind::GetI32)
            .expect("typed i32 getter action");
        let get_string = actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedGetI32StringV1ActionKind::GetString)
            .expect("typed String getter action");
        assert!(matches!(
            actions[1]
                .value_type
                .as_ref()
                .and_then(|ty| program.resolved_types().get(ty)),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
        ));
        assert!(matches!(
            actions[2]
                .value_type
                .as_ref()
                .and_then(|ty| program.resolved_types().get(ty)),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
        ));
        let result_has_shape = |type_id: &ResolvedTypeId, ok: crate::core::PrimitiveType| {
            let types = program.resolved_types();
            let parts = match types.get(type_id) {
                Some(ResolvedType::Result { ok, error }) => Some((ok, error)),
                Some(ResolvedType::Nominal {
                    item, arguments, ..
                }) if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => {
                    Some((&arguments[0], &arguments[1]))
                }
                _ => None,
            };
            parts.is_some_and(|(ok_type, error_type)| {
                matches!(types.get(ok_type), Some(ResolvedType::Primitive(value)) if *value == ok)
                    && matches!(
                        types.get(error_type),
                        Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
                    )
            })
        };
        assert!(result_has_shape(
            get_i32.result_type.as_ref().expect("getter result type"),
            crate::core::PrimitiveType::I32
        ));
        assert!(result_has_shape(
            get_string.result_type.as_ref().expect("getter result type"),
            crate::core::PrimitiveType::String
        ));

        // This is a separately scanned profile. Existing lifecycle evidence
        // alone does not authorize the owned-get storage mode.
        assert!(analysis.ordinary_map_owner_lifecycle_v1_actions.is_empty());
    }

    #[test]
    fn ordinary_map_owned_get_v1_seals_original_tuple_map_get_with_independent_any_owner() {
        let file = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let result = map_get(root, "missing")
    drop(root)
    0
}
"#,
        );
        let program = crate::core::check_program(&file).expect("old tuple map_get remains valid");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");

        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_actions
            .iter()
            .any(|action| action.kind == OrdinaryMapOwnedGetI32StringV1ActionKind::GetAny));
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnedGetI32StringV1AttemptKind::UntypedGet));
        assert!(analysis.has_ordinary_map_owned_get_i32_string_v1_receipt());
    }

    #[test]
    fn ordinary_map_owned_get_v1_requires_its_own_typed_getter_attempt() {
        let file = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let size = map_size(root)
    drop(root)
    size
}
"#,
        );
        let program = crate::core::check_program(&file)
            .expect("ordinary lifecycle remains valid without typed getters");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");

        assert!(analysis.has_ordinary_map_owner_lifecycle_v1_receipt());
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_actions
            .is_empty());
        assert!(!analysis.has_ordinary_map_owned_get_i32_string_v1_receipt());
    }

    #[test]
    fn ordinary_map_owned_get_v1_withholds_receipt_across_result_control_flow() {
        let file = parse(
            r#"
func main() -> i32 {
    let key = "answer"
    let root = map_new()
    let result = map_get_i32(root, key)
    drop(root)
    match result {
        Ok(value) => value,
        Err(code) => code,
    }
}
"#,
        );
        let program =
            crate::core::check_program(&file).expect("Result status handling remains well typed");
        let analysis = program
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");

        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_actions
            .is_empty());
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnedGetI32StringV1AttemptKind::GetI32));
        assert!(!analysis.has_ordinary_map_owned_get_i32_string_v1_receipt());
    }

    #[test]
    fn ordinary_map_owned_get_v1_rejects_temporary_and_dropped_alias_sources() {
        for (name, body) in [
            (
                "temporary",
                r#"
    let key = "answer"
    let result = map_get_i32(map_new(), key)
    0
"#,
            ),
            (
                "alias after source drop",
                r#"
    let key = "answer"
    let root = map_new()
    drop(root)
    let alias = root
    let result = map_get_i32(alias, key)
    drop(alias)
    0
"#,
            ),
        ] {
            let source = format!("func main() -> i32 {{ {body} }}");
            let file = parse(&source);
            let program = crate::core::check_program(&file)
                .unwrap_or_else(|errors| panic!("{name} root remains well typed: {errors:?}"));
            let analysis = program
                .resource_analysis(&NodeId("function:main".into()))
                .expect("resource analysis");

            assert_eq!(
                analysis
                    .ordinary_map_owned_get_i32_string_v1_new_attempts
                    .len(),
                1
            );
            assert!(analysis
                .ordinary_map_owned_get_i32_string_v1_actions
                .is_empty());
            assert!(!analysis.has_ordinary_map_owned_get_i32_string_v1_receipt());
        }
    }
    #[test]
    fn ordinary_map_aliases_have_distinct_clone_receipts_and_scope_cleanup() {
        let file = parse(
            r#"
func main() -> i32 {
    let source = map_new()
    let numbers = map_set(source, "number", 42)
    let mixed = map_set(numbers, "text", "owned")
    let alias = mixed
    drop(mixed)
    map_size(alias) - 2
}
"#,
        );
        let checked =
            crate::core::check_program(&file).expect("ordinary alias remains a free value");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("analysis");
        let actions = &analysis.ordinary_map_owner_lifecycle_v1_actions;
        let alias = actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnerLifecycleActionKind::Clone)
            .expect("alias clone receipt");
        assert_ne!(alias.source_local.as_ref(), Some(&alias.local));
        assert_ne!(alias.source.as_ref(), Some(&alias.root));
        assert_eq!(
            actions
                .iter()
                .filter(|action| action.kind == OrdinaryMapOwnerLifecycleActionKind::Drop)
                .count(),
            4
        );
        assert!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owner_lifecycle_i32_string_v1_complete()
        );
    }

    #[test]
    fn ordinary_map_owned_get_alias_scoped_drops_preserve_snapshot_receipt() {
        let file = parse(
            r#"
func main() -> i32 {
    let source = map_new()
    let full = map_set(source, "text", "kept")
    let alias = full
    let result = map_get_string(alias, "text")
    drop(full)
    drop(result)
    map_size(alias) - 1
}
"#,
        );
        let checked = crate::core::check_program(&file).expect("typed owned snapshot and alias");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("analysis");
        let actions = &analysis.ordinary_map_owned_get_i32_string_v1_actions;
        assert!(actions
            .iter()
            .any(|action| action.kind == OrdinaryMapOwnedGetI32StringV1ActionKind::Clone));
        assert_eq!(
            actions
                .iter()
                .filter(|action| action.kind == OrdinaryMapOwnedGetI32StringV1ActionKind::Drop)
                .count(),
            3
        );
        assert!(analysis.has_ordinary_map_owned_get_i32_string_v1_receipt());
        assert!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_get_i32_string_v1_complete()
        );
    }

    #[test]
    fn ordinary_map_owned_get_v1_receipts_entry_closed_map_free_if_tail() {
        let file = parse(
            r#"
func carry<T>(value: T) -> T { value }

func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "key", "owned")
    let pair = map_get(root, "key")
    drop(empty)
    drop(root)

    let found = pair.0
    let payload = pair.1
    let carried = carry(payload)
    let equal = if found { carried == "owned" } else { false }
    drop(pair)
    drop(payload)
    drop(carried)
    println(equal)
    0
}
"#,
        );
        let checked = crate::core::check_program(&file)
            .expect("the ordinary Any identity helper and Map-free if tail are well typed");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        let receipt = analysis
            .ordinary_map_owned_get_i32_string_v1_cfg_receipt
            .as_ref()
            .expect("closed Map entry must carry its exact Map-free tail CFG");

        assert!(analysis.has_ordinary_map_owned_get_i32_string_v1_receipt());
        assert!(receipt.reachable_blocks.len() > 1);
        assert_eq!(receipt.entry, receipt.map_operation_block);
        assert!(receipt
            .reachable_blocks
            .keys()
            .any(|block| { block.0 .0.ends_with("/cfg:block:if-then") }));
        assert!(receipt
            .reachable_blocks
            .keys()
            .any(|block| { block.0 .0.ends_with("/cfg:block:if-else") }));
        assert!(receipt
            .reachable_blocks
            .keys()
            .any(|block| { block.0 .0.ends_with("/cfg:block:if-join") }));
        assert!(receipt
            .reachable_edges
            .values()
            .any(|edge| { edge.kind == crate::core::cfg::EdgeKind::Then }));
        assert!(receipt
            .reachable_edges
            .values()
            .any(|edge| { edge.kind == crate::core::cfg::EdgeKind::Else }));
        assert!(receipt
            .reachable_edges
            .values()
            .any(|edge| { edge.kind == crate::core::cfg::EdgeKind::Fallthrough }));
        let entry_points = &receipt
            .reachable_blocks
            .get(&receipt.entry)
            .expect("entry fact")
            .points;
        assert!(receipt.action_points.iter().all(|point| {
            entry_points
                .iter()
                .any(|(entry_point, _)| entry_point == point)
        }));
        assert!(receipt.attempts.iter().all(|attempt| {
            entry_points
                .iter()
                .any(|(entry_point, _)| entry_point == &attempt.point)
        }));
    }

    #[test]
    fn ordinary_map_owned_get_v1_keeps_branch_local_map_attempts_fail_closed() {
        let file = parse(
            r#"
func main() -> i32 {
    let size = if true {
        let root = map_new()
        let value = map_get_i32(root, "key")
        drop(root)
        1
    } else {
        0
    }
    size
}
"#,
        );
        let checked = crate::core::check_program(&file)
            .expect("branch-local ordinary Map operations remain valid source");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");

        assert!(!analysis
            .ordinary_map_owned_get_i32_string_v1_operation_attempts
            .is_empty());
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_actions
            .is_empty());
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_cfg_receipt
            .is_none());
    }

    #[test]
    fn ordinary_map_owned_get_v1_accepts_match_tail_without_arm_map() {
        let file = parse(
            r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "key", "owned")
    let pair = map_get(root, "key")
    drop(empty)
    drop(root)
    let payload = pair.1
    let equal = match pair.0 {
        true => payload == "owned",
        false => false,
    }
    drop(pair)
    drop(payload)
    println(equal)
    0
}
"#,
        );
        let checked =
            crate::core::check_program(&file).expect("Map/Any match tail remains valid source");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        let receipt = analysis
            .ordinary_map_owned_get_i32_string_v1_cfg_receipt
            .as_ref()
            .expect("entry Map work with a pure match tail must carry a receipt");
        assert!(receipt
            .reachable_blocks
            .keys()
            .any(|block| { block.0 .0.ends_with("/cfg:block:match-arm") }));
        assert!(receipt
            .reachable_blocks
            .keys()
            .any(|block| { block.0 .0.ends_with("/cfg:block:match-join") }));
        assert!(receipt
            .reachable_edges
            .values()
            .any(|edge| edge.kind == crate::core::cfg::EdgeKind::MatchArm));
        assert!(receipt.action_points.iter().all(|point| {
            receipt
                .reachable_blocks
                .get(&receipt.entry)
                .is_some_and(|block| block.points.iter().any(|(node, _)| node == point))
        }));
    }

    #[test]
    fn ordinary_map_owned_get_v1_keeps_match_arm_map_attempts_fail_closed() {
        let file = parse(
            r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "key", "owned")
    let pair = map_get(root, "key")
    drop(empty)
    drop(root)
    let value = match pair.0 {
        true => {
            let arm_empty = map_new()
            let arm_root = map_set(arm_empty, "arm", 1)
            drop(arm_empty)
            drop(arm_root)
            1
        },
        false => 0,
    }
    drop(pair)
    value
}
"#,
        );
        let checked = crate::core::check_program(&file)
            .expect("match-arm Map operations remain valid source");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnedGetI32StringV1AttemptKind::New));
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_actions
            .is_empty());
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_cfg_receipt
            .is_none());
    }

    #[test]
    fn ordinary_map_owned_get_v1_keeps_loop_attempts_fail_closed() {
        let file = parse(
            r#"
func main() -> i32 {
    let mut i = 0
    while i < 1 {
        let root = map_new()
        let value = map_get_i32(root, "key")
        drop(root)
        i = i + 1
    }
    0
}
"#,
        );
        let checked = crate::core::check_program(&file)
            .expect("loop-local ordinary Map operations are valid source");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");

        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_operation_attempts
            .iter()
            .any(
                |attempt| attempt.kind == OrdinaryMapOwnedGetI32StringV1AttemptKind::UntypedGet
                    || attempt.kind == OrdinaryMapOwnedGetI32StringV1AttemptKind::GetI32
            ));
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_actions
            .is_empty());
        assert!(analysis
            .ordinary_map_owned_get_i32_string_v1_cfg_receipt
            .is_none());
    }

    #[test]
    fn ordinary_map_owned_get_result_receipt_distinguishes_scope_cleanup() {
        let file = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "answer")
    drop(root)
    0
}
"#,
        );
        let checked = crate::core::check_program(&file).expect("scope-only result source checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        let receipt = analysis
            .ordinary_map_owned_get_i32_string_v1_cfg_receipt
            .as_ref()
            .expect("Map root and GetAny operation remain checker-complete");
        assert_eq!(receipt.result_receipts.len(), 1);
        let result = &receipt.result_receipts[0];
        assert_eq!(
            result.kind,
            OrdinaryMapOwnedGetI32StringV1ActionKind::GetAny
        );
        assert!(
            result.binding.is_some(),
            "GetAny result binding is recorded"
        );
        assert_eq!(
            result.settlement,
            OrdinaryMapOwnedGetResultSettlementKind::ScopeDrop
        );
        assert!(!result.settlement.is_receipted());
    }

    #[test]
    fn ordinary_map_owned_get_result_receipt_records_explicit_drop_after_observation() {
        let file = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let snapshot = values(root)
    let count = snapshot.len()
    drop(snapshot)
    drop(root)
    count
}
"#,
        );
        let checked = crate::core::check_program(&file).expect("explicit Values result checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("resource analysis");
        let receipt = analysis
            .ordinary_map_owned_get_i32_string_v1_cfg_receipt
            .as_ref()
            .expect("Values operation remains checker-complete");
        assert_eq!(receipt.result_receipts.len(), 1);
        let result = &receipt.result_receipts[0];
        assert_eq!(
            result.kind,
            OrdinaryMapOwnedGetI32StringV1ActionKind::Values
        );
        assert!(
            result.binding.is_some(),
            "Values result binding is recorded"
        );
        assert_eq!(
            result.settlement,
            OrdinaryMapOwnedGetResultSettlementKind::ExplicitDrop
        );
        assert!(result.settlement.is_receipted());
        assert!(result.settlement_point.is_some());
    }

    #[test]
    fn ordinary_map_owned_any_v2_records_record_payload_clone_and_root_chain() {
        let file = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let empty = map_new()
    let one = map_set(empty, "answer", 42)
    let two = map_set(one, "label", label)
    let overwritten = map_set(two, "label", 99)
    let removed = map_remove(two, "label")
    map_size(empty)
    map_size(one)
    map_size(two)
    map_size(overwritten)
    map_size(removed)
    has_key(two, "label")
    has_key(overwritten, "label")
    has_key(removed, "label")
    println(label.text)
    drop(empty)
    drop(one)
    drop(two)
    drop(overwritten)
    drop(removed)
    drop(label)
    0
}
"#,
        );
        let checked = crate::core::check_program(&file).expect("v2 record fixture checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("v2 record resource analysis");
        assert!(analysis.has_ordinary_map_owned_any_v2_receipt());
        let actions = &analysis.ordinary_map_owned_any_v2_actions;
        assert_eq!(
            actions.iter().map(|action| action.kind).collect::<Vec<_>>(),
            vec![
                OrdinaryMapOwnedAnyV2ActionKind::New,
                OrdinaryMapOwnedAnyV2ActionKind::Set,
                OrdinaryMapOwnedAnyV2ActionKind::Set,
                OrdinaryMapOwnedAnyV2ActionKind::Set,
                OrdinaryMapOwnedAnyV2ActionKind::Remove,
                OrdinaryMapOwnedAnyV2ActionKind::Size,
                OrdinaryMapOwnedAnyV2ActionKind::Size,
                OrdinaryMapOwnedAnyV2ActionKind::Size,
                OrdinaryMapOwnedAnyV2ActionKind::Size,
                OrdinaryMapOwnedAnyV2ActionKind::Size,
                OrdinaryMapOwnedAnyV2ActionKind::HasKey,
                OrdinaryMapOwnedAnyV2ActionKind::HasKey,
                OrdinaryMapOwnedAnyV2ActionKind::HasKey,
                OrdinaryMapOwnedAnyV2ActionKind::Drop,
                OrdinaryMapOwnedAnyV2ActionKind::Drop,
                OrdinaryMapOwnedAnyV2ActionKind::Drop,
                OrdinaryMapOwnedAnyV2ActionKind::Drop,
                OrdinaryMapOwnedAnyV2ActionKind::Drop,
            ]
        );
        let set_actions = actions
            .iter()
            .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Set)
            .collect::<Vec<_>>();
        assert!(set_actions.iter().all(|action| action.payload_clone));
        let record_set = set_actions
            .iter()
            .find(|action| {
                action.value_type
                    != Some(crate::core::ir::primitive_type_id(
                        crate::core::PrimitiveType::I32,
                    ))
            })
            .expect("record payload Set action");
        assert!(matches!(
            checked
                .resolved_types()
                .get(record_set.value_type.as_ref().expect("record payload type")),
            Some(ResolvedType::Nominal { item, arguments, .. })
                if item.as_str().contains("Label") && arguments.is_empty()
        ));
        let set_two = &set_actions[1];
        assert_eq!(set_two.source, Some(set_actions[0].root.clone()));
        assert_eq!(set_actions[2].source, Some(set_two.root.clone()));
        assert_eq!(
            analysis.ordinary_map_owned_any_v2_operation_attempts.len(),
            actions.len()
        );
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("v2 source receipt");
        assert_eq!(receipt.actions, *actions);
        assert_eq!(
            receipt.attempts,
            analysis.ordinary_map_owned_any_v2_operation_attempts
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_clone_preserves_independent_roots_and_payload_selection() {
        let source = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "selected payload" }
    let empty = map_new()
    let selected = map_set(empty, "label", label)
    let copied = selected
    let snapshot = values(copied)
    let item = snapshot[0] as Label
    drop(item)
    drop(snapshot)
    drop(empty)
    drop(selected)
    drop(copied)
    drop(label)
    0
}
"#,
        );
        let checked = crate::core::check_program(&source).expect("Map Clone source checks");
        let owner = NodeId("function:main".into());
        let analysis = checked
            .resource_analysis(&owner)
            .expect("Map Clone resource analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("independent Map roots and their settlements are receipted");
        let body = checked.resolved_body(&owner).expect("resolved main body");
        let clone = receipt
            .actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Clone)
            .expect("Map Clone action");
        let source_root = receipt
            .actions
            .iter()
            .find(|action| {
                action.kind == OrdinaryMapOwnedAnyV2ActionKind::Set
                    && action.local == *clone.source_local.as_ref().expect("clone source local")
            })
            .expect("source root is independently introduced by Set");
        assert_eq!(clone.source, Some(source_root.root.clone()));
        assert_ne!(clone.root, source_root.root);
        assert_eq!(clone.source_node.as_ref(), Some(&clone.point));
        assert_eq!(
            clone.source_type.as_ref(),
            body.locals.get(&clone.local).map(|local| &local.ty),
            "Clone records the exact shared Map-root type identity"
        );
        assert_eq!(
            body.locals
                .get(clone.source_local.as_ref().expect("clone source local"))
                .map(|local| &local.ty),
            clone.source_type.as_ref()
        );
        let clone_attempt = receipt
            .attempts
            .iter()
            .find(|attempt| {
                attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::Clone
                    && attempt.point == clone.point
            })
            .expect("exact Clone attempt");
        assert_eq!(
            clone_attempt.arguments,
            vec![clone.source_node.clone().unwrap()]
        );
        assert_eq!(clone_attempt.receiver_local, clone.source_local);
        assert_eq!(clone_attempt.result_local, Some(clone.local.clone()));

        let values = receipt
            .actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Values)
            .expect("Values action on the clone");
        assert_eq!(values.source_local.as_ref(), Some(&clone.local));
        assert_eq!(receipt.values_unpack_receipts.len(), 1);
        assert_eq!(
            receipt.values_unpack_receipts[0].payload_selection,
            vec![clone.point.clone(), source_root.point.clone()],
            "the clone's selection lineage reaches the Set on its source root"
        );

        let dropped_roots = receipt
            .actions
            .iter()
            .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Drop)
            .map(|action| action.local.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            dropped_roots,
            BTreeSet::from([
                source_root.source_local.clone().expect("empty source root"),
                source_root.local.clone(),
                clone.local.clone(),
            ]),
            "the original persistent chain and cloned root settle independently"
        );
        assert_eq!(
            receipt
                .actions
                .iter()
                .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Drop)
                .count(),
            3
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_clone_rejects_projected_and_unknown_sources() {
        let source = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "selected payload" }
    let empty = map_new()
    let selected = map_set(empty, "label", label)
    let copied = selected
    let pair = map_get(copied, "label")
    let found = pair.0
    drop(pair)
    drop(empty)
    drop(selected)
    drop(copied)
    drop(label)
    if found { 1 } else { 0 }
}
"#,
        );
        let checked = crate::core::check_program(&source).expect("Map Clone source checks");
        let owner = NodeId("function:main".into());
        let cfg = checked.callable_cfg(&owner).expect("main CFG");
        let mut body = checked
            .resolved_body(&owner)
            .expect("resolved main body")
            .clone();
        let types = checked.resolved_types();

        let copied_binding = body
            .root
            .statements
            .iter_mut()
            .find_map(|statement| {
                let ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                } = &mut statement.kind
                else {
                    return None;
                };
                let ResolvedPatternKind::Binding { local, .. } = &pattern.kind else {
                    return None;
                };
                (body
                    .locals
                    .get(local)
                    .is_some_and(|binding| binding.display_name == "copied"))
                .then_some((initializer, local.clone()))
            })
            .expect("direct copied binding");
        let (initializer, copied_local) = copied_binding;
        let ResolvedExprKind::Load(source_place) = &mut initializer.kind else {
            panic!("Map Clone source is a resolved local load");
        };
        source_place.projections.push(ResolvedProjection::Field {
            field: NodeId("test:projected-map-root-field".into()),
            name: "projected".into(),
            ty: initializer.ty.clone(),
        });
        let projected_point = initializer.node_id.clone();
        let (_, attempts, receipt) = OrdinaryMapOwnedAnyV2Checker::collect(cfg, &body, types);
        assert!(
            receipt.is_none(),
            "projected Map-root source must fail closed"
        );
        let projected_attempt = attempts
            .iter()
            .find(|attempt| {
                attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::Clone
                    && attempt.point == projected_point
            })
            .expect("projected source remains visible as a Clone attempt");
        assert_eq!(projected_attempt.result_local, Some(copied_local));
        assert_eq!(projected_attempt.receiver_local, None);

        let mut body = checked
            .resolved_body(&owner)
            .expect("resolved main body")
            .clone();
        let (initializer, copied_local) = body
            .root
            .statements
            .iter_mut()
            .find_map(|statement| {
                let ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                } = &mut statement.kind
                else {
                    return None;
                };
                let ResolvedPatternKind::Binding { local, .. } = &pattern.kind else {
                    return None;
                };
                (body
                    .locals
                    .get(local)
                    .is_some_and(|binding| binding.display_name == "copied"))
                .then_some((initializer, local.clone()))
            })
            .expect("direct copied binding");
        let source_type = initializer.ty.clone();
        let unknown_source = ResolvedLocalId(NodeId("test:unknown-map-root".into()));
        body.locals.insert(
            unknown_source.clone(),
            ResolvedLocal {
                id: unknown_source.clone(),
                display_name: "unknown_map_root".into(),
                ty: source_type.clone(),
                mutable: false,
                origin: initializer.origin.clone(),
            },
        );
        initializer.kind = ResolvedExprKind::Load(ResolvedPlace::root(unknown_source.clone()));
        let unknown_point = initializer.node_id.clone();
        let (_, attempts, receipt) = OrdinaryMapOwnedAnyV2Checker::collect(cfg, &body, types);
        assert!(
            receipt.is_none(),
            "unintroduced Map-root source must fail closed"
        );
        let unknown_attempt = attempts
            .iter()
            .find(|attempt| {
                attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::Clone
                    && attempt.point == unknown_point
            })
            .expect("unknown source remains visible as a Clone attempt");
        assert_eq!(unknown_attempt.receiver_local, Some(unknown_source));
        assert_eq!(unknown_attempt.result_local, Some(copied_local));
    }

    #[test]
    fn ordinary_map_owned_any_v2_clone_requires_explicit_settlement() {
        let source = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "selected payload" }
    let empty = map_new()
    let selected = map_set(empty, "label", label)
    let copied = selected
    map_size(copied)
    drop(empty)
    drop(selected)
    drop(label)
    0
}
"#,
        );
        let checked = crate::core::check_program(&source).expect("Map Clone source checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Map Clone resource analysis");
        assert!(
            analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none(),
            "scope cleanup cannot settle the clone's independent root obligation"
        );
        assert!(analysis
            .ordinary_map_owned_any_v2_operation_attempts
            .iter()
            .any(
                |attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::ScopeDrop
                    && attempt.receiver_local.as_ref().is_some_and(|local| {
                        checked
                            .resolved_body(&NodeId("function:main".into()))
                            .and_then(|body| body.locals.get(local))
                            .is_some_and(|binding| binding.display_name == "copied")
                    })
            ));
    }

    #[test]
    fn ordinary_map_owned_any_v2_get_any_and_values_have_exact_result_receipts() {
        let get = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let pair = map_get(root, "label")
    let _found = pair.0
    drop(pair)
    drop(empty)
    drop(root)
    drop(label)
    0
}
"#,
        );
        let checked = crate::core::check_program(&get).expect("v2 GetAny source checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("GetAny resource analysis");
        assert!(analysis.has_ordinary_map_owned_any_v2_receipt());
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("GetAny V2 source receipt");
        let action = receipt
            .actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::GetAny)
            .expect("GetAny action");
        assert_eq!(
            action.profile_version,
            OrdinaryMapOwnedAnyV2ProfileVersion::V2
        );
        assert_eq!(action.source_local.as_ref(), Some(&action.local));
        assert_eq!(action.source.as_ref(), Some(&action.root));
        assert!(action.source_node.is_some());
        assert!(matches!(
            action
                .source_type
                .as_ref()
                .and_then(|ty| checked.resolved_types().get(ty)),
            Some(ResolvedType::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
        ));
        assert!(action.key.is_some());
        assert_eq!(action.key_local, None);
        assert!(matches!(
            action
                .key_type
                .as_ref()
                .and_then(|ty| checked.resolved_types().get(ty)),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
        ));
        assert!(matches!(
            action
                .result_type
                .as_ref()
                .and_then(|ty| checked.resolved_types().get(ty)),
            Some(ResolvedType::Tuple(elements))
                if elements.len() == 2
                    && matches!(checked.resolved_types().get(&elements[0]), Some(ResolvedType::Primitive(crate::core::PrimitiveType::Bool)))
                    && matches!(checked.resolved_types().get(&elements[1]), Some(ResolvedType::DynamicAny { .. }))
        ));
        assert!(action.result_local.is_some());
        assert_eq!(receipt.result_receipts.len(), 1);
        let result = &receipt.result_receipts[0];
        assert_eq!(
            result.profile_version,
            OrdinaryMapOwnedAnyV2ProfileVersion::V2
        );
        assert_eq!(result.kind, OrdinaryMapOwnedAnyV2ActionKind::GetAny);
        assert_eq!(result.source_local, action.local);
        assert_eq!(result.source, action.root);
        assert_eq!(result.source_node, action.source_node.clone().unwrap());
        assert_eq!(result.key, action.key);
        assert_eq!(result.key_local, action.key_local);
        assert_eq!(result.result_type, action.result_type.clone().unwrap());
        assert_eq!(result.binding, action.result_local.clone().unwrap());
        assert_eq!(
            result.settlement,
            OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        );
        assert!(result.settlement_point.is_some());

        let values = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let snapshot = values(root)
    let count = snapshot.len()
    drop(snapshot)
    drop(empty)
    drop(root)
    drop(label)
    count
}
"#,
        );
        let checked = crate::core::check_program(&values).expect("v2 Values source checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Values resource analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("Values V2 source receipt");
        let action = receipt
            .actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Values)
            .expect("Values action");
        assert_eq!(action.source_local.as_ref(), Some(&action.local));
        assert_eq!(action.source.as_ref(), Some(&action.root));
        assert!(action.source_node.is_some());
        assert!(matches!(
            action
                .result_type
                .as_ref()
                .and_then(|ty| checked.resolved_types().get(ty)),
            Some(ResolvedType::Nominal { item, arguments, .. })
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && matches!(checked.resolved_types().get(&arguments[0]), Some(ResolvedType::DynamicAny { .. }))
        ));
        assert_eq!(receipt.result_receipts.len(), 1);
        assert_eq!(
            receipt.result_receipts[0].settlement,
            OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_values_receipts_cover_direct_constant_and_dynamic_indices() {
        let source = parse(
            r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let snapshot = values(root)
    let index = 0
    let literal_value = snapshot[0] as i32
    let dynamic_value = snapshot[index] as i32
    drop(snapshot)
    drop(empty)
    drop(root)
    literal_value + dynamic_value
}
"#,
        );
        let checked = crate::core::check_program(&source)
            .expect("direct indexed Values casts keep their checked conversions");
        let owner = NodeId("function:main".into());
        let analysis = checked
            .resource_analysis(&owner)
            .expect("Values unpack resource analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("Values, unpack bindings, and explicit snapshot Drop are receipted");
        assert!(
            receipt.unpack_receipts.is_empty(),
            "GetAny receipts stay separate"
        );
        assert_eq!(receipt.values_unpack_receipts.len(), 2);

        let body = checked
            .resolved_body(&owner)
            .expect("Values unpack resolved body");
        let snapshot = body
            .locals
            .values()
            .find(|local| local.display_name == "snapshot")
            .expect("direct Values snapshot local");
        let index_local = body
            .locals
            .values()
            .find(|local| local.display_name == "index")
            .expect("dynamic index local");
        let set = receipt
            .actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Set)
            .expect("same-root payload Set action");
        let values_action = receipt
            .actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Values)
            .expect("Values action");
        let mut bindings = receipt
            .values_unpack_receipts
            .iter()
            .map(|unpack| {
                let local = body
                    .locals
                    .get(&unpack.unpack_binding)
                    .expect("unpack binding local");
                assert_eq!(unpack.operation, values_action.point);
                assert_eq!(unpack.source_local, values_action.local);
                assert_eq!(unpack.source, values_action.root);
                assert_eq!(unpack.snapshot_binding, snapshot.id);
                assert_eq!(unpack.payload_selection, vec![set.point.clone()]);
                assert_eq!(
                    unpack.conversion_from,
                    values_action
                        .result_type
                        .as_ref()
                        .and_then(|ty| match checked.resolved_types().get(ty) {
                            Some(ResolvedType::Nominal { arguments, .. })
                                if arguments.len() == 1 =>
                            {
                                Some(arguments[0].clone())
                            }
                            _ => None,
                        })
                        .expect("Values element type")
                );
                assert_eq!(
                    unpack.conversion_kind,
                    crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                );
                assert_eq!(
                    unpack.conversion_to,
                    crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
                );
                assert_eq!(
                    unpack.settlement,
                    OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                );
                assert_eq!(
                    receipt.result_receipts[0].settlement_point,
                    Some(unpack.settlement_point.clone())
                );
                (
                    local.display_name.clone(),
                    unpack.index_expression.clone(),
                    unpack.index_type.clone(),
                    unpack.index_local.clone(),
                )
            })
            .collect::<Vec<_>>();
        bindings.sort();
        assert_eq!(
            bindings
                .iter()
                .map(|(name, _, _, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["dynamic_value", "literal_value"]
        );
        assert_ne!(bindings[0].1, bindings[1].1);
        assert_eq!(bindings[0].3, Some(index_local.id.clone()));
        assert_eq!(bindings[1].3, None);
        let mut index_forms = body
            .root
            .statements
            .iter()
            .filter_map(|statement| {
                let ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                } = &statement.kind
                else {
                    return None;
                };
                let ResolvedPatternKind::Binding { local, .. } = &pattern.kind else {
                    return None;
                };
                let ResolvedExprKind::Cast { value, .. } = &initializer.kind else {
                    return None;
                };
                let ResolvedExprKind::Project {
                    projection: ResolvedValueProjection::Index(index),
                    ..
                } = &value.kind
                else {
                    return None;
                };
                let display_name = body.locals.get(local)?.display_name.clone();
                let index_local = match &index.kind {
                    ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                        Some(place.base.clone())
                    }
                    _ => None,
                };
                Some((
                    display_name,
                    index.node_id.clone(),
                    index.ty.clone(),
                    index_local,
                ))
            })
            .collect::<Vec<_>>();
        index_forms.sort();
        assert_eq!(
            index_forms
                .iter()
                .map(|(name, _, _, index_local)| (name.as_str(), index_local.is_some()))
                .collect::<Vec<_>>(),
            vec![("dynamic_value", true), ("literal_value", false),]
        );
        for (name, expression, ty, local) in index_forms {
            let (_, receipt_expression, receipt_type, receipt_local) = bindings
                .iter()
                .find(|(binding, _, _, _)| binding == &name)
                .expect("receipt for direct indexed binding");
            assert_eq!(&expression, receipt_expression);
            assert_eq!(&ty, receipt_type);
            assert_eq!(&local, receipt_local);
        }
    }

    #[test]
    fn ordinary_map_owned_any_v2_values_rejects_unselected_wrong_and_unsettled_unpacks() {
        fn assert_no_values_unpack_receipt(source: &str) {
            let file = parse(source);
            match crate::core::check_program(&file) {
                Ok(checked) => {
                    let analysis = checked
                        .resource_analysis(&NodeId("function:main".into()))
                        .expect("negative Values source analysis");
                    assert!(
                        analysis
                            .ordinary_map_owned_any_v2_cfg_receipt
                            .as_ref()
                            .is_none_or(|receipt| receipt.values_unpack_receipts.is_empty()),
                        "an unproved Values cast must not receive the direct-index receipt"
                    );
                }
                Err(errors) => assert!(
                    !errors.is_empty(),
                    "a malformed or early-settled Values unpack must be rejected"
                ),
            }
        }

        // No same-root Set selected i32, so this is an unreceipted unpack.
        assert_no_values_unpack_receipt(
            r#"
func main() -> i32 {
    let root = map_new()
    let snapshot = values(root)
    let item = snapshot[0] as i32
    drop(snapshot)
    drop(root)
    item
}
"#,
        );

        // A Set on this root selected i32, not String.
        assert_no_values_unpack_receipt(
            r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let snapshot = values(root)
    let wrong = snapshot[0] as string
    drop(snapshot)
    drop(empty)
    drop(root)
    drop(wrong)
    0
}
"#,
        );

        // Explicitly settling the snapshot before the read is either rejected
        // by ownership checking or withheld by the exact source receipt.
        assert_no_values_unpack_receipt(
            r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let snapshot = values(root)
    drop(snapshot)
    let item = snapshot[0] as i32
    drop(empty)
    drop(root)
    item
}
"#,
        );

        // The payload selector belongs to a different Map root than the
        // snapshot being indexed.
        assert_no_values_unpack_receipt(
            r#"
func main() -> i32 {
    let selected_empty = map_new()
    let selected = map_set(selected_empty, "answer", 41)
    let unrelated = map_new()
    let snapshot = values(unrelated)
    let item = snapshot[0] as i32
    drop(snapshot)
    drop(selected_empty)
    drop(selected)
    drop(unrelated)
    item
}
"#,
        );

        // Without an explicit drop the Values owner receives only synthetic
        // scope settlement, which is never a source receipt.
        assert_no_values_unpack_receipt(
            r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let snapshot = values(root)
    let item = snapshot[0] as i32
    drop(empty)
    drop(root)
    item
}
"#,
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_values_rejects_projected_index_operand() {
        let source = parse(
            r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let snapshot = values(root)
    let indexes = [0]
    let item = snapshot[indexes[0]] as i32
    drop(snapshot)
    drop(indexes)
    drop(empty)
    drop(root)
    item
}
"#,
        );
        let checked = crate::core::check_program(&source)
            .expect("a projected integer index remains a valid source expression");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("projected index resource analysis");
        assert!(
            analysis
                .ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .is_none_or(|receipt| receipt.values_unpack_receipts.is_empty()),
            "a projected index operand has no direct local MIR operand receipt"
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_dynamic_unpack_return_stays_fail_closed_without_scope_receipt() {
        let accepted = parse(
            r#"
func main() -> i32 {
    let empty = map_new()
    let pair = map_get(empty, "missing")
    drop(empty)
    pair.1
}
"#,
        );
        let checked = crate::core::check_program(&accepted)
            .expect("Any may flow down into the concrete function return type");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("GetAny return resource analysis");
        let body = checked
            .resolved_body(&NodeId("function:main".into()))
            .expect("GetAny return resolved body");
        let cast = body
            .root
            .result
            .as_deref()
            .expect("Any return result")
            .kind
            .clone();
        let pair_local = body
            .locals
            .values()
            .find(|local| local.display_name == "pair")
            .expect("GetAny pair local")
            .id
            .clone();
        let ResolvedExprKind::Cast { value, conversion } = cast else {
            panic!("Any branch result must retain its checked unpack conversion");
        };
        assert_eq!(
            conversion.kind,
            crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
        );
        assert!(matches!(
            &value.kind,
            ResolvedExprKind::Load(place)
                if place.base == pair_local
                    && matches!(place.projections.as_slice(), [ResolvedProjection::Tuple { index: 1, .. }])
        ));
        assert!(
            analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none(),
            "return conversion plus implicit pair scope-drop must stay outside the explicit-settlement receipt"
        );

        let rejected = parse(
            r#"
func helper(foreign: Any) -> i32 {
    let empty = map_new()
    let pair = map_get(empty, "missing")
    let boxed_found: Any = pair.0
    let wrong_field: i32 = boxed_found
    let independent: i32 = foreign
    drop(pair)
    drop(empty)
    wrong_field + independent
}
func main() -> i32 { 0 }
"#,
        );
        let errors = crate::core::check_program(&rejected)
            .expect_err("Any values cannot be sunk into concrete let bindings");
        assert!(
            errors
                .iter()
                .any(|error| error.code.as_deref() == Some("E0209")),
            "the checker must reject a forged field-0/independent Any unpack before routing"
        );

        let escaped = parse(
            r#"
func main() -> i32 {
    let empty = map_new()
    let pair = map_get(empty, "missing")
    let raw = pair.1
    drop(pair)
    drop(empty)
    0
}
"#,
        );
        let checked = crate::core::check_program(&escaped)
            .expect("same-type Any projection is a legal source expression");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("escaped Any projection resource analysis");
        assert!(
            analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none(),
            "an Any alias of GetAny field 1 must not be settled by dropping only the pair"
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_receipts_explicit_checked_unpack_and_pair_drop() {
        let source = parse(
            r#"
func main() -> i32 {
    let empty = map_new()
    let pair = map_get(empty, "missing")
    let value = pair.1 as i32
    drop(pair)
    drop(empty)
    value
}
"#,
        );
        let checked = crate::core::check_program(&source)
            .expect("explicit Any unpack retains its checked conversion");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("GetAny source analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("exact GetAny, unpack, and pair settlement source receipt");
        assert_eq!(receipt.result_receipts.len(), 1);
        assert_eq!(receipt.unpack_receipts.len(), 1);
        let unpack = &receipt.unpack_receipts[0];
        assert_eq!(
            unpack.conversion_kind,
            crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
        );
        assert_eq!(
            unpack.conversion_to,
            crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
        );
        assert_eq!(
            unpack.settlement,
            OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        );
        assert_eq!(
            receipt.result_receipts[0].settlement_point,
            Some(unpack.settlement_point.clone())
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_string_payload_has_checker_set_and_unpack_receipts() {
        let source = parse(
            r#"
func main() -> string {
    let source = "detached String payload"
    let empty = map_new()
    let stored = map_set(empty, "present", source)
    drop(source)
    let pair = map_get(stored, "present")
    let value = pair.1 as string
    drop(pair)
    drop(empty)
    drop(stored)
    value
}
"#,
        );
        let checked = crate::core::check_program(&source)
            .expect("direct String Map payload and checked Any unpack typecheck");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("String AnyOwner-v2 source analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("String Set, GetAny, unpack, and pair Drop have exact Checker receipts");
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let set = receipt
            .actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Set)
            .expect("String Set action");
        assert_eq!(set.value_type.as_ref(), Some(&string_type));
        assert!(set.payload_clone, "Set snapshots its String source");

        assert_eq!(receipt.result_receipts.len(), 1);
        assert_eq!(receipt.unpack_receipts.len(), 1);
        let unpack = &receipt.unpack_receipts[0];
        assert_eq!(unpack.conversion_to, string_type);
        assert_eq!(
            unpack.conversion_kind,
            crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
        );
        assert_eq!(
            unpack.settlement,
            OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        );
        assert_eq!(
            receipt.result_receipts[0].settlement_point,
            Some(unpack.settlement_point.clone()),
            "pair Drop remains an explicit, separate owner settlement"
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_record_unpack_requires_a_set_on_the_get_root_lineage() {
        let selected_on_root = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let empty = map_new()
    let stored = map_set(empty, "label", label)
    let pair = map_get(stored, "label")
    let value = pair.1 as Label
    drop(pair)
    drop(empty)
    drop(stored)
    drop(label)
    println(value.text)
    drop(value)
    0
}
"#,
        );
        let checked = crate::core::check_program(&selected_on_root)
            .expect("a selected one-field record can be checked as the GetAny target");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("selected record source analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("the source Set/GetAny/Record-unpack/Drop chain is receipted");
        assert_eq!(receipt.unpack_receipts.len(), 1);
        let target = receipt.unpack_receipts[0].conversion_to.clone();
        assert!(matches!(
            checked.resolved_types().get(&target),
            Some(ResolvedType::Nominal { item, arguments, .. })
                if item.as_str().ends_with("Label") && arguments.is_empty()
        ));
        assert_eq!(
            receipt
                .actions
                .iter()
                .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Set)
                .and_then(|action| action.value_type.as_ref()),
            Some(&target),
            "the exact Label TypeId is selected on this persistent root"
        );

        let selected_on_other_root = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let selected_empty = map_new()
    let selected = map_set(selected_empty, "label", label)
    let empty = map_new()
    let pair = map_get(empty, "missing")
    let value = pair.1 as Label
    drop(pair)
    drop(selected_empty)
    drop(selected)
    drop(empty)
    drop(label)
    drop(value)
    0
}
"#,
        );
        let checked = crate::core::check_program(&selected_on_other_root)
            .expect("the unrelated-root cast remains representable before MIR admission");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("unrelated-root source analysis");
        assert!(
            analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none(),
            "a Set on another Map root cannot authorize this GetAny Record target"
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_unpack_target_profile_is_i32_string_and_nominal() {
        let i32_program = crate::core::check_program(&parse("func main() -> i32 { 0 }"))
            .expect("i32 target program checks");
        let i32_body = i32_program
            .resolved_body(&NodeId("function:main".into()))
            .expect("i32 target body");
        let i32_ty = &i32_body.root.result.as_deref().expect("i32 result").ty;
        assert!(ordinary_map_owned_any_v2_unpack_target_supported(
            i32_program.resolved_types(),
            i32_ty
        ));

        let string_program = crate::core::check_program(&parse("func main() -> string { \"\" }"))
            .expect("String target program checks");
        let string_body = string_program
            .resolved_body(&NodeId("function:main".into()))
            .expect("String target body");
        let string_ty = &string_body
            .root
            .result
            .as_deref()
            .expect("String result")
            .ty;
        assert!(ordinary_map_owned_any_v2_unpack_target_supported(
            string_program.resolved_types(),
            string_ty
        ));
        let record_program = crate::core::check_program(&parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "target" }
    drop(label)
    0
}
"#,
        ))
        .expect("record target program checks");
        let record_body = record_program
            .resolved_body(&NodeId("function:main".into()))
            .expect("record target body");
        let record_ty = &record_body
            .locals
            .values()
            .find(|local| local.display_name == "label")
            .expect("Label local")
            .ty;
        assert!(ordinary_map_owned_any_v2_unpack_target_supported(
            record_program.resolved_types(),
            record_ty
        ));
        let bool_ty = string_program
            .resolved_types()
            .iter()
            .find_map(|(id, ty)| {
                matches!(
                    ty,
                    ResolvedType::Primitive(crate::core::PrimitiveType::Bool)
                )
                .then_some(id)
            })
            .expect("bool TypeDesc");
        assert!(!ordinary_map_owned_any_v2_unpack_target_supported(
            string_program.resolved_types(),
            bool_ty
        ));
    }

    #[test]
    fn ordinary_map_owned_any_v2_dynamic_place_input_transfer_blocks_pair_drop_receipt() {
        let source = parse(
            r#"
type Label { text: string }
func index_value<T>(value: T) -> i32 { 0 }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let pair = map_get(root, "label")
    let values = [0]
    values[index_value(pair.1)] = 1
    drop(pair)
    drop(empty)
    drop(root)
    drop(label)
    values[0]
}
"#,
        );
        let checked = crate::core::check_program(&source)
            .expect("an Any field may be passed through the dynamic place-input call");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("dynamic-index Map result analysis");
        assert!(
            analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none(),
            "transferring field 1 through a place-input expression must invalidate the pair Drop receipt"
        );
        assert!(
            analysis
                .ordinary_map_owned_any_v2_operation_attempts
                .iter()
                .any(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::GetAny),
            "the negative case must retain its V2 GetAny attempt even when the receipt is withheld"
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_default_input_get_any_blocks_otherwise_valid_receipt() {
        let source = parse(
            r#"
func index_value<T>(value: T) -> i32 { 0 }
func with_default(index: i32 = index_value(map_get(map_new(), "default").1)) -> i32 {
    let root = map_new()
    let pair = map_get(root, "body")
    drop(pair)
    drop(root)
    index
}
func main() -> i32 { 0 }
"#,
        );
        let checked = crate::core::check_program(&source)
            .expect("Map/Any operations in a callable default remain well-typed source");
        let analysis = checked
            .resource_analysis(&NodeId("function:with_default".into()))
            .expect("default-expression Map/Any resource analysis");
        assert!(
            analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none(),
            "a default-owned GetAny outside this callable's CFG must withhold its otherwise-valid body receipt"
        );
        assert!(
            analysis
                .ordinary_map_owned_any_v2_operation_attempts
                .iter()
                .any(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::GetAny),
            "the default expression's GetAny must be retained in the owner attempt census"
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_rejects_missing_double_and_unsettled_results() {
        let missing = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let _pair = map_get(root, "label")
    drop(empty)
    drop(root)
    drop(label)
    0
}
"#,
        );
        let checked = crate::core::check_program(&missing).expect("scope-only result checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("scope-only V2 analysis");
        assert!(analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none());
        assert!(!analysis.has_ordinary_map_owned_any_v2_receipt());

        let unsettled = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let pair = map_get(root, "label")
    let _found = pair.0
    drop(empty)
    drop(root)
    drop(label)
    0
}
"#,
        );
        let checked = crate::core::check_program(&unsettled).expect("observed result checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("observed V2 analysis");
        assert!(analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none());

        let double_drop = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let pair = map_get(root, "label")
    drop(pair)
    drop(pair)
    drop(empty)
    drop(root)
    drop(label)
    0
}
"#,
        );
        match crate::core::check_program(&double_drop) {
            Ok(checked) => {
                let analysis = checked
                    .resource_analysis(&NodeId("function:main".into()))
                    .expect("double-drop V2 analysis");
                assert!(analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none());
                assert!(!analysis.has_ordinary_map_owned_any_v2_receipt());
            }
            Err(errors) => assert!(
                !errors.is_empty(),
                "a source-level double settlement must be rejected"
            ),
        }
    }

    #[test]
    fn ordinary_map_owned_any_v2_accepts_string_locals_and_computed_keys() {
        let direct_key = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let key = "answer"
    let label = Label { text: "payload" }
    let empty = map_new()
    let updated = map_set(empty, key, label)
    let present = has_key(updated, key)
    let pair = map_get(updated, key)
    let removed = map_remove(updated, key)
    drop(pair)
    drop(empty)
    drop(updated)
    drop(removed)
    drop(label)
    drop(key)
    0
}
"#,
        );
        let checked = crate::core::check_program(&direct_key).expect("direct key type checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("direct key analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("direct key V2 receipt");
        let key_local = receipt
            .actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Set)
            .and_then(|action| action.key_local.as_ref())
            .expect("Set should record its borrowed local key");
        assert_eq!(
            receipt
                .actions
                .iter()
                .filter(|action| matches!(
                    action.kind,
                    OrdinaryMapOwnedAnyV2ActionKind::Set
                        | OrdinaryMapOwnedAnyV2ActionKind::Remove
                        | OrdinaryMapOwnedAnyV2ActionKind::HasKey
                        | OrdinaryMapOwnedAnyV2ActionKind::GetAny
                ))
                .count(),
            4
        );
        assert!(receipt
            .actions
            .iter()
            .filter(|action| {
                matches!(
                    action.kind,
                    OrdinaryMapOwnedAnyV2ActionKind::Set
                        | OrdinaryMapOwnedAnyV2ActionKind::Remove
                        | OrdinaryMapOwnedAnyV2ActionKind::HasKey
                        | OrdinaryMapOwnedAnyV2ActionKind::GetAny
                )
            })
            .all(|action| action.key_local.as_ref() == Some(key_local)));
        assert!(receipt
            .actions
            .iter()
            .filter(|action| matches!(
                action.kind,
                OrdinaryMapOwnedAnyV2ActionKind::Set
                    | OrdinaryMapOwnedAnyV2ActionKind::Remove
                    | OrdinaryMapOwnedAnyV2ActionKind::HasKey
                    | OrdinaryMapOwnedAnyV2ActionKind::GetAny
            ))
            .all(|action| action.key_provenance
                == Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::DirectLocal)));

        let computed_key = parse(
            r#"
type Label { text: string }
func main() -> i32 {
    let key = "answer"
    let label = Label { text: "payload" }
    let empty = map_new()
    let updated = map_set(empty, key + "\0雪", label)
    let present = has_key(updated, key + "\0雪")
    let pair = map_get(updated, key + "\0雪")
    let removed = map_remove(updated, key + "\0雪")
    drop(pair)
    drop(empty)
    drop(updated)
    drop(removed)
    drop(label)
    drop(key)
    0
}
"#,
        );
        let checked = crate::core::check_program(&computed_key)
            .expect("computed String key expressions type check");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("computed String key analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("computed String expressions earn V2 key receipts");
        assert_eq!(
            receipt
                .actions
                .iter()
                .filter(|action| matches!(
                    action.kind,
                    OrdinaryMapOwnedAnyV2ActionKind::Set
                        | OrdinaryMapOwnedAnyV2ActionKind::Remove
                        | OrdinaryMapOwnedAnyV2ActionKind::HasKey
                        | OrdinaryMapOwnedAnyV2ActionKind::GetAny
                ))
                .count(),
            4
        );
        assert!(receipt
            .actions
            .iter()
            .filter(|action| matches!(
                action.kind,
                OrdinaryMapOwnedAnyV2ActionKind::Set
                    | OrdinaryMapOwnedAnyV2ActionKind::Remove
                    | OrdinaryMapOwnedAnyV2ActionKind::HasKey
                    | OrdinaryMapOwnedAnyV2ActionKind::GetAny
            ))
            .all(|action| action.key_local.is_none()));
        assert!(receipt
            .actions
            .iter()
            .filter(|action| matches!(
                action.kind,
                OrdinaryMapOwnedAnyV2ActionKind::Set
                    | OrdinaryMapOwnedAnyV2ActionKind::Remove
                    | OrdinaryMapOwnedAnyV2ActionKind::HasKey
                    | OrdinaryMapOwnedAnyV2ActionKind::GetAny
            ))
            .all(|action| action.key_provenance
                == Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::ComputedString)));

        let get = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "answer")
    drop(root)
    drop(pair)
    0
}
"#,
        );
        let checked = crate::core::check_program(&get).expect("Get source type checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("Get analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("empty roots can certify their miss-only GetAny operation");
        assert!(analysis.has_ordinary_map_owned_any_v2_receipt());
        assert!(analysis
            .ordinary_map_owned_any_v2_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::GetAny));
        let get = receipt
            .actions
            .iter()
            .find(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::GetAny)
            .expect("empty-root GetAny action");
        assert_eq!(
            get.key_provenance,
            Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::StringLiteral)
        );
        assert_eq!(
            receipt.result_receipts[0].settlement,
            OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        );

        let unsupported_call = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let all_keys = keys(root)
    drop(root)
    drop(all_keys)
    0
}
"#,
        );
        let checked = crate::core::check_program(&unsupported_call)
            .expect("unsupported key snapshot remains type-correct");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("unsupported key snapshot analysis");
        assert!(!analysis.has_ordinary_map_owned_any_v2_receipt());
        assert!(analysis
            .ordinary_map_owned_any_v2_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::Keys));

        let invalid_key = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let present = has_key(root, 42)
    drop(root)
    0
}
"#,
        );
        match crate::core::check_program(&invalid_key) {
            Err(errors) => assert!(!errors.is_empty()),
            Ok(checked) => {
                let analysis = checked
                    .resource_analysis(&NodeId("function:main".into()))
                    .expect("invalid key analysis");
                assert!(!analysis.has_ordinary_map_owned_any_v2_receipt());
                assert!(analysis
                    .ordinary_map_owned_any_v2_operation_attempts
                    .iter()
                    .any(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::HasKey));
            }
        }
    }

    #[test]
    fn ordinary_map_owned_any_v2_does_not_claim_empty_roots_without_any_use() {
        let empty = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    drop(root)
    0
}
"#,
        );
        let checked = crate::core::check_program(&empty).expect("empty Map root checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("empty root resource analysis");
        assert!(analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none());
        assert!(analysis.ordinary_map_owned_any_v2_actions.is_empty());
        assert!(!analysis.has_ordinary_map_owned_any_v2_receipt());
        assert!(analysis
            .ordinary_map_owned_any_v2_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::New));
        assert!(analysis
            .ordinary_map_owned_any_v2_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::Drop));

        let unsettled = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    0
}
"#,
        );
        let checked = crate::core::check_program(&unsettled).expect("unsettled root checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("unsettled root resource analysis");
        assert!(analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none());
        assert!(!analysis.has_ordinary_map_owned_any_v2_receipt());
    }

    #[test]
    fn ordinary_map_owned_any_v2_keeps_primitive_only_roots_on_legacy_profile() {
        let source = parse(
            r#"
func main() -> i32 {
    let empty = map_new()
    let one = map_set(empty, "key", 1)
    let count = map_size(one)
    drop(empty)
    drop(one)
    count
}
"#,
        );
        let checked = crate::core::check_program(&source).expect("primitive Map checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("primitive Map resource analysis");
        assert!(analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none());
        assert!(analysis.ordinary_map_owned_any_v2_actions.is_empty());
        assert!(!analysis.has_ordinary_map_owned_any_v2_receipt());
        assert!(analysis
            .ordinary_map_owned_any_v2_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::New));
        assert!(analysis
            .ordinary_map_owned_any_v2_operation_attempts
            .iter()
            .any(|attempt| attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::Set));
    }

    #[test]
    fn ordinary_map_owned_any_v2_empty_get_and_values_have_exact_result_receipts() {
        let source = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "\0雪")
    let snapshot = values(root)
    drop(pair)
    drop(snapshot)
    drop(root)
    0
}
"#,
        );
        let checked = crate::core::check_program(&source).expect("empty-root queries check");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("empty-root query analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("empty-root queries have a V2 receipt");
        assert_eq!(
            receipt
                .actions
                .iter()
                .filter(|action| matches!(
                    action.kind,
                    OrdinaryMapOwnedAnyV2ActionKind::GetAny
                        | OrdinaryMapOwnedAnyV2ActionKind::Values
                ))
                .count(),
            2
        );
        assert_eq!(receipt.result_receipts.len(), 2);
        assert!(receipt
            .result_receipts
            .iter()
            .all(|result| result.settlement
                == OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop));
    }

    #[test]
    fn ordinary_map_owned_any_v2_receipts_direct_temporary_empty_get_any_root() {
        let source = parse(
            r#"
func main() -> i32 {
    let pair = map_get(map_new(), "missing")
    drop(pair)
    0
}
"#,
        );
        let checked = crate::core::check_program(&source).expect("temporary-root GetAny checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("temporary-root resource analysis");
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("temporary-root GetAny has a complete V2 receipt");
        let [root] = receipt.temporary_root_receipts.as_slice() else {
            panic!("the immediate map_new source has one compiler-owned root receipt");
        };
        assert_eq!(root.construction, root.source_node);
        assert_eq!(root.root_type, root.source_type);
        assert_eq!(
            root.settlement,
            OrdinaryMapOwnedAnyV2RootSettlementKind::ImmediateGetAnyDrop
        );
        assert_eq!(root.get_any, root.settlement_point);
        let actions = receipt
            .actions
            .iter()
            .filter(|action| action.local == root.local)
            .collect::<Vec<_>>();
        assert_eq!(
            actions.iter().map(|action| action.kind).collect::<Vec<_>>(),
            vec![
                OrdinaryMapOwnedAnyV2ActionKind::New,
                OrdinaryMapOwnedAnyV2ActionKind::GetAny,
                OrdinaryMapOwnedAnyV2ActionKind::Drop,
            ]
        );
        assert!(!receipt.attempts.iter().any(|attempt| {
            attempt.kind == OrdinaryMapOwnedAnyV2AttemptKind::ScopeDrop
                && attempt.receiver_local.as_ref() == Some(&root.local)
        }));
        assert_eq!(receipt.result_receipts.len(), 1);
        assert_eq!(
            receipt.result_receipts[0].settlement,
            OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_if_diamond_carries_equal_root_and_result_owners_to_join() {
        let source = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "missing")
    let snapshot = values(root)
    let flag = true
    let observed = if flag {
        map_size(root)
    } else {
        map_size(root)
    }
    drop(pair)
    drop(snapshot)
    drop(root)
    observed
}
"#,
        );
        let checked = crate::core::check_program(&source).expect("V2 if diamond checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("V2 if diamond ownership analysis");
        let cfg = checked
            .callable_cfg(&NodeId("function:main".into()))
            .expect("V2 if diamond source CFG");
        assert!(
            analysis.ordinary_map_owned_any_v2_cfg_receipt.is_some(),
            "balanced V2 if diamond has a Checker receipt; cfg={cfg:#?}; actions={:#?}; attempts={:#?}",
            analysis.ordinary_map_owned_any_v2_actions,
            analysis.ordinary_map_owned_any_v2_operation_attempts
        );
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("receipt was checked above");
        let flow = receipt.flow.as_ref().expect("diamond has V2 edge facts");
        assert_eq!(flow.reachable_blocks.len(), 4);
        assert_eq!(flow.arm_edges.len(), 2);
        let split = &flow.live_owners_out[&flow.split];
        assert_eq!(split.len(), 3, "one root plus GetAny and Values owners");
        assert!(split.iter().any(|owner| matches!(
            owner,
            crate::core::OrdinaryMapOwnedAnyV2LiveOwner::Root { .. }
        )));
        assert_eq!(
            split
                .iter()
                .filter(|owner| matches!(
                    owner,
                    crate::core::OrdinaryMapOwnedAnyV2LiveOwner::Result { .. }
                ))
                .count(),
            2
        );
        for edge in &flow.arm_edges {
            assert_eq!(&flow.live_owners_on_edge[edge], split);
            let target = &flow.reachable_edges[edge].to;
            assert_eq!(&flow.live_owners_in[target], split);
        }
        assert!(flow.live_owners_out[&flow.join].is_empty());
        assert!(receipt
            .result_receipts
            .iter()
            .all(|result| result.settlement
                == OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                && flow.reachable_blocks[&flow.join]
                    .points
                    .iter()
                    .any(|(point, _)| Some(point) == result.settlement_point.as_ref())));
    }

    #[test]
    fn ordinary_map_owned_any_v2_match_diamond_carries_equal_root_and_result_owners_to_join() {
        let source = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "missing")
    let snapshot = values(root)
    let flag = true
    let observed = match flag {
        true => { map_size(root) },
        false => { map_size(root) },
    }
    drop(pair)
    drop(snapshot)
    drop(root)
    observed
}
"#,
        );
        let checked = crate::core::check_program(&source).expect("V2 match diamond checks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("V2 match diamond ownership analysis");
        let cfg = checked
            .callable_cfg(&NodeId("function:main".into()))
            .expect("V2 match diamond source CFG");
        assert!(
            OrdinaryMapOwnedAnyV2Checker::ordinary_map_owned_any_v2_diamond_shape(cfg).is_some(),
            "two-arm Match shape is the candidate CFG topology"
        );
        assert!(
            analysis.ordinary_map_owned_any_v2_cfg_receipt.is_some(),
            "balanced V2 match diamond has a Checker receipt; cfg={cfg:#?}; actions={:#?}; attempts={:#?}",
            analysis.ordinary_map_owned_any_v2_actions,
            analysis.ordinary_map_owned_any_v2_operation_attempts
        );
        let receipt = analysis
            .ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .expect("receipt was checked above");
        let flow = receipt.flow.as_ref().expect("match has V2 edge facts");
        assert_eq!(flow.reachable_blocks.len(), 4);
        assert_eq!(flow.arm_edges.len(), 2);
        let split = &flow.live_owners_out[&flow.split];
        assert_eq!(split.len(), 3, "one root plus GetAny and Values owners");
        assert!(split.iter().any(|owner| matches!(
            owner,
            crate::core::OrdinaryMapOwnedAnyV2LiveOwner::Root { .. }
        )));
        assert_eq!(
            split
                .iter()
                .filter(|owner| matches!(
                    owner,
                    crate::core::OrdinaryMapOwnedAnyV2LiveOwner::Result { .. }
                ))
                .count(),
            2
        );
        for edge in &flow.arm_edges {
            assert_eq!(&flow.live_owners_on_edge[edge], split);
            let target = &flow.reachable_edges[edge].to;
            assert_eq!(&flow.live_owners_in[target], split);
        }
        assert!(flow.live_owners_out[&flow.join].is_empty());
        assert!(receipt
            .result_receipts
            .iter()
            .all(|result| result.settlement
                == OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                && flow.reachable_blocks[&flow.join]
                    .points
                    .iter()
                    .any(|(point, _)| Some(point) == result.settlement_point.as_ref())));
    }

    #[test]
    fn ordinary_map_owned_any_v2_rejects_branch_owner_actions_mismatches_and_cycles() {
        let branch_owner_action = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "missing")
    let flag = true
    let observed = if flag {
        let branch_root = map_new()
        drop(branch_root)
        map_size(root)
    } else {
        map_size(root)
    }
    drop(pair)
    drop(root)
    observed
}
"#,
        );
        let checked = crate::core::check_program(&branch_owner_action)
            .expect("balanced branch-local owner action typechecks before V2 admission");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("branch owner-action analysis");
        assert!(analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none());

        let one_arm_result_drop = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "missing")
    let flag = true
    if flag {
        drop(pair)
    } else {
        map_size(root)
    }
    drop(root)
    0
}
"#,
        );
        let checked = crate::core::check_program(&one_arm_result_drop)
            .expect("one-arm settlement is a source program before V2 admission");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("one-arm settlement analysis");
        assert!(
            analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none(),
            "one-arm result settlement cannot reach the common owner join"
        );

        let looped = parse(
            r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "missing")
    let flag = false
    while flag {
        map_size(root)
        break
    }
    drop(pair)
    drop(root)
    0
}
"#,
        );
        let checked = crate::core::check_program(&looped).expect("read-only loop typechecks");
        let analysis = checked
            .resource_analysis(&NodeId("function:main".into()))
            .expect("looped V2 analysis");
        assert!(analysis.ordinary_map_owned_any_v2_cfg_receipt.is_none());
    }
}
