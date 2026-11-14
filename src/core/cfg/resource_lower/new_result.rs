//! Source authority for the first recoverable ordinary Map New.
//!
//! This is independent of the unconditional, bare-root V2 action ledger.
//! In particular, an Err edge must never inherit the Ok root obligation.
//! Until executable consumers bind this receipt to their Result operations,
//! collecting it grants no MIR route or backend capability.

use super::*;
use crate::core::cfg::{BasicBlock, BasicBlockId, CfgPoint, CfgPointKind, EdgeId};
use crate::core::ir::{CheckedConversionKind, PrimitiveType};
use crate::core::ownership::{ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2NewResultReceipt};
use crate::core::{LocalId, Origin, Place};

struct ExpectedPoint {
    node: NodeId,
    origin: Origin,
    kind: CfgPointKind,
    uses: Vec<String>,
    defs: Vec<String>,
    reads: Vec<String>,
    writes: Vec<String>,
    read_places: Vec<Place>,
    write_places: Vec<Place>,
}

impl ExpectedPoint {
    fn empty(node: &NodeId, origin: &Origin, kind: CfgPointKind) -> Self {
        Self {
            node: node.clone(),
            origin: origin.clone(),
            kind,
            uses: Vec::new(),
            defs: Vec::new(),
            reads: Vec::new(),
            writes: Vec::new(),
            read_places: Vec::new(),
            write_places: Vec::new(),
        }
    }

    fn local_binding(
        body: &ResolvedBody,
        node: &NodeId,
        origin: &Origin,
        local: &ResolvedLocalId,
    ) -> Self {
        let mut expected = Self::empty(node, origin, CfgPointKind::Binding);
        expected.defs.push(local_name(body, local));
        expected
    }

    fn local_load(
        body: &ResolvedBody,
        expression: &ResolvedExpr,
        local: &ResolvedLocalId,
        kind: CfgPointKind,
    ) -> Self {
        let mut expected = Self::empty(&expression.node_id, &expression.origin, kind);
        let place = root_place(body, local);
        expected.uses.push(place.base_name.clone());
        expected.reads.push(place.display());
        expected.read_places.push(place);
        expected
    }

    fn drop_place(body: &ResolvedBody, statement: &ResolvedStmt, place: &ResolvedPlace) -> Self {
        let mut expected = Self::empty(
            &statement.node_id,
            &statement.origin,
            CfgPointKind::ResourceAction,
        );
        let place = canonical_place(body, place);
        expected.reads.push(place.display());
        expected.read_places.push(place);
        expected
    }
}

fn local_name(body: &ResolvedBody, local: &ResolvedLocalId) -> String {
    body.locals
        .get(local)
        .map(|binding| binding.display_name.clone())
        .unwrap_or_else(|| local.0 .0.clone())
}

fn root_place(body: &ResolvedBody, local: &ResolvedLocalId) -> Place {
    Place::root(LocalId(local.0.clone()), local_name(body, local))
}

fn canonical_place(body: &ResolvedBody, place: &ResolvedPlace) -> Place {
    // The only places admitted by this receipt are direct local loads and the
    // unprojected root Drop. Keep this conversion deliberately exact so the
    // point census also checks stable place identity and display metadata.
    Place::root(LocalId(place.base.0.clone()), local_name(body, &place.base))
}

fn point_matches(point: &CfgPoint, expected: &ExpectedPoint) -> bool {
    point.source.node == expected.node
        && point.source.origin == expected.origin
        && point.source.span == expected.origin.user_span()
        && point.kind == expected.kind
        && point.uses == expected.uses
        && point.defs == expected.defs
        && point.reads == expected.reads
        && point.writes == expected.writes
        && point.read_places == expected.read_places
        && point.write_places == expected.write_places
}

fn exact_point_census(
    block: &BasicBlock,
    expected: &[ExpectedPoint],
    contract_nodes: &BTreeSet<NodeId>,
) -> bool {
    let actual = block
        .points
        .iter()
        .filter(|point| !contract_nodes.contains(&point.source.node))
        .collect::<Vec<_>>();
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| point_matches(actual, expected))
}

pub(super) fn direct_proof_expression_nodes(body: &ResolvedBody) -> BTreeSet<NodeId> {
    let mut nodes = direct_contract_expression_nodes(body);
    nodes.extend(body.root.statements.iter().flat_map(|statement| {
        match &statement.kind {
            ResolvedStmtKind::Math(expressions) => expressions
                .iter()
                .flat_map(ResolvedExpr::eager_source_nodes)
                .collect(),
            _ => Vec::new(),
        }
    }));
    nodes
}

