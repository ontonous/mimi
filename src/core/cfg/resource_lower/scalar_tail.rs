//! Source authority for an entry-closed Map/Any phase followed by a finite
//! pure scalar suffix. This does not authorize a Map operation or route.
use super::*;
use crate::core::cfg::{BasicBlockId, CfgPointKind};
use crate::core::ownership::{
    OrdinaryMapOwnedAnyV2ScalarTailNode as ScalarNode,
    OrdinaryMapOwnedAnyV2ScalarTailNodeKind as ScalarKind,
    OrdinaryMapOwnedAnyV2ScalarTailReceipt as Receipt,
    OrdinaryMapOwnedAnyV2ScalarTailSelector as Selector,
};

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    checked_record_field_types: &BTreeMap<NodeId, Vec<(String, ResolvedTypeId)>>,
    zero_miss: Option<&crate::core::OrdinaryMapOwnedAnyV2ZeroMissI32ObservationReceipt>,
) -> Option<Receipt> {
    if cfg.owner != body.owner || !body.captures.is_empty() {
        return None;
    }
    let scope = OrdinaryMapV2LinearChainScope::collect(cfg, body, types)?;
    let mut prefix_blocks = BTreeSet::new();
    let mut block_spine_order = BTreeMap::new();
    let mut step_joins = Vec::with_capacity(scope.steps.len());
    let mut current_block = cfg.entry.clone();
    block_spine_order.insert(current_block.clone(), 0usize);
    for (step_idx, step) in scope.steps.iter().enumerate() {
        let (dispatch, _, _, ok_entry, _, step_join) = ordinary_map_result_branch_edges(
            cfg,
            &step.scrutinee.node_id,
            &step.match_expr.node_id,
            &step.ok_arm.node_id,
            &step.err_arm.node_id,
        )?;
        if dispatch != current_block {
            return None;
        }
        for err_stmt in step.err_statements {
            let ResolvedStmtKind::Drop(places) = &err_stmt.kind else {
                return None;
            };
            if places.iter().any(|place| !place.projections.is_empty()) {
                return None;
            }
        }
        let err_value = match &step.err_arm.body.kind {
            ResolvedExprKind::Block(block) => block.result.as_deref()?,
            _ => &step.err_arm.body,
        };
        if !err_value.effects.is_empty()
            || !matches!(
                types.get(&err_value.ty),
                Some(ResolvedType::Primitive(
                    crate::core::PrimitiveType::I32 | crate::core::PrimitiveType::Bool
                ))
            )
            || !match &err_value.kind {
                ResolvedExprKind::Literal(ResolvedLiteral::Int(_) | ResolvedLiteral::Bool(_)) => {
                    true
                }
                ResolvedExprKind::Load(place) => place.projections.is_empty(),
                _ => false,
            }
        {
            return None;
        }
        prefix_blocks.insert(current_block);
        step_joins.push((step_join, step.match_expr.node_id.clone()));
        current_block = ok_entry;
        block_spine_order.insert(current_block.clone(), step_idx + 1);
    }
    let split_block = current_block;
    let Terminator::Branch { condition, .. } = &cfg.blocks.get(&split_block)?.terminator else {
        return None;
    };
    let mut points = BTreeMap::new();
    for (block_id, block) in &cfg.blocks {
        for (rank, point) in block.points.iter().enumerate() {
            if points
                .insert(
                    point.source.node.clone(),
                    (block_id.clone(), rank, point.kind),
                )
                .is_some()
            {
                return None;
            }
        }
    }
    let (first_block, first_rank, first_kind) = points.get(condition)?;
    if first_block != &split_block || *first_kind != CfgPointKind::Condition {
        return None;
    }
    let before = |node: &NodeId| {
        points.get(node).is_some_and(|(block, rank, _)| {
            prefix_blocks.contains(block) || (block == &split_block && rank < first_rank)
        })
    };
    let result = scope.leaf_result?;
    let (return_block, _, _) = points.get(&result.node_id)?;
    if scope.is_straight_line {
        if !matches!(&cfg.blocks.get(return_block)?.terminator,
            Terminator::Return { value: Some(node), implicit: true } if node == &result.node_id)
        {
            return None;
        }
    } else {
        let mut cursor = return_block.clone();
        for (expected_join, match_node) in step_joins.iter().rev() {
            let Terminator::Goto { edge } = &cfg.blocks.get(&cursor)?.terminator else {
                return None;
            };
            let cfg_edge = cfg.edges.get(edge)?;
            if &cfg_edge.to != expected_join {
                return None;
            }
            let join_block = cfg.blocks.get(expected_join)?;
            if !join_block
                .points
                .iter()
                .any(|point| &point.source.node == match_node)
            {
                return None;
            }
            cursor = expected_join.clone();
        }
        let Some((outer_join, outer_match)) = step_joins.first() else {
            return None;
        };
        if &cursor != outer_join
            || !matches!(&cfg.blocks.get(&cursor)?.terminator,
                Terminator::Return { value: Some(node), implicit: true } if node == outer_match)
        {
            return None;
        }
    }
    let mut receipt = Receipt {
        owner: body.owner.clone(),
        split_block: split_block.clone(),
        first_condition: condition.clone(),
        first_condition_rank: *first_rank,
        nodes: BTreeMap::new(),
        bindings: BTreeMap::new(),
        selectors: BTreeMap::new(),
        owned_uses: BTreeMap::new(),
        string_drops: BTreeMap::new(),
        zero_miss_stdout: None,
        return_node: result.node_id.clone(),
        return_type: result.ty.clone(),
        return_block: return_block.clone(),
    };
    let mut collector = Collector {
        cfg,
        body,
        types,
        points: &points,
        receipt: &mut receipt,
    };
    // Prefix statements before the split remain the existing V2 collector's
    // responsibility. A statement crossing the split is scalar in its entirety.
    for statement in scope.leaf_statements {
        if matches!(
            &statement.kind,
            ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_)
        ) {
            continue;
        }
        if !before(&statement.node_id) {
            if let (
                Some(observation),
                ResolvedStmtKind::Expr(ResolvedExpr {
                    node_id,
                    kind: ResolvedExprKind::Call(call),
                    ..
                }),
            ) = (zero_miss, &statement.kind)
            {
                if observation.owner != body.owner
                    || observation.return_node != result.node_id
                    || !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "println")
                    || call.arguments.len() != 1
                    || collector.receipt.zero_miss_stdout.is_some()
                {
                    return None;
                }
                let argument = &call.arguments[0].value;
                if !matches!((&argument.kind, &result.kind),
                    (ResolvedExprKind::Load(a), ResolvedExprKind::Load(b))
                        if a.projections.is_empty() && a == b)
                {
                    return None;
                }
                collector.expression(argument)?;
                collector.receipt.zero_miss_stdout =
                    Some((node_id.clone(), argument.node_id.clone()));
                continue;
            }
            collector.statement(statement)?;
        }
    }
    collector.expression(result)?;
    if collector.receipt.selectors.len()
        != cfg
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, Terminator::Branch { .. }))
            .count()
    {
        return None;
    }

    // Every non-Copy source local has an explicit prefix settlement, or the
    // single-field transfer form whose child is itself explicitly settled.
    // This covers unpack payloads as well as roots/results/owned String keys.
    for (local, binding) in &body.locals {
        let copy = matches!(types.get(&binding.ty), Some(ResolvedType::Primitive(primitive))
            if *primitive != crate::core::PrimitiveType::String);
        if copy {
            continue;
        }
        let census = scope.local_source_uses(body, local);
        if census.has_nested_callable
            || !census.captures.is_empty()
            || !census.has_unique_load_nodes()
            || census.loads.iter().any(|load| !before(&load.node))
            || census.places.iter().any(|place| !before(&place.statement))
        {
            return None;
        }
        let explicit_drop = matches!(census.places.as_slice(), [place]
            if place.kind == OrdinaryMapOwnedAnyV2LocalPlaceUseKind::Drop && place.unprojected);
        if explicit_drop {
            let drop = &census.places[0];
            let (drop_block, drop_rank, drop_kind) = points.get(&drop.statement)?;
            let source_drop = scope
                .combined_statements
                .iter()
                .copied()
                .find(|statement| statement.node_id == drop.statement)?;
            if (!prefix_blocks.contains(drop_block) && drop_block != &split_block)
                || (drop_block == &split_block && drop_rank >= first_rank)
                || *drop_kind != CfgPointKind::ResourceAction
                || !matches!(&source_drop.kind, ResolvedStmtKind::Drop(places)
                    if places.get(drop.place_index).is_some_and(|place| place.base == *local && place.projections.is_empty()))
            {
                return None;
            }
            let block_point_before =
                |block_a: &BasicBlockId, rank_a: usize, block_b: &BasicBlockId, rank_b: usize| {
                    matches!(
                        (block_spine_order.get(block_a), block_spine_order.get(block_b)),
                        (Some(order_a), Some(order_b)) if (order_a, rank_a) < (order_b, rank_b)
                    )
                };
            let mut loads = Vec::new();
            for load in &census.loads {
                let (block, rank, _) = points.get(&load.node)?;
                if !block_point_before(block, *rank, drop_block, *drop_rank) {
                    return None;
                }
                loads.push((load.node.clone(), *rank));
            }
            loads.sort_by_key(|(_, rank)| *rank);
            if matches!(
                types.get(&binding.ty),
                Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
            ) {
                receipt.string_drops.insert(
                    local.clone(),
                    crate::core::ownership::OrdinaryMapOwnedAnyV2ScalarTailStringDrop {
                        ty: binding.ty.clone(),
                        point_block: drop_block.clone(),
                        point: drop.statement.clone(),
                        place_index: drop.place_index,
                        point_rank: *drop_rank,
                        loads,
                    },
                );
            }
        }
        let single_string_record = match types.get(&binding.ty) {
            Some(ResolvedType::Nominal { item, arguments, .. }) if arguments.is_empty() => {
                checked_record_field_types.get(&NodeId(item.as_str().to_owned())).is_some_and(|fields| {
                    matches!(fields.as_slice(), [(name, ty)] if name == "text"
                        && matches!(types.get(ty), Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))))
                })
            }
            _ => false,
        };
        let field_transfer = if census.places.is_empty() && single_string_record {
            match census.loads.as_slice() {
                [usage] if !usage.unprojected && usage.direct_field.is_some() => {
                    scope.combined_statements.iter().copied().any(|statement| {
                        let ResolvedStmtKind::Bind {
                            pattern:
                                ResolvedPattern {
                                    kind:
                                        ResolvedPatternKind::Binding {
                                            local: child,
                                            by_reference: None,
                                        },
                                    ..
                                },
                            initializer: Some(value),
                        } = &statement.kind
                        else {
                            return false;
                        };
                        value.node_id == usage.node
                            && before(&statement.node_id)
                            && matches!(
                                types.get(&value.ty),
                                Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
                            )
                            && body
                                .locals
                                .get(child)
                                .is_some_and(|binding| binding.ty == value.ty)
                    })
                }
                _ => false,
            }
        } else {
            false
        };
        if !explicit_drop && !field_transfer {
            return None;
        }
        let uses = census
            .loads
            .iter()
            .map(|load| load.node.clone())
            .chain(census.places.iter().map(|place| place.statement.clone()))
            .collect();
        receipt.owned_uses.insert(local.clone(), uses);
    }
    Some(receipt)
}

