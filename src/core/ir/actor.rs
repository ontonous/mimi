//! Checker-owned actor declarations for AST-free execution consumers.
//!
//! Declaration order, field initializers and method identities are fixed here,
//! before MIR or a backend chooses an execution profile. An unmaterialized
//! initializer is retained explicitly; it is never replaced with zero.

use std::collections::{BTreeMap, BTreeSet};

use super::{PrimitiveType, ResolvedLiteral, ResolvedSignature, ResolvedType, ResolvedTypeId};
use crate::core::{CheckedProgram, FlowId, NodeId, Origin, TransitionId};

pub const RESOLVED_ACTOR_SCHEMA: &str = "mimi-resolved-actor-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedActorInitializerKind {
    Literal(ResolvedLiteral),
    /// The checker accepts the source, but canonical initializer lowering
    /// does not yet implement this expression or default-value shape.
    Unmaterialized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActorFieldInitializer {
    pub node_id: NodeId,
    pub actor: NodeId,
    pub field: NodeId,
    pub ty: ResolvedTypeId,
    pub explicit: bool,
    pub kind: ResolvedActorInitializerKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActorFieldDescriptor {
    pub actor: NodeId,
    pub field: NodeId,
    pub name: String,
    pub ty: ResolvedTypeId,
    pub mutable: bool,
    pub initializer: ResolvedActorFieldInitializer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActorMethodDescriptor {
    pub actor: NodeId,
    pub owner: NodeId,
    pub name: String,
    /// Includes the checker-owned implicit self parameter at position zero.
    pub signature: ResolvedSignature,
    /// Native mailbox compatibility has an i64 sentinel for an omitted
    /// result annotation, independent of the checker-inferred body result.
    pub has_explicit_result: bool,
}

/// Checker-owned Flow state and dispatch identities attached to a `runs Flow`
/// Actor.  Names are descriptive only; every executable association is sealed
/// by the Flow/state/transition NodeIds and canonical signatures below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActorFlowDescriptor {
    pub flow: FlowId,
    pub flow_owner: NodeId,
    pub root_state: NodeId,
    pub root_state_type: ResolvedTypeId,
    pub payload: Vec<ResolvedActorFlowPayloadDescriptor>,
    pub transitions: Vec<ResolvedActorFlowTransitionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActorFlowPayloadDescriptor {
    pub field: NodeId,
    pub name: String,
    pub ty: ResolvedTypeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActorFlowTransitionDescriptor {
    pub transition: TransitionId,
    pub body_owner: NodeId,
    pub method_owner: NodeId,
    pub target_states: Vec<NodeId>,
    pub is_fallback: bool,
    pub is_ffi_pinned: bool,
    pub fails: bool,
    pub source_signature: ResolvedSignature,
    pub body_signature: ResolvedSignature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActorDescriptor {
    pub schema: &'static str,
    pub actor: NodeId,
    pub handle_type: ResolvedTypeId,
    /// Declaration order is the state layout order, not HashMap order.
    pub fields: Vec<ResolvedActorFieldDescriptor>,
    /// Declaration order is the mailbox dispatch order.
    pub methods: Vec<ResolvedActorMethodDescriptor>,
    pub runs_flow: Option<String>,
    /// Immutable checked Flow/state/transition identity snapshot for Actors
    /// declared with `runs Flow`.
    pub flow: Option<ResolvedActorFlowDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActorError {
    pub actor: NodeId,
    pub message: String,
}

impl std::fmt::Display for ResolvedActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "actor '{}': {}", self.actor.0, self.message)
    }
}

impl std::error::Error for ResolvedActorError {}

/// A private snapshot proving the initial scalar Actor declaration contract.
/// This does not claim that any execution backend implements Actor operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScalarActorReceipt {
    descriptor: ResolvedActorDescriptor,
    dispatch_methods: Vec<ResolvedActorMethodDescriptor>,
}

impl ResolvedScalarActorReceipt {
    pub fn descriptor(&self) -> &ResolvedActorDescriptor {
        &self.descriptor
    }

    /// The finite dispatch surface admitted by this scalar receipt. Generated
    /// Fault recovery verbs remain visible in the declaration descriptor but
    /// are not runtime entries in this closed non-failing profile.
    pub(crate) fn dispatch_methods(&self) -> &[ResolvedActorMethodDescriptor] {
        &self.dispatch_methods
    }
}

impl ResolvedActorDescriptor {
    fn error(&self, message: impl Into<String>) -> ResolvedActorError {
        ResolvedActorError {
            actor: self.actor.clone(),
            message: message.into(),
        }
    }

    /// Check both structural associations and the immutable checker snapshot.
    /// Consumers may inspect cloned public descriptors, but cannot authorize a
    /// forged initializer, field, signature or dispatch order from that clone.
    pub fn validate_against(&self, program: &CheckedProgram) -> Result<(), ResolvedActorError> {
        if self.schema != RESOLVED_ACTOR_SCHEMA {
            return Err(self.error("unknown declaration schema"));
        }
        let source = program
            .actors()
            .get(&self.actor)
            .ok_or_else(|| self.error("declaration owner is absent from CheckedProgram"))?;
        if !matches!(program.resolved_types().get(&self.handle_type),
            Some(ResolvedType::Nominal { item, arguments, is_linear: false })
                if item.as_str() == self.actor.0 && arguments.is_empty())
        {
            return Err(self.error("handle type does not identify the actor declaration"));
        }
        if source.fields.len() != self.fields.len() || source.methods.len() != self.methods.len() {
            return Err(self.error("field or method coverage disagrees with the actor declaration"));
        }
        let mut fields = BTreeSet::new();
        let mut initializers = BTreeSet::new();
        for (field, (source_name, _, source_mutable)) in self.fields.iter().zip(&source.fields) {
            if field.actor != self.actor
                || &field.name != source_name
                || field.mutable != *source_mutable
                || source.field_ids.get(&field.name) != Some(&field.field)
                || !fields.insert(field.field.clone())
            {
                return Err(self.error("field identity, owner or declaration order disagrees"));
            }
            if program.resolved_field_type(&field.field) != Some(&field.ty) {
                return Err(self.error("field type disagrees with the canonical field catalog"));
            }
            let init = &field.initializer;
            if init.actor != self.actor || init.field != field.field || init.ty != field.ty {
                return Err(self.error("initializer owner, field or type disagrees"));
            }
            if !initializers.insert(init.node_id.clone())
                || (init.explicit && !program.node_meta().contains_key(&init.node_id))
                || (!init.explicit && init.node_id != default_initializer_id(&field.field))
            {
                return Err(self.error("initializer identity is absent, duplicated or invalid"));
            }
            if let ResolvedActorInitializerKind::Literal(value) = &init.kind {
                if !literal_has_type(value, &init.ty, program) {
                    return Err(self.error("initializer value does not inhabit its declared type"));
                }
            }
        }
        let mut methods = BTreeSet::new();
        for (method, source_name) in self.methods.iter().zip(&source.methods) {
            let expected_owner =
                NodeId(format!("function:{}::{source_name}", source.qualified_name));
            if method.actor != self.actor
                || &method.name != source_name
                || method.owner != expected_owner
                || method.signature.owner != method.owner
                || !methods.insert(method.owner.clone())
                || program.resolved_signature(&method.owner) != Some(&method.signature)
            {
                return Err(self.error("method owner, signature or dispatch order disagrees"));
            }
            let Some(receiver) = method.signature.parameters.first() else {
                return Err(self.error("method has no self parameter"));
            };
            if receiver.name != "self" || receiver.ty != self.handle_type {
                return Err(self.error("method self parameter does not identify this actor"));
            }
        }
        match (&self.runs_flow, &self.flow) {
            (None, None) => (),
            (Some(name), Some(flow)) => {
                validate_actor_flow_descriptor(self, flow, name, program)?;
            }
            _ => return Err(self.error("runs-Flow name and checked Flow descriptor disagree")),
        }
        if program.actor_descriptors().get(&self.actor) != Some(self) {
            return Err(
                self.error("descriptor differs from the checker-owned initializer snapshot")
            );
        }
        Ok(())
    }

    /// Authorize only scalar i32 state and mailbox payloads. Richer actor
    /// declarations remain available for inspection, with no admission receipt.
    pub fn scalar_i32_receipt(
        &self,
        program: &CheckedProgram,
    ) -> Result<ResolvedScalarActorReceipt, ResolvedActorError> {
        self.validate_against(program)?;
        let is_i32 = |ty: &ResolvedTypeId| {
            matches!(
                program.resolved_types().get(ty),
                Some(ResolvedType::Primitive(PrimitiveType::I32))
            )
        };
        let flow_method_owners = self
            .flow
            .as_ref()
            .map(|flow| {
                flow.transitions
                    .iter()
                    .filter(|transition| !transition.is_fallback)
                    .map(|transition| transition.method_owner.clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let mut dispatch_methods = Vec::new();
        for field in &self.fields {
            if !is_i32(&field.ty)
                || !matches!(
                    field.initializer.kind,
                    ResolvedActorInitializerKind::Literal(ResolvedLiteral::Int(_))
                )
            {
                return Err(self.error("scalar Actor state requires materialized i32 initializers"));
            }
        }
        for method in &self.methods {
            let is_user_transition = flow_method_owners.contains(&method.owner)
                && program
                    .functions()
                    .get(&method.owner)
                    .is_some_and(|function| {
                        matches!(
                            &function.origin,
                            Origin::Desugared { rule, .. }
                                if rule == "resolved.runs_flow_transition_method"
                        )
                    });
            let is_generated_flow_method = self.flow.is_some()
                && program
                    .functions()
                    .get(&method.owner)
                    .is_some_and(|function| {
                        matches!(
                            &function.origin,
                            Origin::Desugared { rule, .. }
                                if rule == "resolved.runs_flow_transition_method"
                        )
                    });
            if is_generated_flow_method && !is_user_transition {
                continue;
            }
            let function = program
                .functions()
                .get(&method.owner)
                .ok_or_else(|| self.error("method function is absent"))?;
            if is_user_transition {
                // A synthetic Actor method is only a mailbox identity. Its
                // executable body is the checked Flow transition callable.
                validate_flow_method(self, method, program, is_i32)?;
                dispatch_methods.push(method.clone());
                continue;
            }
            if function.is_async
                || function.is_comptime
                || function.extern_abi.is_some()
                || !method.signature.generic_parameters.is_empty()
                || !is_i32(&method.signature.result)
                || method.signature.parameters.iter().skip(1).any(|parameter| {
                    !is_i32(&parameter.ty)
                        || parameter.permission.is_some()
                        || parameter.has_default
                })
            {
                return Err(
                    self.error("method is outside the concrete synchronous i32 mailbox contract")
                );
            }
            let callable = program
                .callable(&method.owner)
                .ok_or_else(|| self.error("method has no canonical callable body"))?;
            let has_nested_callable = callable.body.root.statements.iter().any(|statement| {
                matches!(
                    statement.kind,
                    crate::core::ir::ResolvedStmtKind::NestedCallable(_)
                )
            });
            if callable.owner != method.owner || callable.signature != method.signature {
                return Err(self.error("method callable and declaration signatures disagree"));
            }
            if !method.signature.effects.is_empty()
                && (!has_nested_callable
                    || method
                        .signature
                        .effects
                        .iter()
                        .any(|effect| effect.as_str() != "unknown"))
            {
                return Err(self.error(
                    "method effect set is outside the checked nested-callable Actor contract",
                ));
            }
            dispatch_methods.push(method.clone());
        }
        if let Some(flow) = &self.flow {
            validate_scalar_i32_flow_profile(self, flow, program, is_i32)?;
        }
        Ok(ResolvedScalarActorReceipt {
            descriptor: self.clone(),
            dispatch_methods,
        })
    }
}

fn default_initializer_id(field: &NodeId) -> NodeId {
    NodeId(format!("{}/actor-default-initializer", field.0))
}

fn literal_has_type(
    value: &ResolvedLiteral,
    ty: &ResolvedTypeId,
    program: &CheckedProgram,
) -> bool {
    use PrimitiveType as P;
    match (value, program.resolved_types().get(ty)) {
        (ResolvedLiteral::Int(value), Some(ResolvedType::Primitive(primitive))) => {
            match primitive {
                P::I8 => i8::try_from(*value).is_ok(),
                P::I16 => i16::try_from(*value).is_ok(),
                P::I32 => i32::try_from(*value).is_ok(),
                P::I64 | P::I128 | P::Isize => true,
                P::U8 => u8::try_from(*value).is_ok(),
                P::U16 => u16::try_from(*value).is_ok(),
                P::U32 => u32::try_from(*value).is_ok(),
                P::U64 | P::U128 | P::Usize => *value >= 0,
                _ => false,
            }
        }
        (ResolvedLiteral::FloatBits(bits), Some(ResolvedType::Primitive(P::F64))) => {
            f64::from_bits(*bits).is_finite()
        }
        (ResolvedLiteral::Bool(_), Some(ResolvedType::Primitive(P::Bool)))
        | (ResolvedLiteral::String(_), Some(ResolvedType::Primitive(P::String)))
        | (ResolvedLiteral::Unit, Some(ResolvedType::Primitive(P::Unit))) => true,
        _ => false,
    }
}

fn literal_initializer(expr: &crate::ast::Expr) -> Option<ResolvedLiteral> {
    use crate::ast::{Expr, Lit, UnOp};
    match expr.unlocated() {
        Expr::Literal(Lit::Int(value)) => Some(ResolvedLiteral::Int(*value)),
        Expr::Literal(Lit::Float(value)) => Some(ResolvedLiteral::float(*value)),
        Expr::Literal(Lit::Bool(value)) => Some(ResolvedLiteral::Bool(*value)),
        Expr::Literal(Lit::String(value)) => Some(ResolvedLiteral::String(value.clone())),
        Expr::Literal(Lit::Unit) => Some(ResolvedLiteral::Unit),
        Expr::Unary(UnOp::Neg, inner) => match literal_initializer(inner)? {
            ResolvedLiteral::Int(value) => value.checked_neg().map(ResolvedLiteral::Int),
            ResolvedLiteral::FloatBits(bits) => Some(ResolvedLiteral::float(-f64::from_bits(bits))),
            _ => None,
        },
        _ => None,
    }
}

fn default_initializer(ty: &ResolvedTypeId, program: &CheckedProgram) -> Option<ResolvedLiteral> {
    use PrimitiveType as P;
    match program.resolved_types().get(ty) {
        Some(ResolvedType::Primitive(
            P::I8
            | P::I16
            | P::I32
            | P::I64
            | P::I128
            | P::U8
            | P::U16
            | P::U32
            | P::U64
            | P::U128
            | P::Isize
            | P::Usize,
        )) => Some(ResolvedLiteral::Int(0)),
        Some(ResolvedType::Primitive(P::F64)) => Some(ResolvedLiteral::float(0.0)),
        Some(ResolvedType::Primitive(P::Bool)) => Some(ResolvedLiteral::Bool(false)),
        Some(ResolvedType::Primitive(P::String)) => Some(ResolvedLiteral::String(String::new())),
        Some(ResolvedType::Primitive(P::Unit)) => Some(ResolvedLiteral::Unit),
        _ => None,
    }
}

fn actor_flow_descriptor(
    actor: &crate::core::ResolvedActor,
    flow_name: Option<&str>,
    root_state_name: Option<&str>,
    program: &CheckedProgram,
) -> Result<Option<ResolvedActorFlowDescriptor>, String> {
    let Some(flow_name) = flow_name else {
        return Ok(None);
    };
    let flow_id = FlowId(flow_name.to_owned());
    let flow = program
        .flows()
        .get(&flow_id)
        .ok_or("runs-Flow target is absent from the checked Flow directory")?;
    let root_state_name = root_state_name.ok_or("runs-Flow target has no root state")?;
    let root = flow
        .states
        .get(root_state_name)
        .ok_or("runs-Flow root state is absent from the checked state directory")?;
    let root_state_type = program
        .resolved_types()
        .iter()
        .find_map(|(ty, shape)| {
            matches!(shape, ResolvedType::Nominal { item, arguments, is_linear: true }
                if item.as_str() == root.node_id.0 && arguments.is_empty())
            .then(|| ty.clone())
        })
        .ok_or("runs-Flow root state has no canonical nominal type")?;
    let payload = root
        .payload
        .iter()
        .map(|(name, _)| {
            let field = root
                .field_ids
                .get(name)
                .ok_or("runs-Flow root payload has no stable field identity")?;
            let ty = program
                .resolved_state_field_types()
                .get(field)
                .ok_or("runs-Flow root payload has no canonical field type")?;
            Ok(ResolvedActorFlowPayloadDescriptor {
                field: field.clone(),
                name: name.clone(),
                ty: ty.clone(),
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    let mut transitions = Vec::with_capacity(flow.transitions.len());
    for transition_id in &flow.transitions {
        let transition = program.transitions().get(transition_id).ok_or(
            "runs-Flow transition identity is absent from the checked transition directory",
        )?;
        let method_owner = NodeId(format!(
            "function:{}::{}",
            actor.qualified_name, transition_id.event
        ));
        let source_signature = program
            .resolved_signature(&method_owner)
            .ok_or("runs-Flow synthetic Actor method has no canonical signature")?
            .clone();
        let body_signature = program
            .resolved_signature(&transition.node_id)
            .ok_or("runs-Flow transition body has no canonical signature")?
            .clone();
        let target_states = transition
            .targets
            .iter()
            .map(|target| {
                flow.states
                    .get(&target.name)
                    .filter(|state| state.id == *target)
                    .map(|state| state.node_id.clone())
                    .ok_or("runs-Flow transition target is absent from the checked state directory")
            })
            .collect::<Result<Vec<_>, _>>()?;
        transitions.push(ResolvedActorFlowTransitionDescriptor {
            transition: transition_id.clone(),
            body_owner: transition.node_id.clone(),
            method_owner,
            target_states,
            is_fallback: transition.is_fallback,
            is_ffi_pinned: transition.is_ffi_pinned,
            fails: transition.fails.is_some(),
            source_signature,
            body_signature,
        });
    }
    Ok(Some(ResolvedActorFlowDescriptor {
        flow: flow.id.clone(),
        flow_owner: flow.node_id.clone(),
        root_state: root.node_id.clone(),
        root_state_type,
        payload,
        transitions,
    }))
}

fn validate_actor_flow_descriptor(
    actor: &ResolvedActorDescriptor,
    descriptor: &ResolvedActorFlowDescriptor,
    flow_name: &str,
    program: &CheckedProgram,
) -> Result<(), ResolvedActorError> {
    let flow_id = FlowId(flow_name.to_owned());
    let Some(flow) = program.flows().get(&flow_id) else {
        return Err(actor.error("runs-Flow target is absent from the checked Flow directory"));
    };
    if descriptor.flow != flow_id
        || descriptor.flow_owner != flow.node_id
        || descriptor.transitions.len() != flow.transitions.len()
    {
        return Err(actor.error("Flow owner or transition coverage differs from CheckedProgram"));
    }
    let Some(root) = flow
        .states
        .values()
        .find(|state| state.node_id == descriptor.root_state)
    else {
        return Err(actor.error("Flow root state identity is absent from CheckedProgram"));
    };
    if root.id.flow != flow_id
        || !matches!(
            program.resolved_types().get(&descriptor.root_state_type),
            Some(ResolvedType::Nominal { item, arguments, is_linear: true })
                if item.as_str() == root.node_id.0 && arguments.is_empty()
        )
        || descriptor.payload.len() != root.payload.len()
    {
        return Err(
            actor.error("Flow root-state type or payload coverage differs from CheckedProgram")
        );
    }
    for (entry, (name, _)) in descriptor.payload.iter().zip(&root.payload) {
        let Some(field) = root.field_ids.get(name) else {
            return Err(actor.error("Flow root payload field is absent from CheckedProgram"));
        };
        if entry.field != *field
            || entry.name != *name
            || program.resolved_state_field_types().get(field) != Some(&entry.ty)
        {
            return Err(
                actor.error("Flow root payload identity or type differs from CheckedProgram")
            );
        }
    }
    for (entry, transition_id) in descriptor.transitions.iter().zip(&flow.transitions) {
        let Some(transition) = program.transitions().get(transition_id) else {
            return Err(actor.error("Flow transition is absent from CheckedProgram"));
        };
        let Some(source_signature) = program.resolved_signature(&entry.method_owner) else {
            return Err(actor.error("Flow Actor method signature is absent from CheckedProgram"));
        };
        let Some(body_signature) = program.resolved_signature(&transition.node_id) else {
            return Err(actor.error("Flow body signature is absent from CheckedProgram"));
        };
        let expected_method = NodeId(format!(
            "function:{}::{}",
            program
                .actors()
                .get(&actor.actor)
                .map_or("", |source| source.qualified_name.as_str()),
            transition_id.event
        ));
        let expected_targets = transition
            .targets
            .iter()
            .filter_map(|target| flow.states.get(&target.name))
            .map(|state| state.node_id.clone())
            .collect::<Vec<_>>();
        if entry.transition != *transition_id
            || transition.id != *transition_id
            || entry.body_owner != transition.node_id
            || entry.method_owner != expected_method
            || entry.target_states != expected_targets
            || entry.is_fallback != transition.is_fallback
            || entry.is_ffi_pinned != transition.is_ffi_pinned
            || entry.fails != transition.fails.is_some()
            || entry.source_signature != *source_signature
            || entry.body_signature != *body_signature
            || entry.body_signature.owner != entry.body_owner
        {
            return Err(
                actor.error("Flow transition identity or signatures differ from CheckedProgram")
            );
        }
    }
    Ok(())
}

fn validate_flow_method(
    actor: &ResolvedActorDescriptor,
    method: &ResolvedActorMethodDescriptor,
    program: &CheckedProgram,
    is_i32: impl Fn(&ResolvedTypeId) -> bool,
) -> Result<(), ResolvedActorError> {
    let flow = actor
        .flow
        .as_ref()
        .ok_or_else(|| actor.error("Flow dispatch method has no checked Flow descriptor"))?;
    let transition = flow
        .transitions
        .iter()
        .find(|transition| transition.method_owner == method.owner && !transition.is_fallback)
        .ok_or_else(|| actor.error("Flow dispatch method has no transition body identity"))?;
    let callable = program
        .callable(&transition.body_owner)
        .ok_or_else(|| actor.error("Flow transition has no canonical callable body"))?;
    let body = &transition.body_signature;
    let source = &transition.source_signature;
    let parameters_match = source.parameters.len() == body.parameters.len()
        && source
            .parameters
            .iter()
            .skip(1)
            .zip(body.parameters.iter().skip(1))
            .all(|(source, body)| {
                source.ty == body.ty
                    && source.permission.is_none()
                    && !source.has_default
                    && is_i32(&source.ty)
            });
    let source_receiver = source.parameters.first();
    let body_receiver = body.parameters.first();
    if callable.owner != transition.body_owner
        || callable.signature != *body
        || !callable.body.captures.is_empty()
        || !callable.contracts.is_empty()
        || source.owner != method.owner
        || method.signature != *source
        || !source.generic_parameters.is_empty()
        || !body.generic_parameters.is_empty()
        || !source.effects.is_empty()
        || !body.effects.is_empty()
        || !parameters_match
        || source
            .parameters
            .len()
            .checked_sub(1)
            .is_none_or(|count| count > 64)
        || source_receiver.is_none_or(|parameter| {
            parameter.name != "self"
                || parameter.ty != actor.handle_type
                || parameter.permission == Some(crate::core::ir::Permission::Consume)
        })
        || body_receiver.is_none_or(|parameter| parameter.ty != flow.root_state_type)
        || source.result != flow.root_state_type
        || body.result != flow.root_state_type
    {
        return Err(
            actor.error("Flow transition body is outside the checked scalar i32 self-loop ABI")
        );
    }
    Ok(())
}

fn validate_scalar_i32_flow_profile(
    actor: &ResolvedActorDescriptor,
    flow_descriptor: &ResolvedActorFlowDescriptor,
    program: &CheckedProgram,
    is_i32: impl Fn(&ResolvedTypeId) -> bool,
) -> Result<(), ResolvedActorError> {
    let flow = program
        .flows()
        .get(&flow_descriptor.flow)
        .ok_or_else(|| actor.error("checked Flow directory entry is absent"))?;
    let user_states = flow
        .states
        .values()
        .filter(|state| matches!(&state.origin, Origin::User(_)))
        .collect::<Vec<_>>();
    if !actor.fields.is_empty()
        || !flow.generic_parameters.is_empty()
        || !flow.persistent_fields.is_empty()
        || user_states.len() != 1
        || user_states[0].node_id != flow_descriptor.root_state
        || flow_descriptor.payload.len() != 1
        || !is_i32(&flow_descriptor.payload[0].ty)
    {
        return Err(
            actor.error("runs-Flow Actor is outside the one-root-state i32 payload profile")
        );
    }
    let mut user_transitions = 0usize;
    for transition in &flow_descriptor.transitions {
        if transition.is_fallback {
            continue;
        }
        user_transitions += 1;
        if transition.is_ffi_pinned
            || transition.fails
            || transition.transition.source.name != user_states[0].id.name
            || transition.target_states.as_slice() != [flow_descriptor.root_state.clone()]
        {
            return Err(actor.error("Flow event is not a non-failing root-state self-loop"));
        }
        let method = actor
            .methods
            .iter()
            .find(|method| method.owner == transition.method_owner)
            .ok_or_else(|| actor.error("Flow event has no synthetic Actor method descriptor"))?;
        let function = program
            .functions()
            .get(&method.owner)
            .ok_or_else(|| actor.error("Flow event synthetic method is absent"))?;
        if !matches!(
            &function.origin,
            Origin::Desugared { rule, .. }
                if rule == "resolved.runs_flow_transition_method"
        ) {
            return Err(actor.error("Flow event collides with an explicit Actor method"));
        }
        validate_flow_method(actor, method, program, &is_i32)?;
    }
    if user_transitions == 0 {
        return Err(actor.error("runs-Flow Actor has no checked user transition dispatch entries"));
    }
    Ok(())
}

/// The only Actor descriptor producer. Surface syntax is consumed here during
/// CheckedProgram assembly and is not retained in the resulting artifact.
pub(crate) fn build_checked_actor_descriptors(
    file: &crate::ast::File,
    program: &CheckedProgram,
) -> Result<BTreeMap<NodeId, ResolvedActorDescriptor>, Vec<crate::diagnostic::Diagnostic>> {
    use crate::core::resolved::{expr_kind, stable_id_fragment, NodeIdBuilder};
    use crate::diagnostic::Diagnostic;
    let ids = NodeIdBuilder::new(&file.sources);
    let mut descriptors = BTreeMap::new();
    let mut errors = Vec::new();
    for item in &file.items {
        let crate::ast::Item::Actor(syntax) = item else {
            continue;
        };
        let actor_id = NodeId(format!("actor:{}", syntax.name));
        let build = || -> Result<ResolvedActorDescriptor, String> {
            let actor = program
                .actors()
                .get(&actor_id)
                .ok_or("actor is absent from the checked declaration directory")?;
            let handle_type = program
                .resolved_types()
                .iter()
                .find_map(|(ty, shape)| {
                    matches!(shape, ResolvedType::Nominal { item, arguments, is_linear: false }
                    if item.as_str() == actor_id.0 && arguments.is_empty())
                    .then(|| ty.clone())
                })
                .ok_or("actor handle type is absent from the canonical type table")?;
            let mut fields = Vec::new();
            for field in &syntax.fields {
                let field_id = actor
                    .field_ids
                    .get(&field.name)
                    .ok_or("actor field has no stable declaration identity")?
                    .clone();
                let ty = program
                    .resolved_field_type(&field_id)
                    .ok_or("actor field has no canonical type")?
                    .clone();
                let (node_id, value, explicit) = if let Some(init) = &field.init {
                    let meta = init.meta();
                    let anchor = meta
                        .map(|meta| meta.span)
                        .filter(|span| span.start_line > 0 && span.start_col > 0);
                    let mut identity_errors = Vec::new();
                    let node_id = ids.anonymous(
                        &actor_id,
                        expr_kind(init),
                        &format!("field.{}.initializer", stable_id_fragment(&field.name)),
                        anchor,
                        meta.map(|meta| meta.origin)
                            .unwrap_or(crate::ast::AstOrigin::User),
                        &mut identity_errors,
                    );
                    if !identity_errors.is_empty() || !program.node_meta().contains_key(&node_id) {
                        return Err("actor initializer has no checked source identity".into());
                    }
                    (node_id, literal_initializer(init), true)
                } else {
                    (
                        default_initializer_id(&field_id),
                        default_initializer(&ty, program),
                        false,
                    )
                };
                let kind = value
                    .filter(|value| literal_has_type(value, &ty, program))
                    .map(ResolvedActorInitializerKind::Literal)
                    .unwrap_or(ResolvedActorInitializerKind::Unmaterialized);
                fields.push(ResolvedActorFieldDescriptor {
                    actor: actor_id.clone(),
                    field: field_id.clone(),
                    name: field.name.clone(),
                    ty: ty.clone(),
                    mutable: field.mut_,
                    initializer: ResolvedActorFieldInitializer {
                        node_id,
                        actor: actor_id.clone(),
                        field: field_id,
                        ty,
                        explicit,
                        kind,
                    },
                });
            }
            let mut methods = Vec::new();
            for name in &actor.methods {
                let owner = NodeId(format!("function:{}::{name}", actor.qualified_name));
                let signature = program
                    .resolved_signature(&owner)
                    .ok_or_else(|| {
                        format!("actor method '{}' has no canonical signature", owner.0)
                    })?
                    .clone();
                methods.push(ResolvedActorMethodDescriptor {
                    actor: actor_id.clone(),
                    owner,
                    name: name.clone(),
                    signature,
                    has_explicit_result: syntax
                        .methods
                        .iter()
                        .find(|method| method.name == *name)
                        .is_some_and(|method| method.ret.is_some()),
                });
            }
            let flow_syntax = syntax.runs_flow.as_deref().and_then(|flow_name| {
                file.items.iter().find_map(|item| match item {
                    crate::ast::Item::Flow(flow) if flow.name == flow_name => Some(flow),
                    _ => None,
                })
            });
            let root_state_name = flow_syntax.and_then(|flow| {
                flow.states
                    .iter()
                    .find(|state| state.name != "Fault")
                    .map(|state| state.name.as_str())
            });
            let flow = actor_flow_descriptor(
                actor,
                syntax.runs_flow.as_deref(),
                root_state_name,
                program,
            )?;
            Ok(ResolvedActorDescriptor {
                schema: RESOLVED_ACTOR_SCHEMA,
                actor: actor_id.clone(),
                handle_type,
                fields,
                methods,
                runs_flow: syntax.runs_flow.clone(),
                flow,
            })
        };
        match build() {
            Ok(descriptor) => {
                descriptors.insert(actor_id, descriptor);
            }
            Err(message) => errors.push(Diagnostic::error(
                format!("TOOL-RESOLUTION-001: actor '{}': {message}", syntax.name),
                syntax.meta.span,
            )),
        }
    }
    if errors.is_empty() {
        Ok(descriptors)
    } else {
        Err(errors)
    }
}
