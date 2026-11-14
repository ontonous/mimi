//! Exact L1 source/CFG authority for direct `match map_new()` followed by
//! Result-matched persistent Sets and the public tuple-valued `map_get`.
//!
//! This receipt is deliberately source-only. It binds the exact winning Set
//! TypeDesc, the tuple snapshot and all branch-local cleanup, but it does not
//! authorize a MIR route or claim that VM/native payload owners survive root
//! destruction. Those require independent L2/L3 consumers.

use super::*;
use crate::core::cfg::{BasicBlockId, CfgPointKind, EdgeId};
use crate::core::ir::{CheckedConversionKind, PrimitiveType, ResolvedProjection};
use crate::core::ownership::{
    ManagedAnyCfgReceipt, OrdinaryMapOwnedAnyV2Attempt,
    OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
    OrdinaryMapOwnedAnyV2DirectTupleGetUnpackPayload as PayloadValue,
    OrdinaryMapOwnedAnyV2DirectTupleGetUnpackProfileVersion as Version,
    OrdinaryMapOwnedAnyV2DirectTupleGetUnpackReceipt as Receipt,
    OrdinaryMapOwnedAnyV2DirectTupleGetUnpackSetReceipt as SetReceipt,
};
use crate::core::LocalId;

const MAX_SET_STEPS: usize = 8;

macro_rules! reject_when {
    ($condition:expr, $stage:literal) => {
        if $condition {
            return None;
        }
    };
}

macro_rules! reject {
    ($stage:literal) => {{
        return None;
    }};
}

macro_rules! option {
    ($value:expr, $stage:literal) => {{
        match $value {
            Some(value) => value,
            None => return None,
        }
    }};
}

fn builtin_call<'a>(
    expression: &'a ResolvedExpr,
    name: &str,
    arity: usize,
) -> Option<&'a ResolvedCall> {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return None;
    };
    (matches!(&call.callee, ResolvedCallee::Builtin(id) if id.as_str() == name)
        && call.arguments.len() == arity
        && call.result == expression.ty
        && call.type_arguments.is_empty()
        && call.permission.is_none()
        && call.session.is_empty())
    .then_some(call)
}

fn load(expression: &ResolvedExpr, local: &ResolvedLocalId, ty: &ResolvedTypeId) -> bool {
    expression.ty == *ty
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
            if place.base == *local && place.projections.is_empty())
}

fn tuple_payload_projection(
    expression: &ResolvedExpr,
    local: &ResolvedLocalId,
    any_type: &ResolvedTypeId,
) -> bool {
    expression.ty == *any_type
        && matches!(&expression.kind, ResolvedExprKind::Load(place)
            if place.base == *local
                && matches!(place.projections.as_slice(), [ResolvedProjection::Tuple {
                    index: 1,
                    ty,
                }] if ty == any_type))
}

fn variant_arm<'a>(arms: &'a [MatchArm], name: &str) -> Option<&'a MatchArm> {
    let expected = format!("builtin:variant:Result::{name}");
    let mut found = arms.iter().filter(|arm| {
        arm.guard.is_none()
            && matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. }
                if variant.0 == expected)
    });
    let arm = found.next()?;
    found.next().is_none().then_some(arm)
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

fn record_type(types: &ResolvedTypeTable, ty: &ResolvedTypeId) -> bool {
    matches!(types.get(ty), Some(ResolvedType::Nominal {
        item, arguments, is_linear: false,
    }) if item.as_str() == "builtin:type:Record" && arguments.is_empty())
}

fn payload<'a>(
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

fn drop_local(statement: &ResolvedStmt, local: &ResolvedLocalId) -> bool {
    matches!(&statement.kind, ResolvedStmtKind::Drop(places)
        if places.len() == 1
            && places[0].base == *local
            && places[0].projections.is_empty())
}

fn direct_arm_result(arm: &MatchArm) -> Option<&ResolvedExpr> {
    match &arm.body.kind {
        ResolvedExprKind::Block(block) => block.result.as_deref(),
        _ => Some(&arm.body),
    }
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

fn set_payload(
    expression: &ResolvedExpr,
    i32_type: &ResolvedTypeId,
    string_type: &ResolvedTypeId,
) -> Option<(ResolvedTypeId, PayloadValue)> {
    match (&expression.kind, &expression.ty) {
        (ResolvedExprKind::Literal(ResolvedLiteral::Int(value)), ty) if ty == i32_type => {
            Some((ty.clone(), PayloadValue::I32(*value)))
        }
        (ResolvedExprKind::Literal(ResolvedLiteral::String(value)), ty) if ty == string_type => {
            Some((ty.clone(), PayloadValue::String(value.clone())))
        }
        _ => None,
    }
}

fn has_point(cfg: &CallableCfg, block: &BasicBlockId, node: &NodeId, kind: CfgPointKind) -> bool {
    let Some(block) = cfg.blocks.get(block) else {
        return false;
    };
    let mut found = block
        .points
        .iter()
        .filter(|point| point.source.node == *node);
    found.next().is_some_and(|point| point.kind == kind) && found.next().is_none()
}

fn has_drop_point(
    cfg: &CallableCfg,
    block: &BasicBlockId,
    statement: &ResolvedStmt,
    local: &ResolvedLocalId,
) -> bool {
    cfg.blocks.get(block).is_some_and(|basic_block| {
        let mut points = basic_block
            .points
            .iter()
            .filter(|point| point.source.node == statement.node_id);
        points.next().is_some_and(|point| {
            point.kind == CfgPointKind::ResourceAction
                && point.read_places.len() == 1
                && point.read_places[0].base == LocalId(local.0.clone())
                && point.read_places[0].projections.is_empty()
                && point.write_places.is_empty()
                && points.next().is_none()
        })
    })
}

fn edge_for_arm<'a>(
    cfg: &'a CallableCfg,
    dispatch: &BasicBlockId,
    arm: &MatchArm,
) -> Option<(&'a EdgeId, &'a BasicBlockId)> {
    let Terminator::Match { arms, .. } = &cfg.blocks.get(dispatch)?.terminator else {
        return None;
    };
    let mut found = arms.iter().filter_map(|edge_id| {
        let edge = cfg.edges.get(edge_id)?;
        (edge.from == *dispatch
            && edge.kind == EdgeKind::MatchArm
            && edge.source.node == arm.node_id)
            .then_some((edge_id, &edge.to))
    });
    let edge = found.next()?;
    found.next().is_none().then_some(edge)
}

