//! Checker-owned actor declarations for AST-free execution consumers.
//!
//! Declaration order, field initializers and method identities are fixed here,
//! before MIR or a backend chooses an execution profile. An unmaterialized
//! initializer is retained explicitly; it is never replaced with zero.

use std::collections::{BTreeMap, BTreeSet};

use super::{PrimitiveType, ResolvedLiteral, ResolvedSignature, ResolvedType, ResolvedTypeId};
use crate::core::{CheckedProgram, NodeId};

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
}

impl ResolvedScalarActorReceipt {
    pub fn descriptor(&self) -> &ResolvedActorDescriptor {
        &self.descriptor
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
        if self.runs_flow.is_some() {
            return Err(self.error("runs-Flow actors require a separate state transition contract"));
        }
        let is_i32 = |ty: &ResolvedTypeId| {
            matches!(
                program.resolved_types().get(ty),
                Some(ResolvedType::Primitive(PrimitiveType::I32))
            )
        };
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
            let function = program
                .functions()
                .get(&method.owner)
                .ok_or_else(|| self.error("method function is absent"))?;
            if function.is_async
                || function.is_comptime
                || function.extern_abi.is_some()
                || !method.signature.generic_parameters.is_empty()
                || !method.signature.effects.is_empty()
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
            if callable.owner != method.owner || callable.signature != method.signature {
                return Err(self.error("method callable and declaration signatures disagree"));
            }
        }
        Ok(ResolvedScalarActorReceipt {
            descriptor: self.clone(),
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
                });
            }
            Ok(ResolvedActorDescriptor {
                schema: RESOLVED_ACTOR_SCHEMA,
                actor: actor_id.clone(),
                handle_type,
                fields,
                methods,
                runs_flow: syntax.runs_flow.clone(),
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
