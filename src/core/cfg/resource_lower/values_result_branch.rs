//! Exact Checker authority for the Result-wrapped Values producer fixtures.
//!
//! This profile is deliberately separate from the straight-line Values return
//! receipt. It binds the complete reachable CFG, each `map_new`/`map_set`
//! Result arm, every terminal return, and all explicit root and payload Drops.

use super::*;
use crate::core::cfg::{EdgeKind, Terminator};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2ValuesResultBranchDrop as BranchDrop,
    OrdinaryMapOwnedAnyV2ValuesResultBranchPath as BranchPath,
    OrdinaryMapOwnedAnyV2ValuesResultPathKind as PathKind,
    OrdinaryMapOwnedAnyV2ValuesResultProducerProfileVersion as ProfileVersion,
    OrdinaryMapOwnedAnyV2ValuesResultProducerReceipt as ProducerReceipt,
    OrdinaryMapOwnedAnyV2ValuesResultSetBranch as SetBranch,
};
use crate::core::PrimitiveType;

pub(super) struct Collected {
    pub(super) receipt: ProducerReceipt,
    pub(super) actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    pub(super) attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    pub(super) root_loads: Vec<(NodeId, ResolvedLocalId)>,
}

struct SetView<'a> {
    match_expression: &'a ResolvedExpr,
    operation: NodeId,
    call: &'a ResolvedCall,
    input_root: ResolvedLocalId,
    key_node: NodeId,
    key_literal: String,
    payload_node: NodeId,
    payload_local: ResolvedLocalId,
    ok_arm: &'a MatchArm,
    ok_pattern: &'a ResolvedPattern,
    output_root: ResolvedLocalId,
    err_arm: &'a MatchArm,
    err_pattern: &'a ResolvedPattern,
    error_local: ResolvedLocalId,
}

fn primitive(ty: &ResolvedTypeId, value: PrimitiveType) -> bool {
    *ty == crate::core::ir::primitive_type_id(value)
}

fn immutable_binding<'a>(
    statement: &'a ResolvedStmt,
    body: &ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedExpr)> {
    let ResolvedStmtKind::Bind {
        pattern,
        initializer: Some(initializer),
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
    (!binding.mutable && binding.ty == pattern.ty && binding.ty == initializer.ty)
        .then_some((local, initializer))
}

fn result_parts<'a>(
    types: &'a ResolvedTypeTable,
    ty: &'a ResolvedTypeId,
) -> Option<(&'a ResolvedTypeId, &'a ResolvedTypeId)> {
    match types.get(ty)? {
        ResolvedType::Result { ok, error } => Some((ok, error)),
        ResolvedType::Nominal {
            item, arguments, ..
        } if item.as_str() == "builtin:type:Result" && arguments.len() == 2 => {
            Some((&arguments[0], &arguments[1]))
        }
        _ => None,
    }
}

fn is_record(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(types.get(ty), Some(ResolvedType::Nominal { item, arguments, .. })
        if item.as_str() == "builtin:type:Record" && arguments.is_empty())
}

fn variant_arm<'a>(arms: &'a [MatchArm], variant_name: &str) -> Option<&'a MatchArm> {
    let expected = format!("builtin:variant:Result::{variant_name}");
    let mut found = arms.iter().filter(|arm| {
        arm.guard.is_none()
            && matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. }
                if variant.0 == expected)
    });
    let arm = found.next()?;
    found.next().is_none().then_some(arm)
}

fn payload_pattern<'a>(
    arm: &'a MatchArm,
    variant_name: &str,
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
    (variant.0 == format!("builtin:variant:Result::{variant_name}")
        && field.0 == format!("builtin:variant:Result::{variant_name}/payload:0")
        && arm.pattern.ty == *result_type
        && pattern.ty == *payload_type
        && binding.ty == *payload_type
        && !binding.mutable)
        .then_some((local, pattern))
}

fn direct_drop(statement: &ResolvedStmt, local: &ResolvedLocalId) -> Option<BranchDrop> {
    let ResolvedStmtKind::Drop(places) = &statement.kind else {
        return None;
    };
    let [place] = places.as_slice() else {
        return None;
    };
    (place.base == *local && place.projections.is_empty()).then(|| BranchDrop {
        local: local.clone(),
        point: statement.node_id.clone(),
        place_index: 0,
    })
}