pub(super) fn direct_contract_expression_nodes(body: &ResolvedBody) -> BTreeSet<NodeId> {
    let mut nodes = BTreeSet::new();
    for statement in &body.root.statements {
        let ResolvedStmtKind::Contract { condition, .. } = &statement.kind else {
            continue;
        };
        nodes.extend(condition.eager_source_nodes());
        add_contract_short_circuit_nodes(condition, &mut nodes);
    }
    nodes
}

fn add_contract_short_circuit_nodes(expression: &ResolvedExpr, nodes: &mut BTreeSet<NodeId>) {
    match &expression.kind {
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let ResolvedFStringPart::Interpolation(value) = part {
                    add_contract_short_circuit_nodes(value, nodes);
                }
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            add_contract_short_circuit_nodes(value, nodes);
            if let ResolvedValueProjection::Index(index) = projection {
                add_contract_short_circuit_nodes(index, nodes);
            }
        }
        ResolvedExprKind::Binary { op, left, right } => {
            if matches!(
                op,
                ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr
            ) {
                nodes.insert(NodeId(format!("{}/short-circuit", expression.node_id.0)));
            }
            add_contract_short_circuit_nodes(left, nodes);
            add_contract_short_circuit_nodes(right, nodes);
        }
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::OptionalChain {
            receiver: operand, ..
        }
        | ResolvedExprKind::Try { value: operand, .. }
        | ResolvedExprKind::Cast { value: operand, .. }
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand) => add_contract_short_circuit_nodes(operand, nodes),
        ResolvedExprKind::Call(call) => {
            for argument in &call.arguments {
                add_contract_short_circuit_nodes(&argument.value, nodes);
            }
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => {
            for value in values {
                add_contract_short_circuit_nodes(value, nodes);
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                add_contract_short_circuit_nodes(key, nodes);
                add_contract_short_circuit_nodes(value, nodes);
            }
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            add_contract_short_circuit_nodes(value, nodes);
            add_contract_short_circuit_nodes(iterable, nodes);
            if let Some(guard) = guard {
                add_contract_short_circuit_nodes(guard, nodes);
            }
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                add_contract_short_circuit_nodes(&field.value, nodes);
            }
            if let Some(rest) = rest {
                add_contract_short_circuit_nodes(rest, nodes);
            }
        }
        ResolvedExprKind::Range { start, end } => {
            add_contract_short_circuit_nodes(start, nodes);
            add_contract_short_circuit_nodes(end, nodes);
        }
        ResolvedExprKind::Slice { target, start, end } => {
            add_contract_short_circuit_nodes(target, nodes);
            if let Some(start) = start {
                add_contract_short_circuit_nodes(start, nodes);
            }
            if let Some(end) = end {
                add_contract_short_circuit_nodes(end, nodes);
            }
        }
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::TypeOf(_)
        | ResolvedExprKind::Old(_)
        | ResolvedExprKind::Block(_)
        | ResolvedExprKind::Scope { .. }
        | ResolvedExprKind::Comptime(_)
        | ResolvedExprKind::If { .. }
        | ResolvedExprKind::Match { .. }
        | ResolvedExprKind::Lambda(_)
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::Quote(_)
        | ResolvedExprKind::TypeValue(_) => {}
    }
}