fn match_has_exact_two_arm_edges(cfg: &CallableCfg, dispatch: &BasicBlockId) -> bool {
    matches!(
        cfg.blocks.get(dispatch).map(|block| &block.terminator),
        Some(Terminator::Match { arms, .. }) if arms.len() == 2
    )
}

struct Terminal<'a> {
    set_ok_body: &'a ResolvedBlock,
    final_root: &'a ResolvedLocalId,
    get_binding: &'a ResolvedStmt,
    root_drop: &'a ResolvedStmt,
    unpack_binding: &'a ResolvedStmt,
    pair_drop: &'a ResolvedStmt,
    unpack_drop: &'a ResolvedStmt,
}

fn collect_set_chain<'a>(
    expression: &'a ResolvedExpr,
    input_root: &'a ResolvedLocalId,
    body: &'a ResolvedBody,
    types: &'a ResolvedTypeTable,
    i32_type: &ResolvedTypeId,
    string_type: &ResolvedTypeId,
    sets: &mut Vec<SetReceipt>,
) -> Option<Terminal<'a>> {
    reject_when!(sets.len() >= MAX_SET_STEPS, "Set chain step limit");
    let ResolvedExprKind::Match { scrutinee, arms } = &expression.kind else {
        reject!("Set chain scrutinee must be a Result match");
    };
    let set_call = option!(
        builtin_call(scrutinee, "map_set", 3),
        "Set chain map_set call shape"
    );
    let [receiver, key, payload_argument] = set_call.arguments.as_slice() else {
        reject!("Set call argument arity");
    };
    let (input_root_type, set_error_type) =
        option!(result_parts(types, &scrutinee.ty), "Set result type");
    reject_when!(
        expression.ty != *i32_type
            || set_error_type != i32_type
            || !record_type(types, input_root_type)
            || !load(&receiver.value, input_root, input_root_type)
            || receiver.conversion.kind != CheckedConversionKind::Identity
            || receiver.conversion.from != *input_root_type
            || receiver.conversion.to != *input_root_type
            || key.value.ty != *string_type
            || !matches!(
                &key.value.kind,
                ResolvedExprKind::Literal(ResolvedLiteral::String(_))
            )
            || key.conversion.kind != CheckedConversionKind::Identity
            || key.conversion.from != *string_type
            || key.conversion.to != *string_type
            || key.conversion.kind == CheckedConversionKind::DynamicAnyPack
            || payload_argument.conversion.kind != CheckedConversionKind::Identity,
        "Set root/key/payload types"
    );
    let (set_payload_type, payload_value) = option!(
        set_payload(&payload_argument.value, i32_type, string_type),
        "Set payload profile"
    );
    reject_when!(
        payload_argument.conversion.from != set_payload_type
            || payload_argument.conversion.to != set_payload_type,
        "Set payload conversion"
    );
    let ResolvedExprKind::Literal(ResolvedLiteral::String(key_value)) = &key.value.kind else {
        reject!("Set key literal identity");
    };
    let (output_root_type, output_error_type) =
        option!(result_parts(types, &scrutinee.ty), "Set output Result type");
    reject_when!(
        output_root_type != input_root_type || output_error_type != i32_type || arms.len() != 2,
        "Set Result arm types/cardinality"
    );
    let ok_arm = option!(variant_arm(arms, "Ok"), "Set Ok arm");
    let error_arm = option!(variant_arm(arms, "Err"), "Set Err arm");
    let (output_root, ok_pattern) = option!(
        payload(ok_arm, "Ok", &scrutinee.ty, output_root_type, body),
        "Set Ok owner binding"
    );
    let (error_local, error_pattern) = option!(
        payload(error_arm, "Err", &scrutinee.ty, i32_type, body),
        "Set Err status binding"
    );
    reject_when!(
        output_root == input_root
            || output_root == error_local
            || output_root_type != input_root_type,
        "Set result owner identity"
    );

    let ResolvedExprKind::Block(ok_body) = &ok_arm.body.kind else {
        reject!("Set Ok body block");
    };
    let ok_input_drop = option!(ok_body.statements.first(), "Set Ok input-root cleanup");
    reject_when!(
        !drop_local(ok_input_drop, input_root),
        "Set Ok input-root Drop"
    );
    let ResolvedExprKind::Block(error_body) = &error_arm.body.kind else {
        reject!("Set Err body block");
    };
    let [error_input_drop] = error_body.statements.as_slice() else {
        reject!("Set Err cleanup cardinality");
    };
    let error_result = option!(direct_arm_result(error_arm), "Set Err status result");
    reject_when!(
        !drop_local(error_input_drop, input_root) || !load(error_result, error_local, i32_type),
        "Set Err root/status cleanup"
    );

    sets.push(SetReceipt {
        operation: scrutinee.node_id.clone(),
        arguments: set_call
            .arguments
            .iter()
            .map(|argument| argument.value.node_id.clone())
            .collect(),
        input_root_local: input_root.clone(),
        key_node: key.value.node_id.clone(),
        key_value: key_value.clone(),
        payload_node: payload_argument.value.node_id.clone(),
        payload_type: set_payload_type,
        payload: payload_value,
        result_type: scrutinee.ty.clone(),
        match_node: expression.node_id.clone(),
        scrutinee: scrutinee.node_id.clone(),
        ok_arm: ok_arm.node_id.clone(),
        ok_pattern: ok_pattern.node_id.clone(),
        output_root_local: output_root.clone(),
        error_arm: error_arm.node_id.clone(),
        error_pattern: error_pattern.node_id.clone(),
        error_local: error_local.clone(),
        ok_input_drop: ok_input_drop.node_id.clone(),
        error_input_drop: error_input_drop.node_id.clone(),
    });

    if let Some(result) = ok_body.result.as_deref() {
        if ok_body.statements.len() == 1 && matches!(&result.kind, ResolvedExprKind::Match { .. }) {
            return collect_set_chain(
                result,
                output_root,
                body,
                types,
                i32_type,
                string_type,
                sets,
            );
        }
    }

    let [_, get_binding, root_drop, unpack_binding, pair_drop, unpack_drop] =
        ok_body.statements.as_slice()
    else {
        reject!("terminal Set Ok statement shape");
    };
    let (pair_local, _) = option!(
        immutable_binding(get_binding, body),
        "terminal Set Get pair binding"
    );
    reject_when!(
        !drop_local(root_drop, output_root) || !drop_local(pair_drop, pair_local),
        "terminal Set owner/pair cleanup"
    );
    Some(Terminal {
        set_ok_body: ok_body,
        final_root: output_root,
        get_binding,
        root_drop,
        unpack_binding,
        pair_drop,
        unpack_drop,
    })
}