fn empty_list(expression: &ResolvedExpr, types: &ResolvedTypeTable) -> bool {
    matches!(&expression.kind, ResolvedExprKind::List(items) if items.is_empty())
        && matches!(
            types.get(&expression.ty),
            Some(ResolvedType::Nominal { item, arguments, .. })
                if item.as_str() == "builtin:type:List" && arguments.is_empty()
        )
}

fn direct_load(expression: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    expression.ty == *ty
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
            if place.base == *local && place.projections.is_empty())
}

fn direct_string_payload_binding(
    body: &ResolvedBody,
    local: &ResolvedLocalId,
    string_type: &ResolvedTypeId,
) -> Option<(NodeId, NodeId, String)> {
    let matches = body
        .root
        .statements
        .iter()
        .filter_map(|statement| {
            let (binding_local, initializer) = immutable_binding(statement, body)?;
            if binding_local != local || initializer.ty != *string_type {
                return None;
            }
            let ResolvedExprKind::Literal(ResolvedLiteral::String(literal)) = &initializer.kind
            else {
                return None;
            };
            Some((
                statement.node_id.clone(),
                initializer.node_id.clone(),
                literal.clone(),
            ))
        })
        .collect::<Vec<_>>();
    let [binding] = matches.as_slice() else {
        return None;
    };
    Some(binding.clone())
}

fn set_view<'a>(
    expression: &'a ResolvedExpr,
    input_root: &ResolvedLocalId,
    body: &ResolvedBody,
    types: &ResolvedTypeTable,
    i32_type: &ResolvedTypeId,
) -> Option<SetView<'a>> {
    let ResolvedExprKind::Match { scrutinee, arms } = &expression.kind else {
        return None;
    };
    let ResolvedExprKind::Call(call) = &scrutinee.kind else {
        return None;
    };
    let [source, key, payload] = call.arguments.as_slice() else {
        return None;
    };
    let ok_arm = variant_arm(arms, "Ok")?;
    let err_arm = variant_arm(arms, "Err")?;
    let (input_root_type, error_type) = result_parts(types, &scrutinee.ty)?;
    let (output_root_type, output_error_type) = result_parts(types, &scrutinee.ty)?;
    if !matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "map_set")
        || !call.type_arguments.is_empty()
        || call.permission.is_some()
        || !call.session.is_empty()
        || call.result != scrutinee.ty
        || input_root_type != &body.locals.get(input_root)?.ty
        || !is_record(types, input_root_type)
        || error_type != i32_type
        || output_root_type != input_root_type
        || output_error_type != i32_type
        || !direct_load(&source.value, input_root, input_root_type)
        || !ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(scrutinee, call)
        || source.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
        || source.conversion.from != source.value.ty
        || source.conversion.to != source.value.ty
        || !matches!(
            &key.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::String(_))
        )
        || !primitive(&key.value.ty, PrimitiveType::String)
        || key.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
        || key.conversion.from != key.value.ty
        || key.conversion.to != key.value.ty
        || !matches!(&payload.value.kind, ResolvedExprKind::Load(place) if place.projections.is_empty())
        || !primitive(&payload.value.ty, PrimitiveType::String)
        || payload.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
        || payload.conversion.from != payload.value.ty
        || payload.conversion.to != payload.value.ty
        || arms.len() != 2
    {
        return None;
    }
    let (output_root, ok_pattern) =
        payload_pattern(ok_arm, "Ok", &scrutinee.ty, output_root_type, body)?;
    let (error_local, err_pattern) =
        payload_pattern(err_arm, "Err", &scrutinee.ty, i32_type, body)?;
    let key_literal = match &key.value.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::String(value)) => value.clone(),
        _ => return None,
    };
    let payload_local = match &payload.value.kind {
        ResolvedExprKind::Load(place) if place.projections.is_empty() => place.base.clone(),
        _ => return None,
    };
    Some(SetView {
        match_expression: expression,
        operation: scrutinee.node_id.clone(),
        call,
        input_root: input_root.clone(),
        key_node: key.value.node_id.clone(),
        key_literal,
        payload_node: payload.value.node_id.clone(),
        payload_local,
        ok_arm,
        ok_pattern,
        output_root: output_root.clone(),
        err_arm,
        err_pattern,
        error_local: error_local.clone(),
    })
}

fn get_block(arm: &MatchArm) -> Option<&ResolvedBlock> {
    let ResolvedExprKind::Block(block) = &arm.body.kind else {
        return None;
    };
    Some(block)
}

fn drop_sequence(block: &ResolvedBlock, expected: &[ResolvedLocalId]) -> Option<Vec<BranchDrop>> {
    if block.statements.len() != expected.len() {
        return None;
    }
    block
        .statements
        .iter()
        .zip(expected)
        .map(|(statement, local)| direct_drop(statement, local))
        .collect()
}