pub(super) fn proof_prelude_is_closed(
    cfg: &CallableCfg,
    core_blocks: &BTreeSet<BasicBlockId>,
    operation_entry: &BasicBlockId,
    proof_nodes: &BTreeSet<NodeId>,
) -> bool {
    let prelude = cfg
        .blocks
        .keys()
        .filter(|block| !core_blocks.contains(*block))
        .cloned()
        .collect::<BTreeSet<_>>();
    if prelude.is_empty() {
        return operation_entry == &cfg.entry;
    }
    if core_blocks.contains(&cfg.entry) || !prelude.contains(&cfg.entry) {
        return false;
    }

    for block_id in &prelude {
        let Some(block) = cfg.blocks.get(block_id) else {
            return false;
        };
        if block
            .points
            .iter()
            .any(|point| !proof_nodes.contains(&point.source.node))
        {
            return false;
        }
        match &block.terminator {
            Terminator::Goto { edge } => {
                let Some(edge) = cfg.edges.get(edge) else {
                    return false;
                };
                if edge.from != *block_id
                    || edge.kind != EdgeKind::Fallthrough
                    || !proof_nodes.contains(&edge.source.node)
                    || (!prelude.contains(&edge.to) && &edge.to != operation_entry)
                {
                    return false;
                }
            }
            Terminator::Branch {
                condition,
                then_edge,
                else_edge,
            } => {
                let (Some(then_edge), Some(else_edge)) =
                    (cfg.edges.get(then_edge), cfg.edges.get(else_edge))
                else {
                    return false;
                };
                if !proof_nodes.contains(condition)
                    || then_edge.from != *block_id
                    || else_edge.from != *block_id
                    || then_edge.kind != EdgeKind::Then
                    || else_edge.kind != EdgeKind::Else
                    || then_edge.id == else_edge.id
                    || !proof_nodes.contains(&then_edge.source.node)
                    || !proof_nodes.contains(&else_edge.source.node)
                    || (!prelude.contains(&then_edge.to) && &then_edge.to != operation_entry)
                    || (!prelude.contains(&else_edge.to) && &else_edge.to != operation_entry)
                {
                    return false;
                }
            }
            _ => return false,
        }
    }

    for edge in cfg.edges.values() {
        if prelude.contains(&edge.from)
            && !prelude.contains(&edge.to)
            && &edge.to != operation_entry
        {
            return false;
        }
        if !prelude.contains(&edge.from) && prelude.contains(&edge.to) {
            return false;
        }
        if core_blocks.contains(&edge.to)
            && !core_blocks.contains(&edge.from)
            && &edge.to != operation_entry
        {
            return false;
        }
    }
    if !cfg
        .edges
        .values()
        .any(|edge| prelude.contains(&edge.from) && &edge.to == operation_entry)
    {
        return false;
    }

    // Every proof-only block must terminate at the operation entry; reject
    // cycles or detached branches even if the generic CFG validator accepts
    // them as reachable.
    let mut reverse_reachable = BTreeSet::from([operation_entry.clone()]);
    loop {
        let previous_len = reverse_reachable.len();
        for edge in cfg.edges.values() {
            if reverse_reachable.contains(&edge.to) && prelude.contains(&edge.from) {
                reverse_reachable.insert(edge.from.clone());
            }
        }
        if reverse_reachable.len() == previous_len {
            break;
        }
    }
    if !prelude
        .iter()
        .all(|block| reverse_reachable.contains(block))
    {
        return false;
    }

    let mut indegree = prelude
        .iter()
        .map(|block| {
            (
                block.clone(),
                cfg.edges
                    .values()
                    .filter(|edge| prelude.contains(&edge.from) && edge.to == *block)
                    .count(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut ready = indegree
        .iter()
        .filter_map(|(block, degree)| (*degree == 0).then_some(block.clone()))
        .collect::<BTreeSet<_>>();
    let mut visited = 0;
    while let Some(block) = ready.pop_first() {
        visited += 1;
        for edge in cfg
            .edges
            .values()
            .filter(|edge| edge.from == block && prelude.contains(&edge.to))
        {
            let Some(degree) = indegree.get_mut(&edge.to) else {
                return false;
            };
            *degree -= 1;
            if *degree == 0 {
                ready.insert(edge.to.clone());
            }
        }
    }
    visited == prelude.len()
}

pub(super) fn proof_nodes_have_exact_point_multiplicity(
    cfg: &CallableCfg,
    proof_nodes: &BTreeSet<NodeId>,
    contract_nodes: &BTreeSet<NodeId>,
) -> bool {
    let mut counts = BTreeMap::<NodeId, usize>::new();
    for point in cfg.blocks.values().flat_map(|block| &block.points) {
        if proof_nodes.contains(&point.source.node) {
            *counts.entry(point.source.node.clone()).or_default() += 1;
        }
    }
    contract_nodes
        .iter()
        .all(|node| counts.get(node) == Some(&1))
        && counts.values().all(|count| *count == 1)
}

/// First closed source shape: one immutable New result, an exhaustive match,
/// Ok(root) observed by map_size and explicitly dropped, and Err(status)
/// returned unchanged. No aliases, guards, hidden inputs, or helper calls can
/// be omitted from this exact statement/expression census.
pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
) -> Option<OrdinaryMapOwnedAnyV2NewResultReceipt> {
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    if cfg.owner != body.owner
        || signature.owner != body.owner
        || signature.result != i32_type
        || body.root.ty != i32_type
        || !signature.generic_parameters.is_empty()
        || !signature.parameters.is_empty()
        || !body.parameters.is_empty()
        || !body.captures.is_empty()
        || !body.place_inputs.is_empty()
        || !body.default_values.is_empty()
    {
        return None;
    }
    let non_contract_statements = body
        .root
        .statements
        .iter()
        .filter(|statement| {
            !matches!(
                &statement.kind,
                ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_)
            )
        })
        .collect::<Vec<_>>();
    let [new_statement] = non_contract_statements.as_slice() else {
        return None;
    };
    let (result_local, new_expr) = immutable_binding(new_statement, body)?;
    let new_call = builtin_call(new_expr, "map_new")?;
    if !new_call.arguments.is_empty() {
        return None;
    }
    let ResolvedType::Result { ok, error } = types.get(&new_expr.ty)? else {
        return None;
    };
    if error != &i32_type
        || !matches!(types.get(ok), Some(ResolvedType::Nominal {
            item, arguments, is_linear: false,
        }) if item.as_str() == "builtin:type:Record" && arguments.is_empty())
        || new_expr.ty != crate::core::ir::result_type_id(ok.clone(), error.clone())
    {
        return None;
    }
    let result_type = new_expr.ty.clone();
    let root_type = ok.clone();
    let match_expr = body.root.result.as_deref()?;
    let ResolvedExprKind::Match { scrutinee, arms } = &match_expr.kind else {
        return None;
    };
    if match_expr.ty != i32_type
        || !is_load(scrutinee, result_local, &result_type)
        || arms.len() != 2
        || arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.body.ty != i32_type)
    {
        return None;
    }
    let ok_arm = unique_variant_arm(arms, "Ok")?;
    let err_arm = unique_variant_arm(arms, "Err")?;
    let (root_local, root_pattern) = payload_binding(ok_arm, "Ok", &result_type, &root_type, body)?;
    let (status_local, status_pattern) =
        payload_binding(err_arm, "Err", &result_type, &i32_type, body)?;
    if BTreeSet::from([result_local, root_local, status_local]).len() != 3 {
        return None;
    }
    let ResolvedExprKind::Block(ok_body) = &ok_arm.body.kind else {
        return None;
    };
    let [size_statement, drop_statement] = ok_body.statements.as_slice() else {
        return None;
    };
    let (size_local, size_expr) = immutable_binding(size_statement, body)?;
    let size_call = builtin_call(size_expr, "map_size")?;
    let [receiver] = size_call.arguments.as_slice() else {
        return None;
    };
    let ok_result = ok_body.result.as_deref()?;
    if size_expr.ty != i32_type
        || !is_load(&receiver.value, root_local, &root_type)
        || receiver.conversion.kind != CheckedConversionKind::Identity
        || receiver.conversion.from != root_type
        || receiver.conversion.to != root_type
        || !is_load(ok_result, size_local, &i32_type)
        || BTreeSet::from([result_local, root_local, status_local, size_local]).len() != 4
    {
        return None;
    }
    let ResolvedStmtKind::Drop(places) = &drop_statement.kind else {
        return None;
    };
    let [place] = places.as_slice() else {
        return None;
    };
    if place.base != *root_local || !place.projections.is_empty() {
        return None;
    }
    let err_result = match &err_arm.body.kind {
        ResolvedExprKind::Block(block) if block.statements.is_empty() => block.result.as_deref()?,
        _ => &err_arm.body,
    };
    if !is_load(err_result, status_local, &i32_type) {
        return None;
    }

    // `result` in an ensures clause is represented by one synthetic local in
    // the resolved body. It is proof input, not another runtime owner. Keep
    // this exception exact: no other locals may be smuggled into the source
    // receipt, and the synthetic result keeps the function's declared i32
    // result type.
    let mut expected_locals = BTreeSet::from([
        result_local.clone(),
        root_local.clone(),
        status_local.clone(),
        size_local.clone(),
    ]);
    let contract_result = ResolvedLocalId(crate::core::NodeId(format!(
        "{}/contract-result/local",
        body.owner.0
    )));
    if let Some(local) = body.locals.get(&contract_result) {
        if local.ty != i32_type
            || local.mutable
            || !body.root.statements.iter().any(|statement| {
                matches!(
                    &statement.kind,
                    ResolvedStmtKind::Contract {
                        kind: crate::core::ir::ContractKind::Ensures,
                        ..
                    }
                )
            })
        {
            return None;
        }
        expected_locals.insert(contract_result);
    }
    if body.locals.keys().cloned().collect::<BTreeSet<_>>() != expected_locals {
        return None;
    }

    // Bind variant selection to the actual Checker CFG, including source
    // arm identities. A graph with the right size but swapped arms is not a
    // proof. Contract expressions add a proof-only prefix to the CFG; require
    // that prefix to be a closed, acyclic subgraph of checked proof nodes that
    // enters the NewResult dispatch and cannot bypass it.
    if cfg.blocks.len() < 4
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().collect::<BTreeSet<_>>() != cfg.reachable.iter().collect()
        || crate::core::cfg::validate_cfg(cfg).is_err()
    {
        return None;
    }
    let mut dispatch_blocks = cfg
        .blocks
        .values()
        .filter(|block| matches!(&block.terminator, Terminator::Match { .. }));
    let entry = dispatch_blocks.next()?;
    if dispatch_blocks.next().is_some() {
        return None;
    }
    let dispatch_block = &entry.id;
    let Terminator::Match {
        scrutinee: cfg_scrutinee,
        arms: dispatch_edges,
    } = &entry.terminator
    else {
        return None;
    };
    if cfg_scrutinee != &scrutinee.node_id || dispatch_edges.len() != 2 {
        return None;
    }
    let arm_edge = |arm: &MatchArm| -> Option<(&EdgeId, &BasicBlockId)> {
        let mut found = dispatch_edges.iter().filter_map(|id| {
            let edge = cfg.edges.get(id)?;
            (edge.from == *dispatch_block
                && edge.kind == EdgeKind::MatchArm
                && edge.source.node == arm.node_id)
                .then_some((id, &edge.to))
        });
        let edge = found.next()?;
        found.next().is_none().then_some(edge)
    };
    let (ok_edge, ok_block) = arm_edge(ok_arm)?;
    let (err_edge, err_block) = arm_edge(err_arm)?;
    let join_edge = |block: &BasicBlockId, arm: &MatchArm| -> Option<(&EdgeId, &BasicBlockId)> {
        let Terminator::Goto { edge } = &cfg.blocks.get(block)?.terminator else {
            return None;
        };
        let source = cfg.edges.get(edge)?;
        (source.from == *block
            && source.kind == EdgeKind::Fallthrough
            && source.source.node == arm.node_id)
            .then_some((edge, &source.to))
    };
    let (ok_join_edge, join) = join_edge(ok_block, ok_arm)?;
    let (err_join_edge, err_join) = join_edge(err_block, err_arm)?;
    if join != err_join
        || BTreeSet::from([dispatch_block, ok_block, err_block, join]).len() != 4
        || !matches!(&cfg.blocks.get(join)?.terminator,
            Terminator::Return { value: Some(value), implicit: true } if value == &match_expr.node_id)
    {
        return None;
    }
    let core_blocks = BTreeSet::from([
        dispatch_block.clone(),
        ok_block.clone(),
        err_block.clone(),
        join.clone(),
    ]);
    let expected_core_edges = BTreeSet::from([
        ok_edge.clone(),
        err_edge.clone(),
        ok_join_edge.clone(),
        err_join_edge.clone(),
    ]);
    if cfg
        .edges
        .values()
        .filter(|edge| core_blocks.contains(&edge.from))
        .map(|edge| edge.id.clone())
        .collect::<BTreeSet<_>>()
        != expected_core_edges
    {
        return None;
    }
    let proof_nodes = direct_proof_expression_nodes(body);
    let contract_nodes = direct_contract_expression_nodes(body);
    if !proof_prelude_is_closed(cfg, &core_blocks, dispatch_block, &proof_nodes)
        || !proof_nodes_have_exact_point_multiplicity(cfg, &proof_nodes, &contract_nodes)
    {
        return None;
    }
    let Some(ok_block_cfg) = cfg.blocks.get(ok_block) else {
        return None;
    };
    let Some(err_block_cfg) = cfg.blocks.get(err_block) else {
        return None;
    };
    let Some(join_cfg) = cfg.blocks.get(join) else {
        return None;
    };
    let entry_points = [
        ExpectedPoint::empty(
            &new_expr.node_id,
            &new_expr.origin,
            CfgPointKind::Expression,
        ),
        ExpectedPoint::local_binding(
            body,
            &new_statement.node_id,
            &new_statement.origin,
            result_local,
        ),
        ExpectedPoint::local_load(body, scrutinee, result_local, CfgPointKind::Condition),
    ];
    let ok_points = [
        ExpectedPoint::local_binding(
            body,
            &ok_arm.pattern.node_id,
            &ok_arm.pattern.origin,
            root_local,
        ),
        ExpectedPoint::local_load(body, &receiver.value, root_local, CfgPointKind::Expression),
        ExpectedPoint::empty(
            &size_expr.node_id,
            &size_expr.origin,
            CfgPointKind::Expression,
        ),
        ExpectedPoint::local_binding(
            body,
            &size_statement.node_id,
            &size_statement.origin,
            size_local,
        ),
        ExpectedPoint::drop_place(body, drop_statement, place),
        ExpectedPoint::local_load(body, ok_result, size_local, CfgPointKind::Expression),
        // Resolved CFG lowering records both the block result and the value
        // of the enclosing arm-body Block expression.
        ExpectedPoint::empty(
            &ok_arm.body.node_id,
            &ok_arm.body.origin,
            CfgPointKind::Expression,
        ),
    ];
    let mut err_points = vec![
        ExpectedPoint::local_binding(
            body,
            &err_arm.pattern.node_id,
            &err_arm.pattern.origin,
            status_local,
        ),
        ExpectedPoint::local_load(body, err_result, status_local, CfgPointKind::Expression),
    ];
    if matches!(&err_arm.body.kind, ResolvedExprKind::Block(_)) {
        // An empty explicit Block around the direct Err payload has its own
        // enclosing expression point after the payload load.
        err_points.push(ExpectedPoint::empty(
            &err_arm.body.node_id,
            &err_arm.body.origin,
            CfgPointKind::Expression,
        ));
    }
    let join_points = [ExpectedPoint::empty(
        &match_expr.node_id,
        &match_expr.origin,
        CfgPointKind::Expression,
    )];
    if !exact_point_census(entry, &entry_points, &proof_nodes)
        || !exact_point_census(ok_block_cfg, &ok_points, &proof_nodes)
        || !exact_point_census(err_block_cfg, &err_points, &proof_nodes)
        || !exact_point_census(join_cfg, &join_points, &proof_nodes)
    {
        return None;
    }

    Some(OrdinaryMapOwnedAnyV2NewResultReceipt {
        owner: body.owner.clone(),
        operation: new_expr.node_id.clone(),
        result_binding_point: new_statement.node_id.clone(),
        result_local: result_local.clone(),
        source_result_type: result_type,
        root_type,
        status_type: i32_type,
        match_node: match_expr.node_id.clone(),
        scrutinee: scrutinee.node_id.clone(),
        ok_arm: ok_arm.node_id.clone(),
        ok_pattern: ok_arm.pattern.node_id.clone(),
        ok_binding_pattern: root_pattern.node_id.clone(),
        root_local: root_local.clone(),
        root: OrdinaryMapOwnedAnyV2Checker::resource_identity(root_local),
        size_operation: size_expr.node_id.clone(),
        size_receiver: receiver.value.node_id.clone(),
        size_binding_point: size_statement.node_id.clone(),
        size_local: size_local.clone(),
        ok_result: ok_result.node_id.clone(),
        root_drop: drop_statement.node_id.clone(),
        err_arm: err_arm.node_id.clone(),
        err_pattern: err_arm.pattern.node_id.clone(),
        err_binding_pattern: status_pattern.node_id.clone(),
        status_local: status_local.clone(),
        err_result: err_result.node_id.clone(),
        entry: cfg.entry.clone(),
        ok_block: ok_block.clone(),
        err_block: err_block.clone(),
        join: join.clone(),
        ok_edge: ok_edge.clone(),
        err_edge: err_edge.clone(),
        ok_join_edge: ok_join_edge.clone(),
        err_join_edge: err_join_edge.clone(),
        cfg: ManagedAnyCfgReceipt::from_cfg(cfg, body),
    })
}

fn immutable_binding<'a>(
    statement: &'a ResolvedStmt,
    body: &ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedExpr)> {
    let ResolvedStmtKind::Bind {
        pattern,
        initializer: Some(expression),
    } = &statement.kind
    else {
        return None;
    };
    let ResolvedPatternKind::Binding {
        local,
        by_reference: None,
    } = &pattern.kind
    else {
        return None;
    };
    let binding = body.locals.get(local)?;
    (!binding.mutable && binding.ty == pattern.ty && binding.ty == expression.ty)
        .then_some((local, expression))
}

