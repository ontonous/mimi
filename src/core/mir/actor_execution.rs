//! Shared preflight for the finite scalar Actor execution graph.
//!
//! Native status-return ABI is a whole reachable-graph obligation. A wrapper
//! around the dispatcher cannot authorize ordinary aborting helpers beneath
//! it. The root entry also returns status, so it closes/joins before reporting
//! an error that occurred while live Actors existed.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::ResolvedCallee;
use crate::core::NodeId;

use super::actor::{MirActorCatalog, MirActorOperation};
use super::types::{MirAbiClass, MirBuiltinKind, MirTypeCatalog, MirTypeKind};
use super::{MirFunction, MirInstructionKind, MirTerminator};

pub const MIR_ACTOR_STATUS_ABI_SCHEMA: &str = "mimi-mir-actor-status-abi-v1";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirActorNativeCallableAbi {
    Ordinary,
    /// Every generated return contains signed-i32 status followed by the
    /// source i32 value. Value is observable only when status is zero.
    TaggedI32 {
        schema: &'static str,
        status_bits: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirActorExecutionPlan {
    pub root: NodeId,
    pub worker_functions: BTreeSet<NodeId>,
    pub tagged_functions: BTreeSet<NodeId>,
    snapshot_root: NodeId,
    snapshot_workers: BTreeSet<NodeId>,
    snapshot_functions: BTreeMap<NodeId, String>,
}

impl MirActorExecutionPlan {
    pub(crate) fn preflight(
        actors: &MirActorCatalog,
        types: &MirTypeCatalog,
        functions: &BTreeMap<NodeId, MirFunction>,
        root: &NodeId,
    ) -> Result<Self, Vec<String>> {
        actors.validate_functions(types, functions)?;
        let mut errors = Vec::new();
        let worker_roots = actors
            .iter()
            .flat_map(|(_, actor)| actor.methods.iter().map(|method| method.body_owner.clone()))
            .collect::<BTreeSet<_>>();
        if worker_roots.is_empty() {
            return Err(vec![
                "Actor execution requires a selected checker method catalog".into(),
            ]);
        }
        for (_, actor) in actors.iter() {
            for method in &actor.methods {
                let Some(function) = functions.get(&method.body_owner) else {
                    errors.push(format!(
                        "Actor body '{}' is absent from the executable MIR table",
                        method.body_owner.0
                    ));
                    continue;
                };
                let parameter_types = function
                    .parameters
                    .iter()
                    .filter_map(|value| function.values.get(value).map(|value| value.ty.clone()))
                    .collect::<Vec<_>>();
                if function.parameters.first() != Some(&method.self_value)
                    || parameter_types != method.execution_parameter_types
                    || function.parameters.len() != method.execution_parameter_types.len()
                    || function.result != method.execution_result_type
                    || function
                        .parameter_permissions
                        .as_ref()
                        .is_none_or(|permissions| {
                            permissions.len() != function.parameters.len()
                                || permissions.iter().any(Option::is_some)
                        })
                {
                    errors.push(format!("Actor body '{}' internal receiver/parameter/result ABI disagrees with its checker contract", method.body_owner.0));
                }
            }
        }
        let workers = reachable(functions, &worker_roots, &mut errors);
        let mut roots = worker_roots;
        roots.insert(root.clone());
        let tagged = reachable(functions, &roots, &mut errors);
        for owner in &tagged {
            let Some(function) = functions.get(owner) else {
                continue;
            };
            let root_flow_state_values = if owner == root {
                checked_flow_state_values(function, actors, root)
            } else {
                BTreeSet::new()
            };
            if let Err(validation) = function.validate() {
                errors.extend(validation.into_iter().map(|error| error.to_string()));
            }
            if function
                .parameter_permissions
                .as_ref()
                .is_none_or(|permissions| {
                    permissions.len() != function.parameters.len()
                        || permissions.iter().any(Option::is_some)
                })
            {
                errors.push(format!(
                    "Actor status function '{}' lacks its concrete no-borrow signature receipt",
                    owner.0
                ));
            }
            if !function.contracts.is_empty() {
                errors.push(format!("Actor status function '{}' requires a separate scalar contract-return protocol", owner.0));
            }
            finite_cfg(function, &mut errors);
            if !is_i32(types, &function.result) {
                errors.push(format!(
                    "Actor status ABI function '{}' requires an i32 result",
                    owner.0
                ));
            }
            for parameter in &function.parameters {
                let Some(value) = function.values.get(parameter) else {
                    continue;
                };
                let is_method_receiver = actors.method(owner).is_some_and(|(actor, method)| {
                    method.flow_transition.is_none()
                        && parameter == &method.self_value
                        && value.ty == actor.state_type
                });
                if !is_method_receiver && !is_i32(types, &value.ty) {
                    errors.push(format!(
                        "Actor status ABI function '{}' has a non-i32 payload parameter",
                        owner.0
                    ));
                }
            }
            for (identity, value) in &function.values {
                let Some(descriptor) = types.get(&value.ty) else {
                    errors.push(format!(
                        "Actor function '{}' value TypeDesc is absent",
                        owner.0
                    ));
                    continue;
                };
                let admitted = descriptor.has_canonical_copy_noop_metadata()
                    && (is_i32(types, &value.ty)
                        || matches!(descriptor.abi, MirAbiClass::Bool | MirAbiClass::Unit))
                    || actors.method(owner).is_some_and(|(actor, method)| {
                        method.flow_transition.is_none()
                            && identity == &method.self_value
                            && value.ty == actor.state_type
                    })
                    || root_flow_state_values.contains(identity)
                    || owner == root && matches!(descriptor.kind, MirTypeKind::ActorHandle { .. });
                if !admitted {
                    errors.push(format!(
                        "Actor status ABI function '{}' has an unmodeled or escaping value '{}'",
                        owner.0,
                        identity.as_str()
                    ));
                }
            }
            for block in function.blocks.values() {
                for instruction in &block.instructions {
                    scalar_instruction_types(
                        function,
                        instruction,
                        actors,
                        &root_flow_state_values,
                        types,
                        functions,
                        &mut errors,
                    );
                    match &instruction.kind {
                        MirInstructionKind::Actor { operation } => {
                            let allowed = match operation {
                                MirActorOperation::Spawn { .. } | MirActorOperation::Call { .. } => !workers.contains(owner),
                                MirActorOperation::FieldLoad { .. } | MirActorOperation::FieldStore { .. } => actors.method(owner).is_some(),
                            };
                            if !allowed {
                                errors.push(format!("Actor operation '{}' violates finite worker/root role separation", instruction.id.as_str()));
                            }
                        }
                        MirInstructionKind::Call { callee, type_arguments, effect_receipts, variant_call_contract, .. } => {
                            if !type_arguments.is_empty() || !effect_receipts.is_empty() || variant_call_contract.is_some() {
                                errors.push(format!("Actor call '{}' has an unsupported generic/effect/variant ABI", instruction.id.as_str()));
                            }
                            if matches!(callee, ResolvedCallee::ActorMethod { .. } | ResolvedCallee::LocalClosure(_)) {
                                errors.push(format!("Actor call '{}' requires a separate mailbox/indirect-call contract", instruction.id.as_str()));
                            }
                        }
                        MirInstructionKind::BuiltinCall { kind: MirBuiltinKind::PrintlnInt | MirBuiltinKind::PrintlnBool, string_field_contract: None, .. } => {
                            if workers.contains(owner) { errors.push(format!("Actor worker instruction '{}' contains blocking output outside its finite turn contract", instruction.id.as_str())); }
                        }
                        MirInstructionKind::Const { .. }
                        | MirInstructionKind::Load { .. }
                        | MirInstructionKind::Project { .. }
                        | MirInstructionKind::Move { .. }
                        | MirInstructionKind::Clone { .. }
                        | MirInstructionKind::Drop { .. }
                        | MirInstructionKind::Copy { .. }
                        | MirInstructionKind::Binary { .. }
                        | MirInstructionKind::Unary { .. }
                        | MirInstructionKind::Nop => (),
                        _ => errors.push(format!("Actor status ABI instruction '{}' has no finite scalar turn implementation", instruction.id.as_str())),
                    }
                }
                match &block.terminator {
                    MirTerminator::Return { value: Some(_) }
                    | MirTerminator::Goto { .. }
                    | MirTerminator::Branch { .. } => (),
                    MirTerminator::Trap { code } => {
                        if let Err(message) = actor_trap_status(code) {
                            errors.push(message);
                        }
                    }
                    _ => errors.push(format!(
                        "Actor function '{}' terminator has no status-return implementation",
                        owner.0
                    )),
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Self {
            root: root.clone(),
            worker_functions: workers.clone(),
            tagged_functions: tagged.clone(),
            snapshot_root: root.clone(),
            snapshot_workers: workers,
            snapshot_functions: tagged
                .into_iter()
                .map(|owner| {
                    let text = functions
                        .get(&owner)
                        .expect("preflight checked function coverage")
                        .canonical_text();
                    (owner, text)
                })
                .collect(),
        })
    }

    pub fn validate_native_abis(
        &self,
        functions: &BTreeMap<NodeId, MirFunction>,
        declarations: &BTreeMap<NodeId, MirActorNativeCallableAbi>,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.root != self.snapshot_root
            || self.worker_functions != self.snapshot_workers
            || self.tagged_functions != self.snapshot_functions.keys().cloned().collect()
        {
            errors.push("Actor status graph differs from its preflight snapshot".into());
        }
        for (owner, snapshot) in &self.snapshot_functions {
            if !functions
                .get(owner)
                .is_some_and(|function| &function.canonical_text() == snapshot)
            {
                errors.push(format!(
                    "Actor status function '{}' changed after graph preflight",
                    owner.0
                ));
            }
            if declarations.get(owner)
                != Some(&MirActorNativeCallableAbi::TaggedI32 {
                    schema: MIR_ACTOR_STATUS_ABI_SCHEMA,
                    status_bits: 32,
                })
            {
                errors.push(format!("Actor reachable function '{}' lacks the mandatory tagged status ABI; ordinary aborting helpers are forbidden", owner.0));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn validate_against(
        &self,
        actors: &MirActorCatalog,
        types: &MirTypeCatalog,
        functions: &BTreeMap<NodeId, MirFunction>,
    ) -> Result<(), Vec<String>> {
        let rebuilt = Self::preflight(actors, types, functions, &self.snapshot_root)?;
        if self != &rebuilt {
            return Err(vec![
                "Actor execution graph differs from its checker materialization snapshot".into(),
            ]);
        }
        Ok(())
    }

    pub(crate) fn canonical_text(&self) -> String {
        format!(
            "mir.actor-execution schema={} root={} workers={:?} tagged={:?}\n",
            MIR_ACTOR_STATUS_ABI_SCHEMA, self.root.0, self.worker_functions, self.tagged_functions
        )
    }
}

fn checked_flow_state_values(
    function: &MirFunction,
    actors: &MirActorCatalog,
    root: &NodeId,
) -> BTreeSet<super::MirValueId> {
    if &function.owner != root {
        return BTreeSet::new();
    }
    let mut values = BTreeSet::new();
    for instruction in function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
    {
        if let MirInstructionKind::Actor {
            operation:
                MirActorOperation::Call {
                    result,
                    actor,
                    method,
                    ..
                },
        } = &instruction.kind
        {
            if actors
                .get(actor)
                .and_then(|contract| {
                    contract
                        .methods
                        .iter()
                        .find(|entry| &entry.owner == method && entry.flow_transition.is_some())
                        .and(contract.flow.as_ref())
                })
                .is_some_and(|flow| {
                    function
                        .values
                        .get(result)
                        .is_some_and(|value| value.ty == flow.root_state_type)
                })
            {
                values.insert(result.clone());
            }
        }
    }
    loop {
        let before = values.len();
        for instruction in function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
        {
            if let MirInstructionKind::Move { result, source } = &instruction.kind {
                if values.contains(source)
                    && function.values.get(result).map(|value| &value.ty)
                        == function.values.get(source).map(|value| &value.ty)
                {
                    values.insert(result.clone());
                }
            }
        }
        if values.len() == before {
            break;
        }
    }
    values
}

fn scalar_instruction_types(
    function: &MirFunction,
    instruction: &super::MirInstruction,
    actors: &MirActorCatalog,
    root_flow_state_values: &BTreeSet<super::MirValueId>,
    types: &MirTypeCatalog,
    functions: &BTreeMap<NodeId, MirFunction>,
    errors: &mut Vec<String>,
) {
    use crate::core::ir::{ResolvedBinaryOp as Binary, ResolvedLiteral, ResolvedUnaryOp as Unary};
    let ty = |value: &super::MirValueId| function.values.get(value).map(|value| &value.ty);
    let i32_value = |value: &super::MirValueId| ty(value).is_some_and(|ty| is_i32(types, ty));
    let primitive = |value: &super::MirValueId, expected| {
        ty(value).and_then(|ty| types.get(ty)).is_some_and(|desc| {
            desc.kind == MirTypeKind::Primitive(expected) && desc.has_canonical_copy_noop_metadata()
        })
    };
    let good = match &instruction.kind {
        MirInstructionKind::Const { result, literal } => match literal {
            ResolvedLiteral::Int(value) => i32_value(result) && i32::try_from(*value).is_ok(),
            ResolvedLiteral::Bool(_) => primitive(result, crate::core::PrimitiveType::Bool),
            ResolvedLiteral::Unit => primitive(result, crate::core::PrimitiveType::Unit),
            _ => false,
        },
        MirInstructionKind::Move { result, source } => {
            ty(result).is_some()
                && ty(result) == ty(source)
                && (ty(result)
                    .and_then(|ty| types.get(ty))
                    .is_some_and(|desc| desc.has_canonical_copy_noop_metadata())
                    || root_flow_state_values.contains(result)
                        && root_flow_state_values.contains(source))
        }
        MirInstructionKind::Copy { result, source }
        | MirInstructionKind::Clone { result, source } => {
            ty(result).is_some()
                && ty(result) == ty(source)
                && ty(result)
                    .and_then(|ty| types.get(ty))
                    .is_some_and(|desc| desc.has_canonical_copy_noop_metadata())
        }
        MirInstructionKind::Load { result, place } => {
            place.projections.is_empty()
                && super::MirValueId::new(format!("local:{}", place.base.0 .0))
                    .ok()
                    .is_some_and(|source| ty(result).is_some() && ty(result) == ty(&source))
        }
        MirInstructionKind::Binary {
            result,
            op,
            left,
            right,
        } => match op {
            Binary::Equal | Binary::NotEqual => {
                ty(left).is_some()
                    && ty(left) == ty(right)
                    && (i32_value(left) || primitive(left, crate::core::PrimitiveType::Bool))
                    && primitive(result, crate::core::PrimitiveType::Bool)
            }
            Binary::Less | Binary::Greater | Binary::LessEqual | Binary::GreaterEqual => {
                i32_value(left)
                    && i32_value(right)
                    && primitive(result, crate::core::PrimitiveType::Bool)
            }
            Binary::LogicalAnd | Binary::LogicalOr => {
                primitive(left, crate::core::PrimitiveType::Bool)
                    && primitive(right, crate::core::PrimitiveType::Bool)
                    && primitive(result, crate::core::PrimitiveType::Bool)
            }
            _ => i32_value(left) && i32_value(right) && i32_value(result),
        },
        MirInstructionKind::Unary {
            result,
            op,
            operand,
        } => match op {
            Unary::Negate => i32_value(result) && i32_value(operand),
            Unary::Not => {
                primitive(result, crate::core::PrimitiveType::Bool)
                    && primitive(operand, crate::core::PrimitiveType::Bool)
            }
            _ => false,
        },
        MirInstructionKind::Call {
            result: Some(result),
            callee: ResolvedCallee::Function(owner),
            arguments,
            ..
        } => functions.get(owner).is_some_and(|target| {
            ty(result) == Some(&target.result)
                && arguments.len() == target.parameters.len()
                && arguments
                    .iter()
                    .zip(&target.parameters)
                    .all(|(argument, parameter)| {
                        ty(argument).is_some()
                            && ty(argument) == target.values.get(parameter).map(|value| &value.ty)
                    })
        }),
        MirInstructionKind::BuiltinCall {
            result,
            kind,
            arguments,
            string_field_contract: None,
        } => {
            primitive(result, crate::core::PrimitiveType::Unit)
                && arguments.len() == 1
                && match kind {
                    MirBuiltinKind::PrintlnInt => i32_value(&arguments[0]),
                    MirBuiltinKind::PrintlnBool => {
                        primitive(&arguments[0], crate::core::PrimitiveType::Bool)
                    }
                    _ => false,
                }
        }
        MirInstructionKind::Project {
            result,
            base,
            projection: super::MirProjection::Field(field),
            list_index_contract: None,
        } => {
            root_flow_state_values.contains(base)
                && flow_payload_type_for_projection(actors, ty(base), field, ty(result), types)
        }
        MirInstructionKind::Drop { value } => root_flow_state_values.contains(value),
        MirInstructionKind::Actor { .. } | MirInstructionKind::Nop => true,
        _ => false,
    };
    if !good {
        errors.push(format!(
            "Actor scalar instruction '{}' has incompatible canonical operand/result types",
            instruction.id.as_str()
        ));
    }
}

fn flow_payload_type_for_projection(
    actors: &MirActorCatalog,
    base: Option<&crate::core::ir::ResolvedTypeId>,
    field: &NodeId,
    result: Option<&crate::core::ir::ResolvedTypeId>,
    types: &MirTypeCatalog,
) -> bool {
    let Some(base) = base else {
        return false;
    };
    actors.iter().any(|(_, actor)| {
        actor.flow.as_ref().is_some_and(|flow| {
            &flow.root_state_type == base
                && flow.payload.as_slice().first().is_some_and(|payload| {
                    flow.payload.len() == 1
                        && &payload.field == field
                        && result == Some(&payload.ty)
                        && is_i32(types, &payload.ty)
                })
        })
    })
}

fn is_i32(types: &MirTypeCatalog, ty: &crate::core::ir::ResolvedTypeId) -> bool {
    types.get(ty).is_some_and(|descriptor| {
        descriptor.has_canonical_copy_noop_metadata()
            && descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
            && descriptor.abi
                == (MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
    })
}

fn reachable(
    functions: &BTreeMap<NodeId, MirFunction>,
    roots: &BTreeSet<NodeId>,
    errors: &mut Vec<String>,
) -> BTreeSet<NodeId> {
    fn visit(
        owner: &NodeId,
        functions: &BTreeMap<NodeId, MirFunction>,
        visiting: &mut BTreeSet<NodeId>,
        complete: &mut BTreeSet<NodeId>,
        errors: &mut Vec<String>,
    ) {
        if complete.contains(owner) {
            return;
        }
        if !visiting.insert(owner.clone()) {
            errors.push(format!(
                "Actor reachable recursion through '{}' requires a separate finite-turn contract",
                owner.0
            ));
            return;
        }
        let Some(function) = functions.get(owner) else {
            errors.push(format!("Actor reachable function '{}' is absent", owner.0));
            visiting.remove(owner);
            return;
        };
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                if let MirInstructionKind::Call { callee, .. } = &instruction.kind {
                    if let ResolvedCallee::Function(target) = callee {
                        visit(target, functions, visiting, complete, errors);
                    } else {
                        errors.push(format!(
                            "Actor reachable call '{}' is outside the direct scalar graph",
                            instruction.id.as_str()
                        ));
                    }
                }
            }
        }
        visiting.remove(owner);
        complete.insert(owner.clone());
    }
    let mut complete = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    for root in roots {
        visit(root, functions, &mut visiting, &mut complete, errors);
    }
    complete
}

fn finite_cfg(function: &MirFunction, errors: &mut Vec<String>) {
    fn visit(
        block: &super::MirBlockId,
        function: &MirFunction,
        visiting: &mut BTreeSet<super::MirBlockId>,
        complete: &mut BTreeSet<super::MirBlockId>,
        errors: &mut Vec<String>,
    ) {
        if complete.contains(block) {
            return;
        }
        if !visiting.insert(block.clone()) {
            errors.push(format!(
                "Actor function '{}' CFG cycle requires a separate finite-turn contract",
                function.owner.0
            ));
            return;
        }
        if let Some(definition) = function.blocks.get(block) {
            let successors = match &definition.terminator {
                MirTerminator::Goto { target, .. } => vec![target],
                MirTerminator::Branch {
                    then_target,
                    else_target,
                    ..
                } => vec![then_target, else_target],
                _ => vec![],
            };
            for target in successors {
                visit(target, function, visiting, complete, errors);
            }
        }
        visiting.remove(block);
        complete.insert(block.clone());
    }
    visit(
        &function.entry,
        function,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
        errors,
    );
}

/// These codes match scalar MIR/reference/VM definedness. Formatting is
/// diagnostic data; unsupported errors must never silently become overflow or
/// zero-division. Generic Actor/runtime traps use E0800 explicitly.
pub(crate) fn actor_trap_status(message: &str) -> Result<i32, String> {
    let message = message.trim();
    for (code, status) in [("E0800", 800), ("E0801", 801), ("E0802", 802)] {
        if message == code
            || message.starts_with(&format!("{code}:"))
            || message.starts_with(&format!("[{code}]"))
        {
            return Ok(status);
        }
    }
    Err(format!(
        "Actor finite-turn trap '{message}' has no registered status ABI"
    ))
}