fn attempt_from_call(
    kind: OrdinaryMapOwnedAnyV2AttemptKind,
    point: &NodeId,
    arguments: &[NodeId],
    receiver: Option<&ResolvedLocalId>,
    result: Option<&ResolvedLocalId>,
) -> OrdinaryMapOwnedAnyV2Attempt {
    OrdinaryMapOwnedAnyV2Attempt {
        kind,
        point: point.clone(),
        arguments: arguments.to_vec(),
        receiver_local: receiver.cloned(),
        result_local: result.cloned(),
    }
}

fn add_root_drop(
    actions: &mut Vec<OrdinaryMapOwnedAnyV2Action>,
    attempts: &mut BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
    drop: &BranchDrop,
) {
    let action = super::OrdinaryMapOwnedAnyV2Checker::build_action(
        OrdinaryMapOwnedAnyV2ActionKind::Drop,
        drop.point.clone(),
        drop.local.clone(),
        None,
        None,
        None,
        None,
        false,
        None,
    );
    actions.push(action);
    attempts.insert(attempt_from_call(
        OrdinaryMapOwnedAnyV2AttemptKind::Drop,
        &drop.point,
        std::slice::from_ref(&drop.local.0),
        Some(&drop.local),
        None,
    ));
}

fn exact_cfg_receipt(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    matches: &BTreeMap<NodeId, BTreeSet<NodeId>>,
    source_nodes: &BTreeSet<NodeId>,
    return_node: &NodeId,
) -> Option<ManagedAnyCfgReceipt> {
    if crate::core::cfg::validate_cfg(cfg).is_err()
        || cfg.reachable.len() != cfg.blocks.len()
        || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
    {
        return None;
    }
    let mut observed = BTreeMap::new();
    let mut observed_points = BTreeSet::new();
    let mut returns = 0usize;
    for (block_id, block) in &cfg.blocks {
        if let Terminator::Match { scrutinee, arms } = &block.terminator {
            if !matches.contains_key(scrutinee)
                || observed
                    .insert(scrutinee.clone(), block_id.clone())
                    .is_some()
            {
                return None;
            }
            let expected = matches.get(scrutinee)?;
            let mut source_arms = BTreeSet::new();
            for edge_id in arms {
                let edge = cfg.edges.get(edge_id)?;
                let target = cfg.blocks.get(&edge.to)?;
                if edge.from != *block_id
                    || edge.kind != EdgeKind::MatchArm
                    || target.source.node != edge.source.node
                    || !expected.contains(&edge.source.node)
                    || !source_arms.insert(edge.source.node.clone())
                {
                    return None;
                }
            }
            if arms.len() != 2 || &source_arms != expected {
                return None;
            }
        } else {
            match &block.terminator {
                Terminator::Goto { edge } => {
                    let edge = cfg.edges.get(edge)?;
                    if edge.from != *block_id || edge.kind != EdgeKind::Fallthrough {
                        return None;
                    }
                }
                Terminator::Return {
                    value: Some(value),
                    implicit: true,
                } if value == return_node => returns += 1,
                _ => return None,
            }
        }
        for point in &block.points {
            if !source_nodes.contains(&point.source.node)
                || !observed_points.insert(point.source.node.clone())
            {
                return None;
            }
        }
    }
    if returns != 1
        || observed.keys().cloned().collect::<BTreeSet<_>>()
            != matches.keys().cloned().collect::<BTreeSet<_>>()
    {
        return None;
    }
    Some(ManagedAnyCfgReceipt::from_cfg(cfg, body))
}

struct RecursiveBuild<'a> {
    body: &'a ResolvedBody,
    types: &'a ResolvedTypeTable,
    i32_type: &'a ResolvedTypeId,
    payload_locals: &'a [ResolvedLocalId],
    all_sets: Vec<SetBranch>,
    paths: Vec<BranchPath>,
    match_arms: BTreeMap<NodeId, BTreeSet<NodeId>>,
    set_count: usize,
    values_operation: Option<NodeId>,
    snapshot_binding_point: Option<NodeId>,
    snapshot_local: Option<ResolvedLocalId>,
    snapshot_type: Option<ResolvedTypeId>,
    surface_return_type: Option<ResolvedTypeId>,
    success_result: Option<NodeId>,
    actions: Vec<OrdinaryMapOwnedAnyV2Action>,
    attempts: BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
}