pub(super) fn collect(
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    operations: &BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
) -> Option<Receipt> {
    let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
    let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    reject_when!(
        cfg.owner != body.owner
            || signature.owner != body.owner
            || signature.result != i32_type
            || body.root.ty != i32_type
            || !signature.generic_parameters.is_empty()
            || !signature.parameters.is_empty()
            || !body.parameters.is_empty()
            || !body.captures.is_empty()
            || !body.place_inputs.is_empty()
            || !body.default_values.is_empty()
            || !body.root.statements.is_empty(),
        "New callable/source envelope"
    );
    let new_match = option!(body.root.result.as_deref(), "New root result expression");
    let ResolvedExprKind::Match {
        scrutinee: new_scrutinee,
        arms: new_arms,
    } = &new_match.kind
    else {
        reject!("New source must be direct Match");
    };
    let _new_call = option!(
        builtin_call(new_scrutinee, "map_new", 0),
        "New call identity"
    );
    let (initial_root_type, new_error_type) = option!(
        result_parts(types, &new_scrutinee.ty),
        "New Result TypeDesc"
    );
    reject_when!(
        new_match.ty != i32_type
            || new_error_type != &i32_type
            || !record_type(types, initial_root_type)
            || new_arms.len() != 2,
        "New result/root type or arm cardinality"
    );
    let new_ok_arm = option!(variant_arm(new_arms, "Ok"), "New Ok arm");
    let new_error_arm = option!(variant_arm(new_arms, "Err"), "New Err arm");
    let (initial_root_local, initial_root_pattern) = option!(
        payload(new_ok_arm, "Ok", &new_scrutinee.ty, initial_root_type, body),
        "New owner binding"
    );
    let (new_error_local, new_error_pattern) = option!(
        payload(new_error_arm, "Err", &new_scrutinee.ty, &i32_type, body),
        "New Err status binding"
    );
    let new_error_result = option!(direct_arm_result(new_error_arm), "New Err status result");
    reject_when!(
        initial_root_local == new_error_local
            || !load(new_error_result, new_error_local, &i32_type),
        "New Err status/root binding shape"
    );
    // Resolved match arms wrap expression bodies in a block. Admit only the
    // exact zero-statement wrapper whose result is the first Set match; this
    // preserves the direct `match map_new()` source lineage.
    let initial_set_expression = match &new_ok_arm.body.kind {
        ResolvedExprKind::Match { .. } => &new_ok_arm.body,
        ResolvedExprKind::Block(block) if block.statements.is_empty() => {
            option!(block.result.as_deref(), "New Ok empty-block Set result")
        }
        _ => reject!("New Ok body direct Set-match shape"),
    };
    let mut sets = Vec::new();
    let terminal = option!(
        collect_set_chain(
            initial_set_expression,
            initial_root_local,
            body,
            types,
            &i32_type,
            &string_type,
            &mut sets,
        ),
        "Set chain completion"
    );
    let terminal_result = option!(
        terminal.set_ok_body.result.as_deref(),
        "terminal Set result expression"
    );
    reject_when!(
        sets.is_empty()
            || !matches!(
                &terminal_result.kind,
                ResolvedExprKind::Literal(ResolvedLiteral::Int(0))
            )
            || terminal_result.ty != i32_type,
        "terminal Set result status/zero shape"
    );

    let (pair_local, get_expression) = option!(
        immutable_binding(terminal.get_binding, body),
        "Get tuple binding"
    );
    let get_call = option!(
        builtin_call(get_expression, "map_get", 2),
        "tuple map_get call identity"
    );
    let [get_receiver, get_key] = get_call.arguments.as_slice() else {
        reject!("tuple map_get argument arity");
    };
    reject_when!(
        !load(&get_receiver.value, terminal.final_root, initial_root_type)
            || get_receiver.conversion.kind != CheckedConversionKind::Identity
            || get_receiver.conversion.from != *initial_root_type
            || get_receiver.conversion.to != *initial_root_type
            || get_key.value.ty != string_type
            || !matches!(
                &get_key.value.kind,
                ResolvedExprKind::Literal(ResolvedLiteral::String(_))
            )
            || get_key.conversion.kind != CheckedConversionKind::Identity
            || get_key.conversion.from != string_type
            || get_key.conversion.to != string_type,
        "tuple Get root/key types and conversions"
    );
    let ResolvedExprKind::Literal(ResolvedLiteral::String(get_key_value)) = &get_key.value.kind
    else {
        reject!("tuple Get exact key literal");
    };
    let get_pair_type = option!(types.get(&get_expression.ty), "tuple Get TypeDesc");
    let ResolvedType::Tuple(pair_types) = get_pair_type else {
        reject!("tuple Get result TypeDesc");
    };
    let [found_type, any_type] = pair_types.as_slice() else {
        reject!("tuple Get result arity");
    };
    let pair_binding = option!(body.locals.get(pair_local), "tuple Get pair local");
    reject_when!(
        found_type != &bool_type
            || !matches!(types.get(any_type), Some(ResolvedType::DynamicAny { .. }))
            || pair_binding.ty != get_expression.ty
            || pair_binding.mutable,
        "tuple Get bool/Any tags and pair binding"
    );
    let (unpack_local, unpack_expression) = option!(
        immutable_binding(terminal.unpack_binding, body),
        "Any unpack binding"
    );
    let ResolvedExprKind::Cast {
        value: unpack_argument,
        conversion: unpack_conversion,
    } = &unpack_expression.kind
    else {
        reject!("Any unpack conversion node");
    };
    let unpack_binding = option!(body.locals.get(unpack_local), "Any unpack local");
    reject_when!(
        unpack_conversion.kind != CheckedConversionKind::DynamicAnyUnpack
            || unpack_conversion.from != *any_type
            || !tuple_payload_projection(unpack_argument, pair_local, any_type)
            || unpack_expression.ty != unpack_conversion.to
            || unpack_conversion.to != i32_type && unpack_conversion.to != string_type
            || unpack_binding.ty != unpack_conversion.to
            || unpack_binding.mutable
            || !drop_local(terminal.pair_drop, pair_local)
            || !drop_local(terminal.unpack_drop, unpack_local),
        "Any unpack conversion/type/owner cleanup"
    );
    // Determine the payload type by the last exact literal-key Set. A miss
    // remains a distinct false/zero tuple result and authorizes only the
    // existing canonical i32 unpack behavior.
    let winning_set = sets
        .iter()
        .rev()
        .find(|set| set.key_value == *get_key_value);
    let selected_winning_set = if let Some(winner) = winning_set {
        if winner.payload_type != unpack_conversion.to {
            reject!("newest exact-key Set payload type does not match unpack target");
        }
        Some(winner.operation.clone())
    } else {
        if unpack_conversion.to != i32_type {
            reject!("static miss only admits canonical i32 unpack");
        }
        None
    };

    let get_root_drop = terminal.root_drop;
    let pair_local = pair_local.clone();
    let mut expected_operations = BTreeSet::from([OrdinaryMapOwnedAnyV2Attempt {
        kind: AttemptKind::New,
        point: new_scrutinee.node_id.clone(),
        arguments: Vec::new(),
        receiver_local: None,
        result_local: None,
    }]);
    for set in &sets {
        expected_operations.insert(OrdinaryMapOwnedAnyV2Attempt {
            kind: AttemptKind::Set,
            point: set.operation.clone(),
            arguments: set.arguments.clone(),
            receiver_local: Some(set.input_root_local.clone()),
            result_local: None,
        });
    }
    expected_operations.insert(OrdinaryMapOwnedAnyV2Attempt {
        kind: AttemptKind::GetAny,
        point: get_expression.node_id.clone(),
        arguments: get_call
            .arguments
            .iter()
            .map(|argument| argument.value.node_id.clone())
            .collect(),
        receiver_local: Some(terminal.final_root.clone()),
        result_local: Some(pair_local.clone()),
    });
    reject_when!(
        operations != &expected_operations,
        "Map operation attempt inventory"
    );
    let mut expected_locals = BTreeSet::from([
        initial_root_local.clone(),
        new_error_local.clone(),
        pair_local.clone(),
        unpack_local.clone(),
    ]);
    for set in &sets {
        if !expected_locals.insert(set.output_root_local.clone())
            || !expected_locals.insert(set.error_local.clone())
        {
            reject!("Set branch local identity uniqueness");
        }
    }
    let observed_locals = body.locals.keys().cloned().collect::<BTreeSet<_>>();
    reject_when!(
        observed_locals != expected_locals
            || cfg.reachable.len() != cfg.blocks.len()
            || cfg.blocks.keys().cloned().collect::<BTreeSet<_>>() != cfg.reachable
            || crate::core::cfg::validate_cfg(cfg).is_err(),
        "CFG validity, reachability, or exact local census"
    );
    let mut expected_match_scrutinees = BTreeSet::from([new_scrutinee.node_id.clone()]);
    expected_match_scrutinees.extend(sets.iter().map(|set| set.scrutinee.clone()));
    let mut actual_match_scrutinees = BTreeSet::new();
    let mut dispatches = BTreeMap::<NodeId, BasicBlockId>::new();
    for (block_id, block) in &cfg.blocks {
        match &block.terminator {
            Terminator::Match { scrutinee, .. } => {
                if !expected_match_scrutinees.contains(scrutinee)
                    || !actual_match_scrutinees.insert(scrutinee.clone())
                {
                    reject!("CFG Match scrutinee identity/cardinality");
                }
                dispatches.insert(scrutinee.clone(), block_id.clone());
            }
            Terminator::Goto { .. } | Terminator::Return { .. } => {}
            _ => reject!("CFG terminator outside direct Result-match profile"),
        }
    }
    reject_when!(
        actual_match_scrutinees != expected_match_scrutinees,
        "CFG match inventory"
    );
    let source_nodes = new_match
        .eager_source_nodes()
        .into_iter()
        .collect::<BTreeSet<_>>();
    let points = cfg
        .blocks
        .values()
        .flat_map(|block| block.points.iter())
        .collect::<Vec<_>>();
    let point_nodes = points
        .iter()
        .map(|point| point.source.node.clone())
        .collect::<BTreeSet<_>>();
    reject_when!(
        point_nodes.len() != points.len(),
        "CFG point source identity uniqueness"
    );
    reject_when!(
        points
            .iter()
            .any(|point| !source_nodes.contains(&point.source.node)),
        "CFG points belong to checked New expression tree"
    );
    reject_when!(
        !has_point(
            cfg,
            option!(
                dispatches.get(&new_scrutinee.node_id),
                "New dispatcher source-node lookup"
            ),
            &new_scrutinee.node_id,
            CfgPointKind::Condition,
        ),
        "New dispatcher has exact scrutinee source point"
    );

    // Match dispatch, arm identity, and every owner Drop must line up with
    // the exact source branch that produced it.
    let new_dispatch = option!(
        dispatches.get(&new_scrutinee.node_id),
        "New CFG dispatcher lookup"
    )
    .clone();
    if !match_has_exact_two_arm_edges(cfg, &new_dispatch) {
        reject!("New Result CFG arm edge census");
    }
    let (_new_ok_edge, first_set_dispatch) = option!(
        edge_for_arm(cfg, &new_dispatch, new_ok_arm),
        "New Ok CFG edge"
    );
    let (_, _new_error_block) = option!(
        edge_for_arm(cfg, &new_dispatch, new_error_arm),
        "New Err CFG edge"
    );
    let first_set = option!(sets.first(), "first Set receipt");
    if dispatches.get(&first_set.scrutinee) != Some(first_set_dispatch)
        || !has_point(
            cfg,
            &new_dispatch,
            &new_scrutinee.node_id,
            CfgPointKind::Condition,
        )
    {
        reject!("New Ok arm to first Set CFG dispatch");
    }
    let mut set_edges = Vec::new();
    for (index, set) in sets.iter().enumerate() {
        let set_dispatch =
            option!(dispatches.get(&set.scrutinee), "Set CFG dispatcher lookup").clone();
        let source_match = if index == 0 {
            initial_set_expression
        } else {
            let parent = &sets[index - 1];
            let parent_match = option!(
                find_match_by_node(new_match, &parent.match_node),
                "parent Set source Match lookup"
            );
            let ResolvedExprKind::Match {
                scrutinee: parent_scrutinee,
                arms: parent_arms,
            } = &parent_match.kind
            else {
                reject!("parent Set source Match identity");
            };
            let parent_ok_arm = option!(variant_arm(parent_arms, "Ok"), "parent Set source Ok arm");
            let parent_error_arm =
                option!(variant_arm(parent_arms, "Err"), "parent Set source Err arm");
            reject_when!(
                parent_match.node_id != parent.match_node
                    || parent_scrutinee.node_id != parent.scrutinee
                    || parent_ok_arm.node_id != parent.ok_arm
                    || parent_error_arm.node_id != parent.error_arm
                    || parent.output_root_local != set.input_root_local,
                "parent Set identity and owner continuity"
            );
            let ResolvedExprKind::Block(parent_ok_body) = &parent_ok_arm.body.kind else {
                reject!("parent Set Ok body block");
            };
            let [parent_drop] = parent_ok_body.statements.as_slice() else {
                reject!("parent Set Ok single root Drop");
            };
            reject_when!(
                parent_drop.node_id != parent.ok_input_drop
                    || !drop_local(parent_drop, &parent.input_root_local),
                "parent Set Ok root cleanup identity"
            );
            let child_result = option!(
                parent_ok_body.result.as_deref(),
                "child Set match from parent Ok result"
            );
            reject_when!(
                !matches!(&child_result.kind, ResolvedExprKind::Match { .. })
                    || child_result.node_id != set.match_node,
                "parent Ok result directly names child Set match"
            );
            let child_match = option!(
                find_match_by_node(new_match, &set.match_node),
                "child Set source Match lookup"
            );
            reject_when!(
                !std::ptr::eq(child_result, child_match),
                "child Set lookup is exact parent Ok result"
            );
            child_match
        };
        let ResolvedExprKind::Match {
            scrutinee: source_scrutinee,
            arms: source_arms,
        } = &source_match.kind
        else {
            reject!("current Set source Match identity");
        };
        reject_when!(
            source_match.node_id != set.match_node
                || source_scrutinee.node_id != set.scrutinee
                || (index == 0 && &set.input_root_local != initial_root_local),
            "current Set match, scrutinee, and input root identity"
        );
        let source_ok_arm = option!(variant_arm(source_arms, "Ok"), "current Set source Ok arm");
        let source_error_arm = option!(
            variant_arm(source_arms, "Err"),
            "current Set source Err arm"
        );
        if source_match.node_id != set.match_node
            || !match_has_exact_two_arm_edges(cfg, &set_dispatch)
            || cfg
                .blocks
                .get(&set_dispatch)
                .is_none_or(|block| match &block.terminator {
                    Terminator::Match { scrutinee, .. } => scrutinee != &set.scrutinee,
                    _ => true,
                })
        {
            reject!("Set CFG dispatch/source identity");
        }
        let (ok_edge, ok_block) = option!(
            edge_for_arm(cfg, &set_dispatch, source_ok_arm),
            "Set Ok CFG edge"
        );
        let (error_edge, error_block) = option!(
            edge_for_arm(cfg, &set_dispatch, source_error_arm),
            "Set Err CFG edge"
        );
        let ok_drop_statement = option!(
            find_drop_stmt(new_match, &set.ok_input_drop),
            "Set Ok Drop source lookup"
        );
        let error_drop_statement = option!(
            find_drop_stmt(new_match, &set.error_input_drop),
            "Set Err Drop source lookup"
        );
        // `map_set` is the scrutinee of this Result match. `lower_match`
        // anchors such operations as Condition points in the dispatch block.
        if !has_point(cfg, &set_dispatch, &set.operation, CfgPointKind::Condition) {
            reject!("Set Result scrutinee source point");
        }
        if !has_drop_point(cfg, ok_block, ok_drop_statement, &set.input_root_local)
            || !has_drop_point(
                cfg,
                error_block,
                error_drop_statement,
                &set.input_root_local,
            )
        {
            reject!("Set Result branch/drop CFG anchors");
        }
        let expected_next = sets
            .get(index + 1)
            .and_then(|next| dispatches.get(&next.scrutinee));
        if let Some(expected_next) = expected_next {
            if !reachable_via_gotos(cfg, ok_block, expected_next) {
                reject!("Set Ok CFG path to next Set");
            }
        } else if !reachable_via_gotos_to_point(cfg, ok_block, &get_expression.node_id) {
            reject!("terminal Set Ok CFG path to tuple Get");
        }
        let _ = (ok_edge, error_edge);
        set_edges.push((set_dispatch, ok_block.clone(), error_block.clone()));
    }

    let get_block = option!(
        find_block_for_point(cfg, &get_expression.node_id),
        "tuple Get CFG point block"
    );
    let unpack_block = option!(
        find_block_for_point(cfg, &unpack_expression.node_id),
        "Any unpack CFG point block"
    );
    let last_set_edge = option!(set_edges.last(), "last Set CFG branch");
    if !reachable_via_gotos_to_point(cfg, &last_set_edge.1, &get_expression.node_id)
        || !has_drop_point(cfg, get_block, get_root_drop, terminal.final_root)
        || !has_drop_point(cfg, get_block, terminal.pair_drop, &pair_local)
        || !has_drop_point(cfg, unpack_block, terminal.unpack_drop, unpack_local)
        || !has_point(
            cfg,
            unpack_block,
            &unpack_expression.node_id,
            CfgPointKind::Expression,
        )
    {
        reject!("branch-local root/pair/payload cleanup CFG anchors");
    }

    Some(Receipt {
        profile_version: Version::V1,
        owner: body.owner.clone(),
        source_body_commitment: format!("{:#?}", body.root),
        new_operation: new_scrutinee.node_id.clone(),
        new_result_type: new_scrutinee.ty.clone(),
        new_match: new_match.node_id.clone(),
        new_dispatch: new_dispatch.clone(),
        new_scrutinee: new_scrutinee.node_id.clone(),
        new_ok_arm: new_ok_arm.node_id.clone(),
        new_ok_pattern: initial_root_pattern.node_id.clone(),
        initial_root_local: initial_root_local.clone(),
        initial_root_type: initial_root_type.clone(),
        new_error_arm: new_error_arm.node_id.clone(),
        new_error_pattern: new_error_pattern.node_id.clone(),
        new_error_local: new_error_local.clone(),
        sets,
        get_operation: get_expression.node_id.clone(),
        get_receiver_local: terminal.final_root.clone(),
        get_key_node: get_key.value.node_id.clone(),
        get_key_value: get_key_value.clone(),
        pair_binding: terminal.get_binding.node_id.clone(),
        pair_local: pair_local.clone(),
        pair_type: get_expression.ty.clone(),
        pair_payload_projection: unpack_argument.node_id.clone(),
        root_get_drop: get_root_drop.node_id.clone(),
        unpack_operation: unpack_expression.node_id.clone(),
        unpack_conversion_kind: unpack_conversion.kind,
        unpack_source_type: unpack_conversion.from.clone(),
        unpack_target_type: unpack_conversion.to.clone(),
        unpack_local: unpack_local.clone(),
        pair_drop: terminal.pair_drop.node_id.clone(),
        unpack_drop: terminal.unpack_drop.node_id.clone(),
        selected_winning_set,
        operations: operations.clone(),
        cfg: ManagedAnyCfgReceipt::from_cfg(cfg, body),
    })
}