struct Collector<'a> {
    cfg: &'a CallableCfg,
    body: &'a ResolvedBody,
    types: &'a ResolvedTypeTable,
    points: &'a BTreeMap<NodeId, (BasicBlockId, usize, CfgPointKind)>,
    receipt: &'a mut Receipt,
}

impl Collector<'_> {
    fn scalar(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Primitive(
                crate::core::PrimitiveType::I32 | crate::core::PrimitiveType::Bool
            ))
        )
    }

    fn statement(&mut self, statement: &ResolvedStmt) -> Option<()> {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern:
                    ResolvedPattern {
                        kind:
                            ResolvedPatternKind::Binding {
                                local,
                                by_reference: None,
                            },
                        ..
                    },
                initializer: Some(value),
            } => {
                let binding = self.body.locals.get(local)?;
                if binding.mutable
                    || binding.ty != value.ty
                    || !self.scalar(&binding.ty)
                    || self
                        .receipt
                        .bindings
                        .insert(
                            local.clone(),
                            (statement.node_id.clone(), value.node_id.clone()),
                        )
                        .is_some()
                {
                    return None;
                }
                self.expression(value)
            }
            ResolvedStmtKind::Expr(value) => {
                if let ResolvedExprKind::If {
                    condition,
                    then_block,
                    else_block,
                } = &value.kind
                {
                    if matches!(
                        self.types.get(&value.ty),
                        Some(ResolvedType::Primitive(crate::core::PrimitiveType::Unit))
                    ) && value.effects.is_empty()
                        && then_block.result.is_none()
                        && else_block.result.is_none()
                    {
                        self.expression(condition)?;
                        for stmt in &then_block.statements {
                            self.statement(stmt)?;
                        }
                        for stmt in &else_block.statements {
                            self.statement(stmt)?;
                        }
                        self.selector(
                            &value.node_id,
                            &condition.node_id,
                            &then_block.node_id,
                            &else_block.node_id,
                            then_block.node_id.clone(),
                            else_block.node_id.clone(),
                        )?;
                        if !self.points.contains_key(&value.node_id)
                            || self
                                .receipt
                                .nodes
                                .insert(
                                    value.node_id.clone(),
                                    ScalarNode {
                                        ty: value.ty.clone(),
                                        kind: ScalarKind::StatementJoin,
                                    },
                                )
                                .is_some()
                        {
                            return None;
                        }
                        return Some(());
                    }
                }
                self.expression(value)
            }
            _ => None,
        }
    }

    fn block(&mut self, block: &ResolvedBlock) -> Option<NodeId> {
        for statement in &block.statements {
            self.statement(statement)?;
        }
        let value = block.result.as_deref()?;
        self.expression(value)?;
        Some(value.node_id.clone())
    }

    fn expression(&mut self, expression: &ResolvedExpr) -> Option<()> {
        use ResolvedBinaryOp as Binary;
        use ResolvedExprKind as Expr;
        if !self.scalar(&expression.ty) || !expression.effects.is_empty() {
            return None;
        }
        let kind = match &expression.kind {
            Expr::Literal(literal @ (ResolvedLiteral::Int(_) | ResolvedLiteral::Bool(_))) => {
                ScalarKind::Literal(literal.clone())
            }
            Expr::Load(place) if place.projections.is_empty() => {
                let binding = self.body.locals.get(&place.base)?;
                if binding.mutable || binding.ty != expression.ty {
                    return None;
                }
                ScalarKind::Load(place.base.clone())
            }
            Expr::Unary { op, operand }
                if matches!(op, ResolvedUnaryOp::Not | ResolvedUnaryOp::Negate) =>
            {
                self.expression(operand)?;
                ScalarKind::Unary(*op, operand.node_id.clone())
            }
            Expr::Binary { op, left, right }
                if matches!(
                    op,
                    Binary::Equal | Binary::NotEqual | Binary::Add | Binary::Subtract
                ) =>
            {
                self.expression(left)?;
                self.expression(right)?;
                ScalarKind::Binary(*op, left.node_id.clone(), right.node_id.clone())
            }
            Expr::Binary { op, left, right }
                if matches!(op, Binary::LogicalAnd | Binary::LogicalOr) =>
            {
                self.expression(left)?;
                self.expression(right)?;
                let shortcut = NodeId(format!("{}/short-circuit", expression.node_id.0));
                self.receipt.nodes.insert(
                    shortcut.clone(),
                    ScalarNode {
                        ty: expression.ty.clone(),
                        kind: ScalarKind::Literal(ResolvedLiteral::Bool(*op == Binary::LogicalOr)),
                    },
                );
                let (then_node, else_node) = if *op == Binary::LogicalAnd {
                    (right.node_id.clone(), shortcut)
                } else {
                    (shortcut, right.node_id.clone())
                };
                self.selector(
                    &expression.node_id,
                    &left.node_id,
                    &then_node,
                    &else_node,
                    then_node.clone(),
                    else_node.clone(),
                )?;
                ScalarKind::Join
            }
            Expr::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expression(condition)?;
                let then_value = self.block(then_block)?;
                let else_value = self.block(else_block)?;
                self.selector(
                    &expression.node_id,
                    &condition.node_id,
                    &then_block.node_id,
                    &else_block.node_id,
                    then_value,
                    else_value,
                )?;
                ScalarKind::Join
            }
            _ => return None,
        };
        if !self.points.contains_key(&expression.node_id)
            || self
                .receipt
                .nodes
                .insert(
                    expression.node_id.clone(),
                    ScalarNode {
                        ty: expression.ty.clone(),
                        kind,
                    },
                )
                .is_some()
        {
            return None;
        }
        Some(())
    }

    fn selector(
        &mut self,
        construct: &NodeId,
        condition: &NodeId,
        then_source: &NodeId,
        else_source: &NodeId,
        then_value: NodeId,
        else_value: NodeId,
    ) -> Option<()> {
        let (block_id, _, kind) = self.points.get(condition)?;
        if *kind != CfgPointKind::Condition {
            return None;
        }
        let block = self.cfg.blocks.get(block_id)?;
        let Terminator::Branch {
            condition: actual,
            then_edge,
            else_edge,
        } = &block.terminator
        else {
            return None;
        };
        let then = self.cfg.edges.get(then_edge)?;
        let otherwise = self.cfg.edges.get(else_edge)?;
        if actual != condition
            || then.from != *block_id
            || otherwise.from != *block_id
            || then.kind != EdgeKind::Then
            || otherwise.kind != EdgeKind::Else
            || then.source.node != *construct
            || otherwise.source.node != *construct
            || then.to.0 .0 != format!("{}/cfg:block:if-then", then_source.0)
            || otherwise.to.0 .0 != format!("{}/cfg:block:if-else", else_source.0)
        {
            return None;
        }
        let join = BasicBlockId(NodeId(format!("{}/cfg:block:if-join", construct.0)));
        if self.points.get(construct).map(|(block, _, _)| block) != Some(&join) {
            return None;
        }
        for source in [then_source, else_source] {
            let edges = self
                .cfg
                .edges
                .values()
                .filter(|edge| edge.to == join && &edge.source.node == source)
                .collect::<Vec<_>>();
            if edges.len() != 1
                || edges[0].kind != EdgeKind::Fallthrough
                || !matches!(&self.cfg.blocks.get(&edges[0].from)?.terminator, Terminator::Goto { edge } if edge == &edges[0].id)
            {
                return None;
            }
        }
        if self
            .receipt
            .selectors
            .insert(
                construct.clone(),
                Selector {
                    selector: crate::core::OrdinaryMapOwnedAnyV2BranchSelectorReceipt {
                        block: block_id.clone(),
                        condition: condition.clone(),
                        then_edge: then_edge.clone(),
                        then_target: then.to.clone(),
                        else_edge: else_edge.clone(),
                        else_target: otherwise.to.clone(),
                    },
                    then_value,
                    else_value,
                    then_source: then_source.clone(),
                    else_source: else_source.clone(),
                },
            )
            .is_some()
        {
            return None;
        }
        Some(())
    }
}