impl RecursiveBuild<'_> {
    fn process_set(
        &mut self,
        expression: &ResolvedExpr,
        input_root: &ResolvedLocalId,
        prior_roots: &[ResolvedLocalId],
        arm_path: &[NodeId],
    ) -> Option<()> {
        if self.all_sets.len() >= 2 {
            return None;
        }
        let view = set_view(expression, input_root, self.body, self.types, self.i32_type)?;
        let (output_root_type, _) = result_parts(self.types, &view.match_expression.ty)?;
        if self.body.locals.get(&view.output_root)?.ty != *output_root_type
            || view.output_root == *input_root
            || view.error_local == view.output_root
        {
            return None;
        }
        let key_value = match &view.call.arguments[1].value.kind {
            ResolvedExprKind::Literal(ResolvedLiteral::String(value)) => value,
            _ => return None,
        };
        let source = &view.call.arguments[0].value;
        let key = &view.call.arguments[1].value;
        let payload = &view.call.arguments[2].value;
        let payload_local = match &view.call.arguments[2].value.kind {
            ResolvedExprKind::Load(place) => place.base.clone(),
            _ => return None,
        };
        let (payload_binding_point, payload_initializer_node, payload_literal) =
            direct_string_payload_binding(
                self.body,
                &payload_local,
                &crate::core::ir::primitive_type_id(PrimitiveType::String),
            )?;
        if !self.payload_locals.contains(&payload_local)
            || view.payload_local != payload_local
            || view.payload_node != payload.node_id
        {
            return None;
        }
        self.set_count += 1;
        let arm_ids = BTreeSet::from([view.ok_arm.node_id.clone(), view.err_arm.node_id.clone()]);
        self.match_arms.insert(view.operation.clone(), arm_ids);
        let mut set_action = super::OrdinaryMapOwnedAnyV2Checker::build_action(
            OrdinaryMapOwnedAnyV2ActionKind::Set,
            view.operation.clone(),
            view.output_root.clone(),
            Some(key.node_id.clone()),
            Some(key.ty.clone()),
            Some(payload.node_id.clone()),
            Some(payload.ty.clone()),
            true,
            Some(view.input_root.clone()),
        );
        set_action.source_node = Some(source.node_id.clone());
        set_action.source_type = Some(source.ty.clone());
        set_action.key_provenance =
            Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::StringLiteral);
        set_action.key_static_digest =
            Some(blake3::hash(key_value.as_bytes()).to_hex().to_string());
        self.actions.push(set_action);
        self.attempts.insert(attempt_from_call(
            OrdinaryMapOwnedAnyV2AttemptKind::Set,
            &view.operation,
            &view
                .call
                .arguments
                .iter()
                .map(|argument| argument.value.node_id.clone())
                .collect::<Vec<_>>(),
            Some(&view.input_root),
            Some(&view.output_root),
        ));

        self.all_sets.push(SetBranch {
            operation: view.operation.clone(),
            match_node: view.match_expression.node_id.clone(),
            input_root: view.input_root.clone(),
            key_node: view.key_node.clone(),
            key_literal: view.key_literal.clone(),
            payload_node: view.payload_node.clone(),
            payload_local: payload_local.clone(),
            payload_binding_point,
            payload_initializer_node,
            payload_literal,
            ok_arm: view.ok_arm.node_id.clone(),
            ok_pattern: view.ok_pattern.node_id.clone(),
            output_root: view.output_root.clone(),
            err_arm: view.err_arm.node_id.clone(),
            err_pattern: view.err_pattern.node_id.clone(),
            error_local: view.error_local.clone(),
        });

        let err_block = get_block(view.err_arm)?;
        let err_drop_locals = prior_roots
            .iter()
            .cloned()
            .chain(self.payload_locals.iter().cloned())
            .collect::<Vec<_>>();
        let err_drops = drop_sequence(err_block, &err_drop_locals)?;
        let err_result = err_block.result.as_deref()?;
        if !empty_list(err_result, self.types) {
            return None;
        }
        let mut err_path = arm_path.to_vec();
        err_path.push(view.err_arm.node_id.clone());
        self.paths.push(BranchPath {
            arm_path: err_path,
            result: err_result.node_id.clone(),
            kind: PathKind::EmptyList,
            drops: err_drops.clone(),
        });
        let root_locals = prior_roots.iter().cloned().collect::<BTreeSet<_>>();
        for drop in &err_drops {
            if root_locals.contains(&drop.local) {
                add_root_drop(&mut self.actions, &mut self.attempts, drop);
            }
        }

        let ok_block = get_block(view.ok_arm)?;
        let mut ok_path = arm_path.to_vec();
        ok_path.push(view.ok_arm.node_id.clone());
        let mut descendants = prior_roots.to_vec();
        descendants.push(view.output_root.clone());
        if ok_block.statements.is_empty() {
            let child = ok_block.result.as_deref()?;
            if !matches!(&child.kind, ResolvedExprKind::Match { .. }) {
                return None;
            }
            let mut next_root_chain = prior_roots.to_vec();
            next_root_chain.push(view.output_root.clone());
            self.process_set(child, &view.output_root, &next_root_chain, &ok_path)?;
        } else {
            if self.all_sets.len() != self.payload_locals.len()
                || !ok_block.statements.first().is_some_and(|statement| {
                    matches!(statement.kind, ResolvedStmtKind::Bind { .. })
                })
            {
                return None;
            }
            let (snapshot_local, values_expression) =
                immutable_binding(&ok_block.statements[0], self.body)?;
            let ResolvedExprKind::Call(values_call) = &values_expression.kind else {
                return None;
            };
            if !matches!(&values_call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "values")
                || values_call.arguments.len() != 1
                || !direct_load(
                    &values_call.arguments[0].value,
                    &view.output_root,
                    output_root_type,
                )
                || values_call.arguments[0].conversion.kind
                    != crate::core::ir::CheckedConversionKind::Identity
                || values_call.arguments[0].conversion.from != values_call.arguments[0].value.ty
                || values_call.arguments[0].conversion.to != values_call.arguments[0].value.ty
                || values_call.result != values_expression.ty
                || !values_call.type_arguments.is_empty()
                || values_call.permission.is_some()
                || !values_call.session.is_empty()
                || !ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(
                    values_expression,
                    values_call,
                )
            {
                return None;
            }
            let snapshot_type = values_expression.ty.clone();
            let ResolvedType::Nominal {
                item, arguments, ..
            } = self.types.get(&snapshot_type)?
            else {
                return None;
            };
            if item.as_str() != "builtin:type:List"
                || arguments.len() != 1
                || !matches!(self.types.get(&arguments[0]), Some(ResolvedType::DynamicAny { capability }) if capability == "type.dynamic_value")
            {
                return None;
            }
            let mut success_drop_locals = descendants;
            success_drop_locals.extend(self.payload_locals.iter().cloned());
            let expected_drops = success_drop_locals.clone();
            let required_count = 1 + expected_drops.len();
            if ok_block.statements.len() != required_count {
                return None;
            }
            let drop_stmts = &ok_block.statements[1..];
            let drops = drop_stmts
                .iter()
                .zip(&expected_drops)
                .map(|(statement, local)| direct_drop(statement, local))
                .collect::<Option<Vec<_>>>()?;
            let return_expression = ok_block.result.as_deref()?;
            let ResolvedExprKind::Cast { value, conversion } = &return_expression.kind else {
                return None;
            };
            if conversion.kind != crate::core::ir::CheckedConversionKind::ContainerErase
                || conversion.from != snapshot_type
                || !direct_load(value, snapshot_local, &snapshot_type)
                || value.ty != snapshot_type
                || !matches!(self.types.get(&conversion.to), Some(ResolvedType::Nominal { item, arguments, .. }) if item.as_str() == "builtin:type:List" && arguments.is_empty())
                || return_expression.ty != conversion.to
            {
                return None;
            }
            self.values_operation = Some(values_expression.node_id.clone());
            self.snapshot_binding_point = Some(ok_block.statements[0].node_id.clone());
            self.snapshot_local = Some(snapshot_local.clone());
            self.snapshot_type = Some(snapshot_type);
            self.surface_return_type = Some(conversion.to.clone());
            self.success_result = Some(return_expression.node_id.clone());
            self.paths.push(BranchPath {
                arm_path: ok_path,
                result: return_expression.node_id.clone(),
                kind: PathKind::ValuesSnapshot,
                drops,
            });
            for drop in self.paths.last()?.drops.iter() {
                if prior_roots.contains(&drop.local) || drop.local == view.output_root {
                    add_root_drop(&mut self.actions, &mut self.attempts, drop);
                }
            }
            let mut values_action = super::OrdinaryMapOwnedAnyV2Checker::build_action(
                OrdinaryMapOwnedAnyV2ActionKind::Values,
                values_expression.node_id.clone(),
                view.output_root.clone(),
                None,
                None,
                None,
                None,
                false,
                Some(view.output_root.clone()),
            );
            values_action.source_node = Some(values_call.arguments[0].value.node_id.clone());
            values_action.source_type = Some(output_root_type.clone());
            values_action.result_type = Some(values_expression.ty.clone());
            values_action.result_local = Some(snapshot_local.clone());
            self.actions.push(values_action);
            self.attempts.insert(attempt_from_call(
                OrdinaryMapOwnedAnyV2AttemptKind::Values,
                &values_expression.node_id,
                &[values_call.arguments[0].value.node_id.clone()],
                Some(&view.output_root),
                Some(snapshot_local),
            ));
        }
        Some(())
    }
}

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
) -> Option<Collected> {
    if cfg.owner != body.owner
        || signature.owner != body.owner
        || !signature.parameters.is_empty()
        || !signature.generic_parameters.is_empty()
        || !body.parameters.is_empty()
        || !body.captures.is_empty()
        || !body.place_inputs.is_empty()
        || !body.default_values.is_empty()
    {
        return None;
    }
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    if !matches!(
        types.get(&signature.result),
        Some(ResolvedType::Nominal { item, arguments, .. })
            if item.as_str() == "builtin:type:List" && arguments.is_empty()
    ) || body.root.ty != signature.result
    {
        return None;
    }
    let payload_statements = body.root.statements.as_slice();
    if !(payload_statements.len() == 1 || payload_statements.len() == 2) {
        return None;
    }
    let payload_locals = payload_statements
        .iter()
        .map(|statement| {
            let (local, initializer) = immutable_binding(statement, body)?;
            (matches!(
                &initializer.kind,
                ResolvedExprKind::Literal(ResolvedLiteral::String(_))
            ) && initializer.ty == string_type)
                .then_some(local.clone())
        })
        .collect::<Option<Vec<_>>>()?;
    let root_result = body.root.result.as_deref()?;
    let ResolvedExprKind::Match {
        scrutinee: new_call_expr,
        arms: new_arms,
    } = &root_result.kind
    else {
        return None;
    };
    let ResolvedExprKind::Call(new_call) = &new_call_expr.kind else {
        return None;
    };
    if !matches!(&new_call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "map_new")
        || !new_call.arguments.is_empty()
        || !new_call.type_arguments.is_empty()
        || new_call.permission.is_some()
        || !new_call.session.is_empty()
        || new_call.result != new_call_expr.ty
        || !ordinary_map_owned_any_v2_map_builtin_metadata_is_exact(new_call_expr, new_call)
    {
        return None;
    }
    let (root_type, new_error_type) = result_parts(types, &new_call_expr.ty)?;
    if !is_record(types, root_type) || new_error_type != &i32_type || new_arms.len() != 2 {
        return None;
    }
    let new_ok_arm = variant_arm(new_arms, "Ok")?;
    let new_err_arm = variant_arm(new_arms, "Err")?;
    let (initial_root, new_ok_pattern) =
        payload_pattern(new_ok_arm, "Ok", &new_call_expr.ty, root_type, body)?;
    let (new_error_local, new_err_pattern) =
        payload_pattern(new_err_arm, "Err", &new_call_expr.ty, &i32_type, body)?;
    let new_err_block = get_block(new_err_arm)?;
    let new_err_expected = payload_locals.clone();
    let new_err_drops = drop_sequence(new_err_block, &new_err_expected)?;
    let new_err_result = new_err_block.result.as_deref()?;
    if !empty_list(new_err_result, types) {
        return None;
    }
    let mut new_action = super::OrdinaryMapOwnedAnyV2Checker::build_action(
        OrdinaryMapOwnedAnyV2ActionKind::New,
        new_call_expr.node_id.clone(),
        initial_root.clone(),
        None,
        None,
        None,
        None,
        false,
        None,
    );
    new_action.source_node = Some(new_call_expr.node_id.clone());
    new_action.source_type = Some(new_call_expr.ty.clone());
    let new_ok_block = get_block(new_ok_arm)?;
    if !new_ok_block.statements.is_empty() {
        return None;
    }
    let first_set = new_ok_block.result.as_deref()?;
    let mut recursive = RecursiveBuild {
        body,
        types,
        i32_type: &i32_type,
        payload_locals: &payload_locals,
        all_sets: Vec::new(),
        paths: vec![BranchPath {
            arm_path: vec![new_err_arm.node_id.clone()],
            result: new_err_result.node_id.clone(),
            kind: PathKind::EmptyList,
            drops: new_err_drops,
        }],
        match_arms: BTreeMap::from([(
            new_call_expr.node_id.clone(),
            BTreeSet::from([new_ok_arm.node_id.clone(), new_err_arm.node_id.clone()]),
        )]),
        set_count: 0,
        values_operation: None,
        snapshot_binding_point: None,
        snapshot_local: None,
        snapshot_type: None,
        surface_return_type: None,
        success_result: None,
        actions: vec![new_action],
        attempts: BTreeSet::from([attempt_from_call(
            OrdinaryMapOwnedAnyV2AttemptKind::New,
            &new_call_expr.node_id,
            &[],
            None,
            None,
        )]),
    };
    recursive.process_set(
        first_set,
        initial_root,
        std::slice::from_ref(initial_root),
        std::slice::from_ref(&new_ok_arm.node_id),
    )?;
    let count = recursive.set_count;
    if count != payload_locals.len() || recursive.all_sets.len() != count {
        return None;
    }
    let profile_version = match count {
        1 => ProfileVersion::V1,
        2 => ProfileVersion::V2,
        _ => return None,
    };
    let values_operation = recursive.values_operation.clone()?;
    let values_action = recursive.actions.iter().find(|action| {
        action.kind == OrdinaryMapOwnedAnyV2ActionKind::Values && action.point == values_operation
    })?;
    if values_action.result_type != recursive.snapshot_type
        || recursive.actions.len() != (if count == 1 { 6 } else { 10 })
        || recursive.attempts.len() != recursive.actions.len()
        || recursive.actions.iter().any(|action| {
            !matches!(
                action.kind,
                OrdinaryMapOwnedAnyV2ActionKind::New
                    | OrdinaryMapOwnedAnyV2ActionKind::Set
                    | OrdinaryMapOwnedAnyV2ActionKind::Values
                    | OrdinaryMapOwnedAnyV2ActionKind::Drop
            )
        })
    {
        return None;
    }
    let set_action_points = recursive
        .all_sets
        .iter()
        .map(|set| set.operation.clone())
        .collect::<BTreeSet<_>>();
    let action_set_points = recursive
        .actions
        .iter()
        .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Set)
        .map(|action| action.point.clone())
        .collect::<BTreeSet<_>>();
    if action_set_points != set_action_points
        || recursive
            .actions
            .iter()
            .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::New)
            .count()
            != 1
        || recursive
            .actions
            .iter()
            .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Values)
            .count()
            != 1
        || recursive
            .actions
            .iter()
            .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Drop)
            .count()
            != recursive
                .paths
                .iter()
                .flat_map(|path| path.drops.iter())
                .filter(|drop| {
                    drop.local == *initial_root
                        || recursive
                            .all_sets
                            .iter()
                            .any(|set| drop.local == set.output_root)
                })
                .count()
    {
        return None;
    }
    for branch in &recursive.all_sets {
        let mut found = recursive.actions.iter().filter(|action| {
            action.kind == OrdinaryMapOwnedAnyV2ActionKind::Set && action.point == branch.operation
        });
        let action = found.next()?;
        if found.next().is_some() {
            return None;
        }
        if action.source_local.as_ref() != Some(&branch.input_root)
            || action.local != branch.output_root
        {
            return None;
        }
    }
    let root_locals = std::iter::once(initial_root.clone())
        .chain(recursive.all_sets.iter().map(|set| set.output_root.clone()))
        .collect::<BTreeSet<_>>();
    let expected_drop_points = recursive
        .paths
        .iter()
        .flat_map(|path| path.drops.iter())
        .filter(|drop| root_locals.contains(&drop.local))
        .map(|drop| (drop.local.clone(), drop.point.clone()))
        .collect::<BTreeSet<_>>();
    let observed_drop_points = recursive
        .actions
        .iter()
        .filter(|action| action.kind == OrdinaryMapOwnedAnyV2ActionKind::Drop)
        .map(|action| (action.local.clone(), action.point.clone()))
        .collect::<BTreeSet<_>>();
    if expected_drop_points != observed_drop_points {
        return None;
    }
    let values_source_local = recursive.all_sets.last()?.output_root.clone();
    let values_action = recursive.actions.iter().find(|action| {
        action.kind == OrdinaryMapOwnedAnyV2ActionKind::Values && action.point == values_operation
    })?;
    let values_source_node = values_action.source_node.clone()?;
    let values_source_type = values_action.source_type.clone()?;
    if recursive.attempts.len() != recursive.actions.len()
        || recursive.actions.iter().any(|action| {
            !recursive.attempts.iter().any(|attempt| {
                attempt.point == action.point
                    && matches!(
                        (action.kind, attempt.kind),
                        (
                            OrdinaryMapOwnedAnyV2ActionKind::New,
                            OrdinaryMapOwnedAnyV2AttemptKind::New
                        ) | (
                            OrdinaryMapOwnedAnyV2ActionKind::Set,
                            OrdinaryMapOwnedAnyV2AttemptKind::Set
                        ) | (
                            OrdinaryMapOwnedAnyV2ActionKind::Values,
                            OrdinaryMapOwnedAnyV2AttemptKind::Values
                        ) | (
                            OrdinaryMapOwnedAnyV2ActionKind::Drop,
                            OrdinaryMapOwnedAnyV2AttemptKind::Drop
                        )
                    )
            })
        })
    {
        return None;
    }
    let expected_locals = payload_locals
        .iter()
        .cloned()
        .chain(std::iter::once(initial_root.clone()))
        .chain(std::iter::once(new_error_local.clone()))
        .chain(
            recursive
                .all_sets
                .iter()
                .flat_map(|set| [set.output_root.clone(), set.error_local.clone()]),
        )
        .chain(std::iter::once(recursive.snapshot_local.clone()?))
        .collect::<BTreeSet<_>>();
    if expected_locals.len() != body.locals.len()
        || expected_locals != body.locals.keys().cloned().collect::<BTreeSet<_>>()
    {
        return None;
    }
    let mut source_nodes = root_result
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    source_nodes.insert(body.root.node_id.clone());
    for statement in payload_statements {
        source_nodes.insert(statement.node_id.clone());
        if let ResolvedStmtKind::Bind {
            initializer: Some(initializer),
            ..
        } = &statement.kind
        {
            source_nodes.extend(initializer.eager_source_nodes());
        }
    }
    let mut cfg_matches = recursive.match_arms.clone();
    for set in &recursive.all_sets {
        cfg_matches
            .entry(set.operation.clone())
            .or_insert_with(|| BTreeSet::from([set.ok_arm.clone(), set.err_arm.clone()]));
    }
    let cfg_receipt =
        exact_cfg_receipt(cfg, body, &cfg_matches, &source_nodes, &root_result.node_id)?;
    let root_locals = std::iter::once(initial_root.clone())
        .chain(recursive.all_sets.iter().map(|set| set.output_root.clone()))
        .collect::<BTreeSet<_>>();
    let root_settlements = recursive
        .paths
        .iter()
        .flat_map(|path| path.drops.iter())
        .filter(|drop| root_locals.contains(&drop.local))
        .cloned()
        .collect::<Vec<_>>();
    let payload_settlements = recursive
        .paths
        .iter()
        .flat_map(|path| path.drops.iter())
        .filter(|drop| payload_locals.contains(&drop.local))
        .cloned()
        .collect::<Vec<_>>();
    let receipt = ProducerReceipt {
        profile_version,
        owner: body.owner.clone(),
        map_new_operation: new_call_expr.node_id.clone(),
        map_new_match: root_result.node_id.clone(),
        map_new_ok_arm: new_ok_arm.node_id.clone(),
        map_new_ok_pattern: new_ok_pattern.node_id.clone(),
        initial_root: initial_root.clone(),
        map_new_err_arm: new_err_arm.node_id.clone(),
        map_new_err_pattern: new_err_pattern.node_id.clone(),
        map_new_error_local: new_error_local.clone(),
        set_branches: recursive.all_sets,
        values_operation,
        values_source_local: values_source_local.clone(),
        values_source_node,
        values_source_type,
        values_binding_point: recursive.snapshot_binding_point?,
        snapshot_local: recursive.snapshot_local?,
        snapshot_type: recursive.snapshot_type?,
        surface_return_type: recursive.surface_return_type?,
        success_result: recursive.success_result?,
        paths: recursive.paths,
        root_settlements,
        payload_settlements,
        cfg: cfg_receipt,
    };
    let root_loads = recursive
        .actions
        .iter()
        .filter(|action| {
            matches!(
                action.kind,
                OrdinaryMapOwnedAnyV2ActionKind::Set | OrdinaryMapOwnedAnyV2ActionKind::Values
            )
        })
        .filter_map(|action| Some((action.source_node.clone()?, action.source_local.clone()?)))
        .collect::<Vec<_>>();
    Some(Collected {
        receipt,
        actions: recursive.actions,
        attempts: recursive.attempts,
        root_loads,
    })
}