fn find_match_by_node<'a>(expression: &'a ResolvedExpr, node: &NodeId) -> Option<&'a ResolvedExpr> {
    if expression.node_id == *node {
        return Some(expression);
    }
    match &expression.kind {
        ResolvedExprKind::Block(block) => block
            .result
            .as_deref()
            .and_then(|result| find_match_by_node(result, node)),
        ResolvedExprKind::Match { arms, .. } => {
            for arm in arms {
                if let Some(found) = find_match_by_node(&arm.body, node) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn find_drop_stmt<'a>(expression: &'a ResolvedExpr, node: &NodeId) -> Option<&'a ResolvedStmt> {
    fn in_block<'a>(block: &'a ResolvedBlock, node: &NodeId) -> Option<&'a ResolvedStmt> {
        if let Some(stmt) = block.statements.iter().find(|stmt| stmt.node_id == *node) {
            return Some(stmt);
        }
        for stmt in &block.statements {
            if let ResolvedStmtKind::Bind {
                initializer: Some(expr),
                ..
            } = &stmt.kind
            {
                if let Some(found) = find_drop_stmt(expr, node) {
                    return Some(found);
                }
            }
        }
        block
            .result
            .as_deref()
            .and_then(|expr| find_drop_stmt(expr, node))
    }
    match &expression.kind {
        ResolvedExprKind::Block(block) => in_block(block, node),
        ResolvedExprKind::Match { arms, .. } => arms.iter().find_map(|arm| {
            if let ResolvedExprKind::Block(block) = &arm.body.kind {
                in_block(block, node)
            } else {
                find_drop_stmt(&arm.body, node)
            }
        }),
        _ => None,
    }
}