fn builtin_call<'a>(expression: &'a ResolvedExpr, name: &str) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return None;
    };
    (matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == name)
        && call.result == expression.ty
        && call.type_arguments.is_empty()
        && call.permission.is_none()
        && call.session.is_empty())
    .then_some(call)
}

fn is_load(expression: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    expression.ty == *ty
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
            if place.base == *local && place.projections.is_empty())
}

fn unique_variant_arm<'a>(arms: &'a [MatchArm], name: &str) -> Option<&'a MatchArm> {
    let expected = format!("builtin:variant:Result::{name}");
    let mut matches = arms.iter().filter(|arm| {
        matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. }
            if variant.0 == expected)
    });
    let arm = matches.next()?;
    matches.next().is_none().then_some(arm)
}

fn payload_binding<'a>(
    arm: &'a MatchArm,
    name: &str,
    result_type: &ResolvedTypeId,
    payload_type: &ResolvedTypeId,
    body: &ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedPattern)> {
    let ResolvedPatternKind::Constructor { variant, fields } = &arm.pattern.kind else {
        return None;
    };
    let [(field, pattern)] = fields.as_slice() else {
        return None;
    };
    let ResolvedPatternKind::Binding {
        local,
        by_reference: None,
    } = &pattern.kind
    else {
        return None;
    };
    let binding = body.locals.get(local)?;
    (variant.0 == format!("builtin:variant:Result::{name}")
        && field.0 == format!("builtin:variant:Result::{name}/payload:0")
        && arm.pattern.ty == *result_type
        && pattern.ty == *payload_type
        && binding.ty == *payload_type
        && !binding.mutable)
        .then_some((local, pattern))
}