fn reachable_via_gotos(cfg: &CallableCfg, start: &BasicBlockId, target: &BasicBlockId) -> bool {
    let mut current = start.clone();
    let mut seen = BTreeSet::new();
    loop {
        if &current == target {
            return true;
        }
        if !seen.insert(current.clone()) {
            return false;
        }
        let Some(block) = cfg.blocks.get(&current) else {
            return false;
        };
        match &block.terminator {
            Terminator::Goto { edge } => {
                let Some(edge) = cfg.edges.get(edge) else {
                    return false;
                };
                current = edge.to.clone();
            }
            _ => return false,
        }
    }
}

fn reachable_via_gotos_to_point(cfg: &CallableCfg, start: &BasicBlockId, node: &NodeId) -> bool {
    let mut current = start.clone();
    let mut seen = BTreeSet::new();
    loop {
        if cfg
            .blocks
            .get(&current)
            .is_some_and(|block| block.points.iter().any(|point| point.source.node == *node))
        {
            return true;
        }
        if !seen.insert(current.clone()) {
            return false;
        }
        let Some(block) = cfg.blocks.get(&current) else {
            return false;
        };
        match &block.terminator {
            Terminator::Goto { edge } => {
                let Some(edge) = cfg.edges.get(edge) else {
                    return false;
                };
                current = edge.to.clone();
            }
            _ => return false,
        }
    }
}