/// Source replay rejects modified receipts as well as modified CFG/type
/// inputs. This is a Checker helper; executable consumers must validate an
/// independently mirrored MIR operation/variant proof rather than invoke it
/// to re-derive source semantics.
pub(super) fn validate(
    receipt: &OrdinaryMapOwnedAnyV2NewResultReceipt,
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
) -> Result<(), String> {
    if collect(cfg, body, signature, types).as_ref() == Some(receipt) {
        Ok(())
    } else {
        Err(
            "ordinary Map New Result receipt disagrees with exact Checker source/CFG evidence"
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
        func observe() -> i32 {
            let outcome = map_new()
            match outcome {
                Ok(root) => {
                    let size = map_size(root)
                    drop(root)
                    size
                },
                Err(status) => status
            }
        }
        func main() -> i32 { 0 }
    "#;

    fn checked(source: &str) -> Result<crate::core::CheckedProgram, Vec<Diagnostic>> {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file)
    }

    #[test]
    fn new_result_source_receipt_binds_canonical_result_and_both_payloads() {
        let checked_program = checked(SOURCE).expect("New Result source checks");
        let owner = NodeId("function:observe".into());
        let callable = checked_program
            .callable(&owner)
            .expect("ordinary source callable");
        let receipt = callable
            .resources
            .ordinary_map_owned_any_v2_new_result_receipt
            .as_ref()
            .expect("exact New Result source receipt");
        validate(
            receipt,
            &callable.cfg,
            &callable.body,
            &callable.signature,
            checked_program.resolved_types(),
        )
        .expect("source replay");
        assert_eq!(
            receipt.source_result_type,
            crate::core::ir::result_type_id(receipt.root_type.clone(), receipt.status_type.clone())
        );
        assert_ne!(receipt.root_local, receipt.result_local);
        assert_ne!(receipt.root_local, receipt.status_local);
        assert_ne!(receipt.ok_edge, receipt.err_edge);
        assert_ne!(receipt.ok_block, receipt.err_block);
        assert!(callable
            .resources
            .ordinary_map_owned_any_v2_cfg_receipt
            .is_none());
        assert!(
            callable
                .resources
                .ordinary_map_owned_any_v2_actions
                .is_empty(),
            "Result receipt must not manufacture an unconditional bare-root New action"
        );

        let braced_err =
            checked(&SOURCE.replace("Err(status) => status", "Err(status) => { status }"))
                .expect("empty Err block remains in the admitted source shape");
        assert!(braced_err
            .resource_analysis(&owner)
            .unwrap()
            .ordinary_map_owned_any_v2_new_result_receipt
            .is_some());
    }

    #[test]
    fn new_result_source_receipt_rejects_owner_or_variant_gaps() {
        for (name, source) in [
            ("missing root drop", SOURCE.replace("drop(root)", "")),
            (
                "duplicate root drop",
                SOURCE.replace("drop(root)", "drop(root)\n drop(root)"),
            ),
            (
                "wrong Err result",
                SOURCE.replace("Err(status) => status", "Err(status) => 3"),
            ),
            (
                "wildcard Err",
                SOURCE.replace("Err(status) => status", "_ => 3"),
            ),
            ("wildcard Ok payload", SOURCE.replace("Ok(root)", "Ok(_)")),
            (
                "guard",
                SOURCE.replace("Err(status) =>", "Err(status) if status == 3 =>"),
            ),
            (
                "extra New",
                SOURCE.replace("let outcome", "let extra = map_new()\n let outcome"),
            ),
            (
                "mutable result",
                SOURCE.replace("let outcome", "let mut outcome"),
            ),
            (
                "result alias",
                SOURCE.replace("match outcome", "let alias = outcome\n match alias"),
            ),
            (
                "extra root use",
                SOURCE.replace("drop(root)", "map_size(root)\n drop(root)"),
            ),
        ] {
            if let Ok(checked) = checked(&source) {
                let analysis = checked
                    .resource_analysis(&NodeId("function:observe".into()))
                    .expect("ordinary callable resource analysis");
                assert!(
                    analysis
                        .ordinary_map_owned_any_v2_new_result_receipt
                        .is_none(),
                    "{name} must not receive the New Result source authority"
                );
            }
        }
    }

    #[test]
    fn new_result_source_receipt_rejects_forged_payload_and_swapped_cfg_edges() {
        let checked = checked(SOURCE).expect("New Result source checks");
        let callable = checked
            .callable(&NodeId("function:observe".into()))
            .unwrap();
        let original = callable
            .resources
            .ordinary_map_owned_any_v2_new_result_receipt
            .as_ref()
            .expect("exact New Result source receipt");
        let types = checked.resolved_types();
        for forged in [
            {
                let mut r = original.clone();
                r.root_local = r.status_local.clone();
                r
            },
            {
                let mut r = original.clone();
                r.operation = r.size_operation.clone();
                r
            },
            {
                let mut r = original.clone();
                r.source_result_type = r.root_type.clone();
                r
            },
            {
                let mut r = original.clone();
                r.root_drop = r.err_result.clone();
                r
            },
            {
                let mut r = original.clone();
                std::mem::swap(&mut r.ok_edge, &mut r.err_edge);
                r
            },
        ] {
            assert!(validate(
                &forged,
                &callable.cfg,
                &callable.body,
                &callable.signature,
                types
            )
            .is_err());
        }
        let mut cfg = callable.cfg.clone();
        let ok_to = cfg.edges[&original.ok_edge].to.clone();
        let err_to = cfg.edges[&original.err_edge].to.clone();
        cfg.edges.get_mut(&original.ok_edge).unwrap().to = err_to;
        cfg.edges.get_mut(&original.err_edge).unwrap().to = ok_to;
        assert!(collect(&cfg, &callable.body, &callable.signature, types).is_none());
        assert!(validate(original, &cfg, &callable.body, &callable.signature, types).is_err());
    }

    #[test]
    fn new_result_source_receipt_rejects_extra_missing_and_forged_cfg_points() {
        let checked = checked(SOURCE).expect("New Result source checks");
        let callable = checked
            .callable(&NodeId("function:observe".into()))
            .unwrap();
        let original = callable
            .resources
            .ordinary_map_owned_any_v2_new_result_receipt
            .as_ref()
            .expect("exact New Result source receipt");
        let types = checked.resolved_types();

        let mut extra_point = callable.cfg.clone();
        let mut injected = extra_point.blocks[&original.entry].points[0].clone();
        injected.source.node = NodeId("synthetic:unaccounted-point".into());
        extra_point
            .blocks
            .get_mut(&original.entry)
            .unwrap()
            .points
            .push(injected);
        assert!(crate::core::cfg::validate_cfg(&extra_point).is_ok());
        assert!(collect(&extra_point, &callable.body, &callable.signature, types).is_none());

        let mut missing_point = callable.cfg.clone();
        missing_point
            .blocks
            .get_mut(&original.ok_block)
            .unwrap()
            .points
            .retain(|point| point.source.node != original.root_drop);
        assert!(crate::core::cfg::validate_cfg(&missing_point).is_ok());
        assert!(collect(&missing_point, &callable.body, &callable.signature, types).is_none());

        let mut forged_drop_reads = callable.cfg.clone();
        let drop_point = forged_drop_reads
            .blocks
            .get_mut(&original.ok_block)
            .unwrap()
            .points
            .iter_mut()
            .find(|point| point.source.node == original.root_drop)
            .unwrap();
        drop_point.reads.clear();
        assert!(crate::core::cfg::validate_cfg(&forged_drop_reads).is_ok());
        assert!(collect(
            &forged_drop_reads,
            &callable.body,
            &callable.signature,
            types
        )
        .is_none());

        let mut forged_drop_place = callable.cfg.clone();
        let drop_point = forged_drop_place
            .blocks
            .get_mut(&original.ok_block)
            .unwrap()
            .points
            .iter_mut()
            .find(|point| point.source.node == original.root_drop)
            .unwrap();
        drop_point.read_places[0].base = LocalId(original.status_local.0.clone());
        assert!(crate::core::cfg::validate_cfg(&forged_drop_place).is_ok());
        assert!(collect(
            &forged_drop_place,
            &callable.body,
            &callable.signature,
            types
        )
        .is_none());
    }
}