fn find_block_for_point<'a>(cfg: &'a CallableCfg, node: &NodeId) -> Option<&'a BasicBlockId> {
    let mut found = cfg.blocks.iter().filter_map(|(block_id, block)| {
        block
            .points
            .iter()
            .any(|point| point.source.node == *node)
            .then_some(block_id)
    });
    let block = found.next()?;
    found.next().is_none().then_some(block)
}

pub(super) fn validate(
    receipt: &Receipt,
    cfg: &CallableCfg,
    body: &ResolvedBody,
    signature: &ResolvedSignature,
    types: &ResolvedTypeTable,
    operations: &BTreeSet<OrdinaryMapOwnedAnyV2Attempt>,
) -> Result<(), String> {
    if collect(cfg, body, signature, types, operations).as_ref() == Some(receipt) {
        Ok(())
    } else {
        Err("ordinary Map direct Result-matched tuple Get/unpack L1 receipt disagrees with exact Checker source, CFG, winner Set, or operation attempts".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> Result<crate::core::CheckedProgram, Vec<Diagnostic>> {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file)
    }

    fn multi_set_source(get_key: &str, unpack: &str) -> String {
        format!(
            r#"
func main() -> i32 {{
    match map_new() {{
        Ok(empty) => match map_set(empty, "old", 11) {{
            Ok(first) => {{
                drop(empty)
                match map_set(first, "target", "overwritten text") {{
                    Ok(second) => {{
                        drop(first)
                        match map_set(second, "target", 73) {{
                            Ok(root) => {{
                                drop(second)
                                match map_set(root, "text", "owned text") {{
                                    Ok(other) => {{
                                        drop(root)
                                        let pair = map_get(other, "{get_key}")
                                        drop(other)
                                        let decoded = {unpack}(pair.1)
                                        drop(pair)
                                        drop(decoded)
                                        0
                                    }},
                                    Err(status) => {{ drop(root); status }},
                                }}
                            }},
                            Err(status) => {{ drop(second); status }},
                        }}
                    }},
                    Err(status) => {{ drop(first); status }},
                }}
            }},
            Err(status) => {{ drop(empty); status }},
        }},
        Err(status) => {{ status }},
    }}
}}
"#
        )
    }

    fn direct_single_source(payload: &str, get_key: &str, unpack: &str) -> String {
        format!(
            r#"
func main() -> i32 {{
    match map_new() {{
        Ok(empty) => match map_set(empty, "stored", {payload}) {{
            Ok(root) => {{
                drop(empty)
                let pair = map_get(root, "{get_key}")
                drop(root)
                let decoded = {unpack}(pair.1)
                drop(pair)
                drop(decoded)
                0
            }},
            Err(status) => {{ drop(empty); status }},
        }},
        Err(status) => {{ status }},
    }}
}}
"#
        )
    }

    fn main_receipt(program: &crate::core::CheckedProgram) -> Option<&Receipt> {
        program
            .resource_analysis(&NodeId("function:main".into()))?
            .ordinary_map_owned_any_v2_direct_tuple_get_unpack_receipt
            .as_ref()
    }

    #[test]
    fn direct_new_match_and_four_set_winner_receipt_replay_exact_checker_cfg() {
        for (source, target_key, target_type, winner_index) in [
            (
                multi_set_source("target", "unpack_i32"),
                "target",
                crate::core::ir::primitive_type_id(PrimitiveType::I32),
                Some(2),
            ),
            (
                multi_set_source("text", "unpack_string"),
                "text",
                crate::core::ir::primitive_type_id(PrimitiveType::String),
                Some(3),
            ),
        ] {
            let program = checked(&source).expect("Result-matched direct source checks");
            let owner = NodeId("function:main".into());
            let callable = program.callable(&owner).expect("main callable");
            let receipt = main_receipt(&program).expect("direct tuple Get L1 receipt");
            validate(
                receipt,
                &callable.cfg,
                &callable.body,
                &callable.signature,
                program.resolved_types(),
                &callable
                    .resources
                    .ordinary_map_owned_any_v2_operation_attempts,
            )
            .expect("receipt replays exact Checker body, CFG, and attempts");
            assert_eq!(receipt.profile_version, Version::V1);
            assert_eq!(receipt.sets.len(), 4);
            assert_eq!(receipt.get_key_value, target_key);
            assert_eq!(receipt.unpack_target_type, target_type);
            assert_eq!(
                receipt.selected_winning_set.as_ref(),
                winner_index.map(|index| &receipt.sets[index].operation)
            );
            assert_eq!(receipt.cfg.owner, owner);
            assert!(!receipt.cfg.reachable_blocks.is_empty());
            assert_eq!(receipt.operations.len(), 6);
        }
    }

    #[test]
    fn direct_new_match_accepts_i32_miss_but_rejects_string_miss_and_wrong_winner_type() {
        let miss_i32 = direct_single_source("41", "missing", "unpack_i32");
        let miss_program = checked(&miss_i32).expect("canonical i32 miss remains admitted");
        let miss_receipt = main_receipt(&miss_program).expect("i32 miss direct tuple receipt");
        assert!(miss_receipt.selected_winning_set.is_none());

        for (source, expected, label) in [
            (
                direct_single_source(r#""owned text""#, "missing", "unpack_string"),
                "E0200",
                "String unpack at a static miss",
            ),
            (
                multi_set_source("target", "unpack_string"),
                "E0200",
                "String unpack when the last matching Set is i32",
            ),
        ] {
            let diagnostics = checked(&source).expect_err(label);
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code.as_deref() == Some(expected)),
                "{label} must fail with {expected}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn direct_tuple_get_receipt_requires_every_set_branch_to_settle_and_no_extra_get() {
        let base = multi_set_source("target", "unpack_i32");
        let mutations = [
            (
                "one Set Err branch leaks its input root",
                base.replace(
                    "Err(status) => { drop(second); status }",
                    "Err(status) => { status }",
                ),
            ),
            (
                "a Set Ok branch fails to settle its input root",
                base.replace(
                    "drop(first)\n                        match map_set",
                    "match map_set",
                ),
            ),
            (
                "an extra tuple Get is present",
                base.replace(
                    "drop(other)\n                                        let decoded",
                    "let extra = map_get(other, \"target\")\n                                        drop(extra)\n                                        drop(other)\n                                        let decoded",
                ),
            ),
        ];
        for (label, source) in mutations {
            assert_ne!(source, base, "{label} mutation applies");
            match checked(&source) {
                Ok(program) => assert!(
                    main_receipt(&program).is_none(),
                    "{label} must withhold the exact L1 receipt"
                ),
                Err(diagnostics) => assert!(
                    diagnostics.iter().any(|diagnostic| {
                        matches!(
                            diagnostic.code.as_deref(),
                            Some("E0200" | "E0256" | "E0304")
                        )
                    }),
                    "{label} must retain a source/ownership diagnostic: {diagnostics:?}"
                ),
            }
        }
    }
}
