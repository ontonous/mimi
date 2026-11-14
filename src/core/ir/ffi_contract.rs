//! Checker-owned extern declaration facts for typed FFI contract consumers.
//!
//! The producer is frontend-only: surface declaration typing and CapMode are
//! captured once when CheckedProgram is assembled. Consumers use canonical
//! identities below. A complete declaration catalog is not a solver proof or
//! an execution/ABI effect receipt.
use std::collections::BTreeMap;

use super::{
    lower::ResolvedDeclarationExpressionKind, PrimitiveType, ResolvedBodyError, ResolvedLocalId,
    ResolvedParameterId, ResolvedType, ResolvedTypeId, TypedCallEffectAuthority,
    TypedCallExecutionDisposition, TypedMatchPathReceipt, TypedProofScopeFrame,
};
use crate::core::{CheckedProgram, EffectId, NodeId, ResolvedExternFunc};
use crate::span::Span;

pub const RESOLVED_FFI_DECLARATION_SCHEMA: &str = "resolved-ffi-declaration-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedFfiParameterMode {
    Value,
    Borrow,
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFfiParameter {
    pub id: ResolvedParameterId,
    pub ty: ResolvedTypeId,
    pub mode: ResolvedFfiParameterMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFfiSignature {
    pub parameters: Vec<ResolvedFfiParameter>,
    pub result: ResolvedTypeId,
}

#[derive(Debug, Clone)]
pub enum ResolvedFfiContractRoot {
    Absent,
    Ready {
        node_id: NodeId,
    },
    Deferred {
        node_id: Option<NodeId>,
        errors: Vec<ResolvedBodyError>,
    },
}

impl ResolvedFfiContractRoot {
    pub fn is_declared(&self) -> bool {
        !matches!(self, Self::Absent)
    }
}

/// Declaration metadata keeps block and declaration facts separate. Future
/// invocation receipts must bind actual errno capture, no-unwind protection,
/// default C promotions, and capability actions to the call node; preserving
/// these flags cannot manufacture any of those effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedFfiInvocationMetadata {
    pub variadic: bool,
    pub block_no_panic: bool,
    pub declaration_no_panic: bool,
    pub block_returns_errno: bool,
    pub declaration_returns_errno: bool,
    pub unsafe_block: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedFfiDeclarationContract {
    pub schema: &'static str,
    pub block: NodeId,
    pub declaration: NodeId,
    pub symbol: String,
    pub abi: String,
    pub fixed_parameter_ids: Vec<ResolvedParameterId>,
    pub fixed_parameter_modes: Vec<ResolvedFfiParameterMode>,
    /// Canonicalization may be Deferred for a historically legal declaration.
    /// It must retain its declaration and contracts in the complete catalog.
    pub signature: Result<ResolvedFfiSignature, Vec<ResolvedBodyError>>,
    pub requires: ResolvedFfiContractRoot,
    pub ensures: ResolvedFfiContractRoot,
    pub invocation: ResolvedFfiInvocationMetadata,
    pub span: Span,
    // This private witness prevents cloned public metadata from authorizing a
    // changed ABI, contract root, parameter mode, or invocation effect. It is
    // bound to the typed roots in this exact CheckedProgram and is not a proof.
    checked_witness: Option<ResolvedFfiDeclarationWitness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedFfiDeclarationWitness {
    fingerprint: blake3::Hash,
}

impl ResolvedFfiDeclarationWitness {
    fn from_checked_declaration(
        declaration: &ResolvedFfiDeclarationContract,
        program: &CheckedProgram,
    ) -> Self {
        // The descriptor fields are public for read-only consumer access and
        // are routinely cloned into planning data. Hash a clone with the
        // witness cleared so the value is stable and non-recursive.
        let mut descriptor = declaration.clone();
        descriptor.checked_witness = None;
        let typed_roots = program
            .declaration_expression_bodies()
            .values()
            .filter(|root| root.owner == declaration.declaration)
            .collect::<Vec<_>>();
        let material = format!("descriptor={descriptor:#?}\nroots={typed_roots:#?}");
        Self {
            fingerprint: blake3::hash(material.as_bytes()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedFfiPendingEffect {
    VariadicAbiAndPromotions,
    ErrnoCapture,
    NoUnwindProtection,
    ParameterBorrow(ResolvedParameterId),
    ParameterMove(ResolvedParameterId),
}

#[derive(Debug)]
pub struct ResolvedFfiInputBinding<'a> {
    pub parameter: &'a ResolvedFfiParameter,
    pub argument: &'a super::ResolvedArgument,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedFfiDefaultPromotionKind {
    Identity,
    IntegerPromotionToI32,
    FloatPromotionToF64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedFfiPromotionState {
    Ready {
        target_type: ResolvedTypeId,
        kind: ResolvedFfiDefaultPromotionKind,
    },
    Deferred {
        reason: String,
    },
}

/// Checker-owned source receipt for one C variadic tail operand. Ordinals are
/// zero-based positions in the full call argument list, including fixed args.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFfiVariadicArgument {
    pub call_node: NodeId,
    pub ordinal: usize,
    /// Resolved source expression identity. MIR consumers encode its value as
    /// `expr:<source_node>`; the node itself remains unprefixed here.
    pub source_node: NodeId,
    pub synthetic_parameter: ResolvedParameterId,
    pub source_type: ResolvedTypeId,
    /// Exact checker conversion attached to the original source operand.
    /// C's default promotion is a separate physical ABI conversion and must
    /// not overwrite this source-level fact.
    pub conversion: super::CheckedConversion,
    /// Checker-authenticated source nodes evaluated eagerly for this exact
    /// argument. Latent closure/compile-time bodies are excluded.
    pub eager_source_nodes: Vec<NodeId>,
    pub promotion: ResolvedFfiPromotionState,
}

/// Input predicates and postconditions retain separate readiness. A Ready
/// Requires root cannot erase a Deferred Ensures root or invocation effect.
#[derive(Debug)]
pub struct ResolvedFfiTypedCallContext<'a> {
    pub declaration: &'a ResolvedFfiDeclarationContract,
    pub call_node: &'a NodeId,
    /// Canonical call shape copied from the unique Resolved Call node. MIR
    /// materialization and the independent proof-source census compare these
    /// facts rather than treating a NodeId as a complete invocation receipt.
    pub expression_type: &'a ResolvedTypeId,
    pub type_arguments: &'a [ResolvedTypeId],
    pub effects: &'a [EffectId],
    pub selected_signature: String,
    /// Exact checker call-event location and path. A NodeId alone does not
    /// establish whether this call is in the body timeline, a guard, a loop,
    /// or an unsupported/deferred expression context.
    pub source_statement: NodeId,
    pub source_order: u64,
    pub source_scope: Vec<TypedProofScopeFrame>,
    pub source_guards: Vec<(NodeId, bool)>,
    pub source_match_path: Vec<TypedMatchPathReceipt>,
    pub source_disposition: TypedCallExecutionDisposition,
    /// Extern calls have no checker-authenticated no-write receipt; this
    /// remains UnknownMayReadWrite even if `effects` is empty.
    pub source_effect_authority: TypedCallEffectAuthority,
    pub inputs: Vec<ResolvedFfiInputBinding<'a>>,
    pub requires: Result<Option<&'a super::ResolvedBody>, Vec<ResolvedBodyError>>,
    pub ensures: Result<Option<&'a super::ResolvedBody>, Vec<ResolvedBodyError>>,
    /// Stable declaration postcondition placeholder. A verifier must bind this
    /// to a fresh foreign result and separately discharge its own obligations.
    pub contract_result: ResolvedLocalId,
    /// Fresh result identity is scoped to this exact syntactic foreign call.
    /// Distinct calls to the same declaration cannot share a solver result.
    pub foreign_result: &'a NodeId,
    /// Checker-selected call result may include an explicit effect conversion;
    /// it is not silently equated with the declaration's physical result ABI.
    pub call_result_type: &'a ResolvedTypeId,
    pub pending_effects: Vec<ResolvedFfiPendingEffect>,
    variadic_tail: Vec<ResolvedFfiVariadicArgument>,
}

impl ResolvedFfiTypedCallContext<'_> {
    pub fn variadic_tail(&self) -> &[ResolvedFfiVariadicArgument] {
        &self.variadic_tail
    }
}

fn expressions_at_node<'a>(
    body: &'a super::ResolvedBody,
    node_id: &NodeId,
) -> Vec<&'a super::ResolvedExpr> {
    let mut matches = Vec::new();
    visit_block_for_expression(&body.root, node_id, &mut matches);
    for expression in body
        .place_inputs
        .values()
        .chain(body.default_values.values())
    {
        visit_expression(expression, node_id, &mut matches);
    }
    matches
}

fn resolved_call_expression_matches(
    left: &super::ResolvedExpr,
    right: &super::ResolvedExpr,
) -> bool {
    if left.node_id != right.node_id || left.ty != right.ty {
        return false;
    }
    let (super::ResolvedExprKind::Call(left), super::ResolvedExprKind::Call(right)) =
        (&left.kind, &right.kind)
    else {
        return false;
    };
    left.callee == right.callee
        && left.result == right.result
        && left.type_arguments == right.type_arguments
        && left.effects == right.effects
        && left.arguments.len() == right.arguments.len()
        && left
            .arguments
            .iter()
            .zip(&right.arguments)
            .all(|(left, right)| {
                left.parameter == right.parameter
                    && left.value.node_id == right.value.node_id
                    && left.value.ty == right.value.ty
                    && left.conversion == right.conversion
            })
}

fn visit_block_for_expression<'a>(
    block: &'a super::ResolvedBlock,
    node_id: &NodeId,
    matches: &mut Vec<&'a super::ResolvedExpr>,
) {
    for statement in &block.statements {
        match &statement.kind {
            super::ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(expression) = initializer {
                    visit_expression(expression, node_id, matches);
                }
            }
            super::ResolvedStmtKind::Assign { value, .. }
            | super::ResolvedStmtKind::Expr(value) => {
                visit_expression(value, node_id, matches);
            }
            super::ResolvedStmtKind::Return { value, .. }
            | super::ResolvedStmtKind::Break(value) => {
                if let Some(expression) = value {
                    visit_expression(expression, node_id, matches);
                }
            }
            super::ResolvedStmtKind::Continue
            | super::ResolvedStmtKind::Drop(_)
            | super::ResolvedStmtKind::NestedCallable(_) => {}
            super::ResolvedStmtKind::While { condition, body } => {
                visit_expression(condition, node_id, matches);
                visit_block_for_expression(body, node_id, matches);
            }
            super::ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | super::ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                visit_expression(initializer, node_id, matches);
                visit_block_for_expression(body, node_id, matches);
            }
            super::ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                visit_expression(initializer, node_id, matches);
                visit_block_for_expression(then_block, node_id, matches);
                if let Some(else_block) = else_block {
                    visit_block_for_expression(else_block, node_id, matches);
                }
            }
            super::ResolvedStmtKind::Loop(body) | super::ResolvedStmtKind::Scope { body, .. } => {
                visit_block_for_expression(body, node_id, matches);
            }
            super::ResolvedStmtKind::Contract { condition, .. } => {
                visit_expression(condition, node_id, matches);
            }
            super::ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    visit_expression(expression, node_id, matches);
                }
            }
            super::ResolvedStmtKind::Pinned { value, body, .. } => {
                visit_expression(value, node_id, matches);
                visit_block_for_expression(body, node_id, matches);
            }
        }
    }
    if let Some(result) = &block.result {
        visit_expression(result, node_id, matches);
    }
}

fn visit_expression<'a>(
    expression: &'a super::ResolvedExpr,
    node_id: &NodeId,
    matches: &mut Vec<&'a super::ResolvedExpr>,
) {
    if &expression.node_id == node_id {
        matches.push(expression);
    }
    use super::ResolvedExprKind as K;
    match &expression.kind {
        K::FString(parts) => {
            for part in parts {
                if let super::ResolvedFStringPart::Interpolation(expression) = part {
                    visit_expression(expression, node_id, matches);
                }
            }
        }
        K::Project { value, projection } => {
            visit_expression(value, node_id, matches);
            if let super::ResolvedValueProjection::Index(index) = projection {
                visit_expression(index, node_id, matches);
            }
        }
        K::Binary { left, right, .. } => {
            visit_expression(left, node_id, matches);
            visit_expression(right, node_id, matches);
        }
        K::Unary { operand, .. } | K::Spawn(operand) | K::Await(operand) => {
            visit_expression(operand, node_id, matches);
        }
        K::Call(call) => {
            for argument in &call.arguments {
                visit_expression(&argument.value, node_id, matches);
            }
        }
        K::Tuple(values) | K::List(values) | K::Set(values) => {
            for value in values {
                visit_expression(value, node_id, matches);
            }
        }
        K::Map(entries) => {
            for (key, value) in entries {
                visit_expression(key, node_id, matches);
                visit_expression(value, node_id, matches);
            }
        }
        K::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            visit_expression(value, node_id, matches);
            visit_expression(iterable, node_id, matches);
            if let Some(guard) = guard {
                visit_expression(guard, node_id, matches);
            }
        }
        K::OptionalChain { receiver, .. }
        | K::TypeOf(receiver)
        | K::Old(receiver)
        | K::Try {
            value: receiver, ..
        }
        | K::Cast {
            value: receiver, ..
        } => visit_expression(receiver, node_id, matches),
        K::Record { fields, rest, .. } => {
            for field in fields {
                visit_expression(&field.value, node_id, matches);
            }
            if let Some(rest) = rest {
                visit_expression(rest, node_id, matches);
            }
        }
        K::Block(block) | K::Comptime(block) | K::Quote(block) => {
            visit_block_for_expression(block, node_id, matches);
        }
        K::Scope { body, .. } => visit_block_for_expression(body, node_id, matches),
        K::If {
            condition,
            then_block,
            else_block,
        } => {
            visit_expression(condition, node_id, matches);
            visit_block_for_expression(then_block, node_id, matches);
            visit_block_for_expression(else_block, node_id, matches);
        }
        K::Match { scrutinee, arms } => {
            visit_expression(scrutinee, node_id, matches);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_expression(guard, node_id, matches);
                }
                visit_expression(&arm.body, node_id, matches);
            }
        }
        K::Range { start, end } => {
            visit_expression(start, node_id, matches);
            visit_expression(end, node_id, matches);
        }
        K::Slice { target, start, end } => {
            visit_expression(target, node_id, matches);
            if let Some(start) = start {
                visit_expression(start, node_id, matches);
            }
            if let Some(end) = end {
                visit_expression(end, node_id, matches);
            }
        }
        K::Literal(_)
        | K::Load(_)
        | K::Constant(_)
        | K::Callable(_)
        | K::DefaultArgument { .. }
        | K::ComptimeValue(_)
        | K::TypeValue(_) => {}
        K::Lambda(lambda) => visit_block_for_expression(&lambda.body, node_id, matches),
    }
}

impl ResolvedFfiDeclarationContract {
    fn anchor_checked_witness(&mut self, program: &CheckedProgram) {
        self.checked_witness = Some(ResolvedFfiDeclarationWitness::from_checked_declaration(
            self, program,
        ));
    }

    pub fn has_declared_contract(&self) -> bool {
        self.requires.is_declared() || self.ensures.is_declared()
    }

    pub fn pending_effects(&self) -> Vec<ResolvedFfiPendingEffect> {
        let mut effects = Vec::new();
        if self.invocation.variadic {
            effects.push(ResolvedFfiPendingEffect::VariadicAbiAndPromotions);
        }
        if self.invocation.block_returns_errno || self.invocation.declaration_returns_errno {
            effects.push(ResolvedFfiPendingEffect::ErrnoCapture);
        }
        if self.invocation.block_no_panic || self.invocation.declaration_no_panic {
            effects.push(ResolvedFfiPendingEffect::NoUnwindProtection);
        }
        for (parameter, mode) in self
            .fixed_parameter_ids
            .iter()
            .zip(&self.fixed_parameter_modes)
        {
            match mode {
                ResolvedFfiParameterMode::Value => {}
                ResolvedFfiParameterMode::Borrow => {
                    effects.push(ResolvedFfiPendingEffect::ParameterBorrow(parameter.clone()))
                }
                ResolvedFfiParameterMode::Move => {
                    effects.push(ResolvedFfiPendingEffect::ParameterMove(parameter.clone()))
                }
            }
        }
        effects
    }

    pub fn call_context<'a>(
        &'a self,
        program: &'a CheckedProgram,
        body: &'a super::ResolvedBody,
        expression: &'a super::ResolvedExpr,
    ) -> Result<ResolvedFfiTypedCallContext<'a>, Vec<ResolvedBodyError>> {
        self.validate(program)?;
        let fail = |message| {
            vec![ResolvedBodyError::new(
                expression.node_id.clone(),
                format!("TOOL-RESOLUTION-001: {message}"),
            )]
        };
        let super::ResolvedExprKind::Call(call) = &expression.kind else {
            return Err(fail("typed FFI context requires a call expression"));
        };
        if !matches!(&call.callee, super::ResolvedCallee::Extern(declaration) if declaration == &self.declaration)
        {
            return Err(fail(
                "typed FFI context call/declaration identities disagree",
            ));
        }
        let signature = self.signature.as_ref().map_err(Clone::clone)?;
        let directory = program
            .call_sites()
            .get(&expression.node_id)
            .ok_or_else(|| fail("typed FFI context has no checked call-site identity"))?;
        if !program
            .resolved_bodies()
            .get(&body.owner)
            .is_some_and(|checked| std::ptr::eq(checked, body))
        {
            return Err(fail(
                "typed FFI context requires the canonical CheckedProgram caller body",
            ));
        }
        if directory.node_id != expression.node_id
            || directory.owner != body.owner.0
            || directory.callee != self.symbol
            || directory.kind != crate::core::ResolvedCallKind::Extern
            || directory.argc != call.arguments.len()
            || directory.expected_argc != Some(signature.parameters.len())
            || call.result != expression.ty
            || program.resolved_node_type(&expression.node_id) != Some(&expression.ty)
        {
            return Err(fail(
                "typed FFI context owner/symbol/kind/arity disagrees with checked call directory",
            ));
        }
        if call.arguments.len() < signature.parameters.len() {
            return Err(fail(
                "typed FFI call omits one or more fixed declaration arguments",
            ));
        }
        if !self.invocation.variadic && call.arguments.len() != signature.parameters.len() {
            return Err(fail(
                "non-variadic typed FFI call has arguments beyond its fixed declaration arity",
            ));
        }
        let mut inputs = Vec::new();
        for (parameter, argument) in signature.parameters.iter().zip(&call.arguments) {
            if argument.parameter != parameter.id
                || argument.conversion.from != argument.value.ty
                || argument.conversion.to != argument.value.ty
                || argument.conversion.kind != super::CheckedConversionKind::Identity
                || program.resolved_node_type(&argument.value.node_id) != Some(&argument.value.ty)
                || program.resolved_types().get(&argument.value.ty).is_none()
            {
                return Err(fail(
                    "typed FFI input parameter/conversion identities disagree",
                ));
            }
            inputs.push(ResolvedFfiInputBinding {
                parameter,
                argument,
            });
        }
        let mut variadic_tail = Vec::new();
        if self.invocation.variadic {
            for (ordinal, argument) in call
                .arguments
                .iter()
                .enumerate()
                .skip(signature.parameters.len())
            {
                let synthetic_parameter = ResolvedParameterId(NodeId(format!(
                    "{}/variadic:{ordinal}",
                    self.declaration.0
                )));
                if argument.parameter != synthetic_parameter
                    || argument.conversion.from != argument.value.ty
                    || argument.conversion.to != argument.value.ty
                    || argument.conversion.kind != super::CheckedConversionKind::Identity
                    || program.resolved_node_type(&argument.value.node_id)
                        != Some(&argument.value.ty)
                    || program.resolved_types().get(&argument.value.ty).is_none()
                {
                    return Err(fail(
                        "typed FFI variadic argument identity/conversion/type disagrees with the checked source",
                    ));
                }
                variadic_tail.push(ResolvedFfiVariadicArgument {
                    call_node: expression.node_id.clone(),
                    ordinal,
                    source_node: argument.value.node_id.clone(),
                    synthetic_parameter,
                    source_type: argument.value.ty.clone(),
                    conversion: argument.conversion.clone(),
                    eager_source_nodes: argument
                        .value
                        .eager_source_nodes_with_place_inputs(&body.place_inputs),
                    promotion: c_default_argument_promotion(program, &argument.value.ty),
                });
            }
        }
        validate_variadic_tail_receipts(
            program,
            &expression.node_id,
            &self.declaration,
            call,
            &body.place_inputs,
            signature,
            self.invocation.variadic,
            &variadic_tail,
        )?;
        let declaration_identity = format!(
            "symbol={:?}; abi={:?}; signature={:#?}; invocation={:?}",
            self.symbol, self.abi, self.signature, self.invocation
        );
        let call_shape = (
            &expression.ty,
            &call.type_arguments,
            &call.result,
            &call.effects,
            &call.arguments,
        );
        let selected_signature = format!(
            "callee={:?}; declaration={}; call_shape={:?}",
            call.callee,
            declaration_identity,
            Some(call_shape),
        );
        let callable = program
            .callable(&body.owner)
            .ok_or_else(|| fail("typed FFI caller has no checker-owned callable proof timeline"))?;
        let worklist = callable.proof_point_worklist();
        if !worklist.validate_in_checked_program(program) {
            return Err(fail(
                "typed FFI caller proof timeline is not sealed to this CheckedProgram",
            ));
        }
        let matching_events = worklist
            .calls
            .iter()
            .chain(&worklist.deferred_calls)
            .filter(|point| point.call_node == expression.node_id && point.body_owner == body.owner)
            .collect::<Vec<_>>();
        let [call_event] = matching_events.as_slice() else {
            return Err(fail(
                "typed FFI call has no unique checker-ordered body call event",
            ));
        };
        if !call_event.validate_in_checked_program(program)
            || call_event.owner != body.owner
            || !resolved_call_expression_matches(&call_event.expression, expression)
            || call_event.effect.call_node != expression.node_id
            || call_event.effect.callee != call.callee
            || call_event.effect.declared_effects != call.effects
        {
            return Err(fail(
                "typed FFI call event disagrees with the exact Resolved Call expression",
            ));
        }
        let source_guards = call_event
            .guards
            .iter()
            .map(|guard| (guard.condition.node_id.clone(), guard.when))
            .collect();
        Ok(ResolvedFfiTypedCallContext {
            declaration: self,
            call_node: &expression.node_id,
            expression_type: &expression.ty,
            type_arguments: &call.type_arguments,
            effects: &call.effects,
            selected_signature,
            source_statement: call_event.statement.clone(),
            source_order: call_event.order,
            source_scope: call_event.scope.clone(),
            source_guards,
            source_match_path: call_event.match_path.clone(),
            source_disposition: call_event.disposition.clone(),
            source_effect_authority: call_event.effect.authority,
            inputs,
            requires: typed_contract_body(program, &self.requires),
            ensures: typed_contract_body(program, &self.ensures),
            contract_result: ResolvedLocalId(NodeId(format!(
                "{}/contract-result",
                self.declaration.0
            ))),
            foreign_result: &expression.node_id,
            call_result_type: &call.result,
            pending_effects: self.pending_effects(),
            variadic_tail,
        })
    }

    /// Resolve a typed call directly from a canonical caller body so MIR and
    /// verifier consumers do not repeat source-tree expression discovery.
    /// The target NodeId must identify exactly one expression and that
    /// expression must be a call; `call_context` then enforces the checked
    /// caller body pointer, declaration witness, and call-site directory facts.
    pub fn call_context_at<'a>(
        &'a self,
        program: &'a CheckedProgram,
        body: &'a super::ResolvedBody,
        call_node: &NodeId,
    ) -> Result<ResolvedFfiTypedCallContext<'a>, Vec<ResolvedBodyError>> {
        let matches = expressions_at_node(body, call_node);
        let [expression] = matches.as_slice() else {
            return Err(vec![ResolvedBodyError::new(
                call_node.clone(),
                format!(
                    "TOOL-RESOLUTION-001: typed FFI call lookup found {} expressions for the requested NodeId; exactly one is required",
                    matches.len()
                ),
            )]);
        };
        if !matches!(&expression.kind, super::ResolvedExprKind::Call(_)) {
            return Err(vec![ResolvedBodyError::new(
                call_node.clone(),
                "TOOL-RESOLUTION-001: typed FFI call lookup target is not a call expression",
            )]);
        }
        self.call_context(program, body, expression)
    }

    /// Validate typed root/signature identity without reading extern source
    /// declarations. The private witness was minted by the checker while it
    /// still owned those declarations.
    /// Deferred materialization remains explicit data, never NoContracts.
    pub fn validate(&self, program: &CheckedProgram) -> Result<(), Vec<ResolvedBodyError>> {
        let fail = |node: &NodeId, message: &str| {
            vec![ResolvedBodyError::new(
                node.clone(),
                format!("TOOL-RESOLUTION-001: {message}"),
            )]
        };
        let witness = ResolvedFfiDeclarationWitness::from_checked_declaration(self, program);
        if self.checked_witness.as_ref() != Some(&witness) {
            return Err(fail(
                &self.declaration,
                "typed FFI declaration or its checker-owned roots disagree with the private checker witness",
            ));
        }
        if self.schema != RESOLVED_FFI_DECLARATION_SCHEMA {
            return Err(fail(
                &self.declaration,
                "unsupported typed FFI declaration schema",
            ));
        }
        // The private witness is minted only by the checker while it still
        // owns the source declaration. Consumers validate the canonical
        // descriptor and its exact typed roots below; they must not reopen the
        // legacy extern-block/signature catalog to reconstruct declaration
        // metadata or contract ASTs.
        if self.fixed_parameter_ids.len() != self.fixed_parameter_modes.len() {
            return Err(fail(
                &self.declaration,
                "extern fixed parameter modes lack exact identity coverage",
            ));
        }
        let signature = match &self.signature {
            Ok(signature) => Some(signature),
            Err(errors) if !errors.is_empty() => None,
            Err(_) => {
                return Err(fail(
                    &self.declaration,
                    "Deferred extern signature has no reason",
                ));
            }
        };
        if let Some(signature) = signature {
            if signature.parameters.len() != self.fixed_parameter_ids.len()
                || signature
                    .parameters
                    .iter()
                    .zip(&self.fixed_parameter_ids)
                    .any(|(parameter, expected)| &parameter.id != expected)
                || signature
                    .parameters
                    .iter()
                    .zip(&self.fixed_parameter_modes)
                    .any(|(parameter, expected)| &parameter.mode != expected)
                || program.resolved_types().get(&signature.result).is_none()
                || signature
                    .parameters
                    .iter()
                    .any(|parameter| program.resolved_types().get(&parameter.ty).is_none())
            {
                return Err(fail(
                    &self.declaration,
                    "extern canonical signature identities/types are inconsistent",
                ));
            }
        }
        for (kind, contract) in [
            (
                ResolvedDeclarationExpressionKind::ExternRequires,
                &self.requires,
            ),
            (
                ResolvedDeclarationExpressionKind::ExternEnsures,
                &self.ensures,
            ),
        ] {
            let node = match contract {
                ResolvedFfiContractRoot::Absent => continue,
                ResolvedFfiContractRoot::Deferred { errors, .. } if !errors.is_empty() => continue,
                ResolvedFfiContractRoot::Deferred { .. } => {
                    return Err(fail(
                        &self.declaration,
                        "Deferred extern contract has no reason",
                    ));
                }
                ResolvedFfiContractRoot::Ready { node_id } => node_id,
            };
            let root = program
                .declaration_expression_bodies()
                .get(node)
                .ok_or_else(|| fail(node, "typed extern contract root is absent"))?;
            let body = root
                .body
                .as_ref()
                .map_err(|_| fail(node, "Ready extern contract points to a Deferred body"))?;
            if &root.node_id != node
                || root.owner != self.declaration
                || body.owner != self.declaration
                || root.kind != kind
                || body.root.result.as_ref().map(|result| &result.node_id) != Some(node)
                || !matches!(
                    body.root
                        .result
                        .as_ref()
                        .and_then(|result| program.resolved_types().get(&result.ty)),
                    Some(ResolvedType::Primitive(PrimitiveType::Bool))
                )
            {
                return Err(fail(
                    node,
                    "extern contract owner/kind/root/Boolean type is inconsistent",
                ));
            }
            if body.parameters.len() != self.fixed_parameter_ids.len()
                || body
                    .parameters
                    .iter()
                    .zip(&self.fixed_parameter_ids)
                    .any(|(local, parameter)| local.0 .0 != format!("{}/local", parameter.0 .0))
            {
                return Err(fail(
                    node,
                    "extern contract parameter identity coverage is inconsistent",
                ));
            }
            if let Some(signature) = signature {
                if body.parameters.len() != signature.parameters.len() {
                    return Err(fail(
                        node,
                        "extern contract does not cover the fixed parameter signature",
                    ));
                }
                for (local, parameter) in body.parameters.iter().zip(&signature.parameters) {
                    if local.0 .0 != format!("{}/local", parameter.id.0 .0)
                        || body.locals.get(local).map(|local| &local.ty) != Some(&parameter.ty)
                    {
                        return Err(fail(
                            node,
                            "extern contract parameter identity/type disagrees with its declaration",
                        ));
                    }
                }
                if kind == ResolvedDeclarationExpressionKind::ExternEnsures {
                    let result =
                        ResolvedLocalId(NodeId(format!("{}/contract-result", self.declaration.0)));
                    if body.locals.get(&result).map(|local| &local.ty) != Some(&signature.result) {
                        return Err(fail(
                            node,
                            "extern Ensures result identity/type disagrees with its declaration",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

fn c_default_argument_promotion(
    program: &CheckedProgram,
    source_type: &ResolvedTypeId,
) -> ResolvedFfiPromotionState {
    let Some(ResolvedType::Primitive(primitive)) = program.resolved_types().get(source_type) else {
        return ResolvedFfiPromotionState::Deferred {
            reason: format!(
                "no canonical C default-promotion rule for resolved type {}",
                source_type.as_str()
            ),
        };
    };
    let (target_type, kind) = match primitive {
        PrimitiveType::Bool
        | PrimitiveType::I8
        | PrimitiveType::U8
        | PrimitiveType::I16
        | PrimitiveType::U16 => (
            super::primitive_type_id(PrimitiveType::I32),
            ResolvedFfiDefaultPromotionKind::IntegerPromotionToI32,
        ),
        PrimitiveType::F32 => (
            super::primitive_type_id(PrimitiveType::F64),
            ResolvedFfiDefaultPromotionKind::FloatPromotionToF64,
        ),
        PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::F64 => (
            source_type.clone(),
            ResolvedFfiDefaultPromotionKind::Identity,
        ),
        _ => {
            return ResolvedFfiPromotionState::Deferred {
                reason: format!(
                    "no canonical C default-promotion rule for primitive {}",
                    primitive.language_name()
                ),
            };
        }
    };
    // Primitive IDs are structural and deterministic. A promotion target need
    // not have appeared in source (e.g. an otherwise-f64-free function with an
    // f32 tail), so do not degrade a known rule merely because the source type
    // table did not need to intern that target.
    ResolvedFfiPromotionState::Ready { target_type, kind }
}

fn validate_variadic_tail_receipts(
    program: &CheckedProgram,
    call_node: &NodeId,
    declaration: &NodeId,
    call: &super::ResolvedCall,
    place_inputs: &BTreeMap<NodeId, super::ResolvedExpr>,
    signature: &ResolvedFfiSignature,
    variadic: bool,
    receipts: &[ResolvedFfiVariadicArgument],
) -> Result<(), Vec<ResolvedBodyError>> {
    let fail = |message: &str| {
        vec![ResolvedBodyError::new(
            call_node.clone(),
            format!("TOOL-RESOLUTION-001: {message}"),
        )]
    };
    let fixed_count = signature.parameters.len();
    if call.arguments.len() < fixed_count {
        return Err(fail("variadic call omits a fixed declaration argument"));
    }
    let tail_count = call.arguments.len() - fixed_count;
    if !variadic && tail_count != 0 {
        return Err(fail("non-variadic declaration has a variadic-tail receipt"));
    }
    let expected_receipt_count = if variadic { tail_count } else { 0 };
    if receipts.len() != expected_receipt_count {
        return Err(fail(
            "variadic tail receipt count does not cover the checked call arguments",
        ));
    }
    for (offset, (receipt, argument)) in receipts
        .iter()
        .zip(call.arguments.iter().skip(fixed_count))
        .enumerate()
    {
        let ordinal = fixed_count + offset;
        let expected_parameter =
            ResolvedParameterId(NodeId(format!("{}/variadic:{ordinal}", declaration.0)));
        if receipt.call_node != *call_node
            || receipt.ordinal != ordinal
            || receipt.source_node != argument.value.node_id
            || receipt.synthetic_parameter != expected_parameter
            || receipt.source_type != argument.value.ty
            || receipt.conversion != argument.conversion
            || receipt.eager_source_nodes
                != argument
                    .value
                    .eager_source_nodes_with_place_inputs(place_inputs)
            || argument.parameter != expected_parameter
            || argument.conversion.kind != super::CheckedConversionKind::Identity
            || argument.conversion.from != argument.value.ty
            || argument.conversion.to != argument.value.ty
        {
            return Err(fail(
                "variadic tail receipt call/ordinal/argument/parameter/type identity is inconsistent",
            ));
        }
        if program.resolved_node_type(&receipt.source_node) != Some(&receipt.source_type)
            || program.resolved_types().get(&receipt.source_type).is_none()
        {
            return Err(fail(
                "variadic tail receipt source type disagrees with the checker-owned argument node type",
            ));
        }
        if receipt.promotion != c_default_argument_promotion(program, &receipt.source_type) {
            return Err(fail(
                "variadic tail promotion does not match the canonical C default-promotion rule",
            ));
        }
    }
    Ok(())
}

fn typed_contract_body<'a>(
    program: &'a CheckedProgram,
    state: &ResolvedFfiContractRoot,
) -> Result<Option<&'a super::ResolvedBody>, Vec<ResolvedBodyError>> {
    match state {
        ResolvedFfiContractRoot::Absent => Ok(None),
        ResolvedFfiContractRoot::Deferred { errors, .. } => Err(errors.clone()),
        ResolvedFfiContractRoot::Ready { node_id } => {
            let root = program
                .declaration_expression_bodies()
                .get(node_id)
                .ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        "TOOL-RESOLUTION-001: typed FFI contract root is absent",
                    )]
                })?;
            root.body.as_ref().map(Some).map_err(Clone::clone)
        }
    }
}

/// Frontend-only producer. Visit all declarations, including uncalled externs;
/// no MIR reachability filter participates in the declaration obligation census.
pub(crate) fn build_checked_ffi_declarations(
    program: &CheckedProgram,
) -> Result<BTreeMap<NodeId, ResolvedFfiDeclarationContract>, Vec<crate::diagnostic::Diagnostic>> {
    let mut declarations = BTreeMap::new();
    for block in program.extern_blocks_sorted() {
        for declaration in CheckedProgram::extern_signatures_sorted(block) {
            let signature = canonical_signature(program, declaration);
            let mut descriptor = ResolvedFfiDeclarationContract {
                schema: RESOLVED_FFI_DECLARATION_SCHEMA,
                block: block.node_id.clone(),
                declaration: declaration.node_id.clone(),
                symbol: declaration.name.clone(),
                abi: block.abi.clone(),
                fixed_parameter_ids: declaration
                    .parameter_ids
                    .iter()
                    .cloned()
                    .map(ResolvedParameterId)
                    .collect(),
                fixed_parameter_modes: declaration
                    .typed_params
                    .iter()
                    .map(|(_, _, mode)| parameter_mode(*mode))
                    .collect(),
                signature,
                requires: contract_root(
                    program,
                    declaration,
                    ResolvedDeclarationExpressionKind::ExternRequires,
                    declaration.requires.is_some(),
                ),
                ensures: contract_root(
                    program,
                    declaration,
                    ResolvedDeclarationExpressionKind::ExternEnsures,
                    declaration.ensures.is_some(),
                ),
                invocation: ResolvedFfiInvocationMetadata {
                    variadic: declaration.variadic,
                    block_no_panic: block.no_panic,
                    // `no_panic` currently has block-only source syntax; the
                    // signature snapshot mirrors that effective block flag.
                    // Keep the declaration-level slot false instead of
                    // copying one source fact into both fields.
                    declaration_no_panic: false,
                    block_returns_errno: block.returns_errno,
                    declaration_returns_errno: declaration.declared_returns_errno,
                    unsafe_block: block.unsafe_,
                },
                span: declaration.span,
                checked_witness: None,
            };
            descriptor.anchor_checked_witness(program);
            descriptor.validate(program).map_err(|errors| {
                errors
                    .into_iter()
                    .map(|error| {
                        crate::diagnostic::Diagnostic::error(error.message, declaration.span)
                    })
                    .collect::<Vec<_>>()
            })?;
            if declarations
                .insert(declaration.node_id.clone(), descriptor)
                .is_some()
            {
                return Err(vec![crate::diagnostic::Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: duplicate typed FFI declaration '{}'",
                        declaration.node_id.0
                    ),
                    declaration.span,
                )]);
            }
        }
    }
    Ok(declarations)
}

fn parameter_mode(mode: Option<crate::ast::CapMode>) -> ResolvedFfiParameterMode {
    match mode {
        None => ResolvedFfiParameterMode::Value,
        Some(crate::ast::CapMode::Borrow) => ResolvedFfiParameterMode::Borrow,
        Some(crate::ast::CapMode::Move) => ResolvedFfiParameterMode::Move,
    }
}

fn canonical_signature(
    program: &CheckedProgram,
    declaration: &ResolvedExternFunc,
) -> Result<ResolvedFfiSignature, Vec<ResolvedBodyError>> {
    if declaration.parameter_ids.len() != declaration.typed_params.len() {
        return Err(vec![ResolvedBodyError::new(
            declaration.node_id.clone(),
            "TOOL-RESOLUTION-001: extern fixed parameter identity coverage is incomplete",
        )]);
    }
    let parameters = declaration
        .typed_params
        .iter()
        .zip(&declaration.parameter_ids)
        .map(|((_, ty, mode), id)| {
            Ok(ResolvedFfiParameter {
                id: ResolvedParameterId(id.clone()),
                ty: super::lower::declaration_canonical_type(program, ty, id)?,
                mode: parameter_mode(*mode),
            })
        })
        .collect::<Result<Vec<_>, Vec<ResolvedBodyError>>>()?;
    let result = declaration
        .ret_type
        .as_ref()
        .map(|ty| super::lower::declaration_canonical_type(program, ty, &declaration.node_id))
        .transpose()?
        .unwrap_or_else(|| super::primitive_type_id(PrimitiveType::Unit));
    if program.resolved_types().get(&result).is_none() {
        return Err(vec![ResolvedBodyError::new(
            declaration.node_id.clone(),
            "TOOL-RESOLUTION-001: extern result is absent from the canonical type table",
        )]);
    }
    Ok(ResolvedFfiSignature { parameters, result })
}

fn contract_root(
    program: &CheckedProgram,
    declaration: &ResolvedExternFunc,
    kind: ResolvedDeclarationExpressionKind,
    declared: bool,
) -> ResolvedFfiContractRoot {
    if !declared {
        return ResolvedFfiContractRoot::Absent;
    }
    let roots = program
        .declaration_expression_bodies()
        .values()
        .filter(|root| root.owner == declaration.node_id && root.kind == kind)
        .collect::<Vec<_>>();
    let [root] = roots.as_slice() else {
        return ResolvedFfiContractRoot::Deferred {
            node_id: None,
            errors: vec![ResolvedBodyError::new(
                declaration.node_id.clone(),
                format!("TOOL-RESOLUTION-001: declared extern {kind:?} has no unique typed root"),
            )],
        };
    };
    match &root.body {
        Ok(_) => ResolvedFfiContractRoot::Ready {
            node_id: root.node_id.clone(),
        },
        Err(errors) => ResolvedFfiContractRoot::Deferred {
            node_id: Some(root.node_id.clone()),
            errors: errors.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new_memory(tokens, "ffi.descriptor", "test", source)
            .expect("source identity")
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file).expect("declaration fixture remains legal")
    }

    fn variadic_checked() -> CheckedProgram {
        checked(
            r#"
            extern "C" {
                func foreign(head: i32 ...) -> i32;
            }
            func main() -> i32 {
                let flag: bool = true
                let single: f32 = 1.0 as f32
                let word: i32 = 8
                let wide: i64 = 2
                let double: f64 = 3.0
                let text: string = "deferred"
                foreign(7, flag, single, word, wide, double, text)
            }
        "#,
        )
    }

    fn variadic_context_parts(
        program: &CheckedProgram,
    ) -> (
        &ResolvedFfiDeclarationContract,
        &super::super::ResolvedBody,
        &super::super::ResolvedExpr,
    ) {
        let declaration = program
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == "foreign")
            .unwrap();
        let body = program
            .resolved_bodies()
            .get(&NodeId("function:main".into()))
            .unwrap();
        let expression = body.root.result.as_deref().unwrap();
        (declaration, body, expression)
    }

    #[test]
    fn typed_ffi_declaration_descriptor_census_keeps_unused_contracts() {
        let program = checked(
            r#"
            extern "C" {
                func unused(value: i32) -> i32 requires: value >= 0 ensures: result >= value;
                func plain(value: i32) -> i32;
            }
            func main() -> i32 { 0 }
        "#,
        );
        assert_eq!(program.ffi_declarations().len(), 2);
        let unused = program
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == "unused")
            .unwrap();
        assert!(unused.has_declared_contract());
        assert!(unused.requires.is_declared());
        assert!(unused.ensures.is_declared());
        assert!(!program
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == "plain")
            .unwrap()
            .has_declared_contract());
        for declaration in program.ffi_declarations().values() {
            declaration
                .validate(&program)
                .expect("typed declaration root identity");
        }
    }

    #[test]
    fn typed_ffi_declaration_descriptor_keeps_invocation_facts_without_effect_proof() {
        for (declaration, variadic, borrow) in [
            (
                "func foreign(value: i64 ...) -> i64 requires: value >= 0;",
                true,
                false,
            ),
            (
                "func foreign(&value: i64) -> i64 requires: value >= 0;",
                false,
                true,
            ),
        ] {
            let source = format!(
                r#"#[errno] #[no_panic] extern "C" {{ {declaration} }}
func main() -> i32 {{ 0 }}"#
            );
            let program = checked(&source);
            let descriptor = program.ffi_declarations().values().next().unwrap();
            assert_eq!(descriptor.abi, "C");
            assert_eq!(descriptor.invocation.variadic, variadic);
            assert!(descriptor.invocation.block_returns_errno);
            assert!(descriptor.invocation.block_no_panic);
            assert!(!descriptor.invocation.declaration_returns_errno);
            assert!(!descriptor.invocation.declaration_no_panic);
            assert!(descriptor.has_declared_contract());
            assert_eq!(
                descriptor.fixed_parameter_modes[0],
                if borrow {
                    ResolvedFfiParameterMode::Borrow
                } else {
                    ResolvedFfiParameterMode::Value
                }
            );
        }
    }

    #[test]
    fn typed_ffi_descriptor_separates_block_defaults_from_declaration_errno() {
        let program = checked(
            r#"
            #[errno] #[no_panic] extern "C" {
                func inherited(value: i32) -> i32;
                #[errno] func explicit(value: i32) -> i32;
            }
            func main() -> i32 { 0 }
        "#,
        );
        let inherited = program
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == "inherited")
            .unwrap();
        let explicit = program
            .ffi_declarations()
            .values()
            .find(|declaration| declaration.symbol == "explicit")
            .unwrap();

        assert!(inherited.invocation.block_returns_errno);
        assert!(!inherited.invocation.declaration_returns_errno);
        assert!(inherited.invocation.block_no_panic);
        assert!(!inherited.invocation.declaration_no_panic);
        assert!(inherited
            .pending_effects()
            .contains(&ResolvedFfiPendingEffect::ErrnoCapture));
        assert!(inherited
            .pending_effects()
            .contains(&ResolvedFfiPendingEffect::NoUnwindProtection));

        assert!(explicit.invocation.block_returns_errno);
        assert!(explicit.invocation.declaration_returns_errno);
        assert!(explicit.invocation.block_no_panic);
        assert!(!explicit.invocation.declaration_no_panic);
        explicit
            .validate(&program)
            .expect("explicit declaration attribute is preserved separately");
    }

    #[test]
    fn typed_ffi_declaration_descriptor_retains_failed_optional_predicate_materialization() {
        let program = checked(
            r#"
            extern "C" { func foreign(s: string) -> i32 requires: unsupported_contract(s); }
            func main() -> i32 { 0 }
        "#,
        );
        let descriptor = program.ffi_declarations().values().next().unwrap();
        assert!(descriptor.has_declared_contract());
        assert!(
            matches!(&descriptor.requires, ResolvedFfiContractRoot::Deferred { errors, .. } if !errors.is_empty())
        );
        descriptor
            .validate(&program)
            .expect("explicit Deferred is data, not an omitted contract");
    }
    #[test]
    fn typed_ffi_declaration_descriptor_rejects_forged_root_parameter_and_result_identities() {
        let program = checked(
            r#"
            extern "C" { func foreign(value: i32) -> i32 requires: value >= 0 ensures: result >= value; }
            func wider() -> i64 { 0 }
            func main() -> i32 { foreign(1) }
        "#,
        );
        let descriptor = program.ffi_declarations().values().next().unwrap();
        descriptor
            .validate(&program)
            .expect("original declaration/root identities");
        assert!(matches!(
            descriptor.requires,
            ResolvedFfiContractRoot::Ready { .. }
        ));
        assert!(matches!(
            descriptor.ensures,
            ResolvedFfiContractRoot::Ready { .. }
        ));
        let mut root = descriptor.clone();
        root.requires = ResolvedFfiContractRoot::Ready {
            node_id: NodeId("forged:requires".into()),
        };
        assert!(root.validate(&program).is_err());
        let mut parameter = descriptor.clone();
        parameter.fixed_parameter_ids[0] = ResolvedParameterId(NodeId("forged:parameter".into()));
        assert!(parameter.validate(&program).is_err());
        let mut result = descriptor.clone();
        let wider = program
            .resolved_types()
            .iter()
            .find_map(|(id, ty)| {
                matches!(ty, ResolvedType::Primitive(PrimitiveType::I64)).then_some(id.clone())
            })
            .unwrap();
        result.signature.as_mut().unwrap().result = wider;
        assert!(result.validate(&program).is_err());
        let mut mode = descriptor.clone();
        mode.fixed_parameter_modes[0] = ResolvedFfiParameterMode::Move;
        assert!(mode.validate(&program).is_err());
    }

    #[test]
    fn typed_ffi_declaration_descriptor_rejects_forged_checker_owned_metadata() {
        let program = checked(
            r#"
            #[errno] #[no_panic] extern "C" {
                func foreign(value: i32) -> i32 requires: value >= 0 ensures: result >= value;
            }
            func main() -> i32 { foreign(1) }
        "#,
        );
        let descriptor = program.ffi_declarations().values().next().unwrap();
        descriptor
            .validate(&program)
            .expect("original checked descriptor");

        let mut abi = descriptor.clone();
        abi.abi = "system".into();
        assert!(abi.validate(&program).is_err());

        let mut symbol = descriptor.clone();
        symbol.symbol = "different_symbol".into();
        assert!(symbol.validate(&program).is_err());

        let mut no_panic = descriptor.clone();
        no_panic.invocation.block_no_panic = false;
        assert!(no_panic.validate(&program).is_err());

        let mut declaration_no_panic = descriptor.clone();
        declaration_no_panic.invocation.declaration_no_panic = true;
        assert!(declaration_no_panic.validate(&program).is_err());

        let mut errno = descriptor.clone();
        errno.invocation.block_returns_errno = false;
        assert!(errno.validate(&program).is_err());

        let mut declaration_errno = descriptor.clone();
        declaration_errno.invocation.declaration_returns_errno = true;
        assert!(declaration_errno.validate(&program).is_err());

        let mut unsafe_block = descriptor.clone();
        unsafe_block.invocation.unsafe_block = true;
        assert!(unsafe_block.validate(&program).is_err());

        let mut variadic = descriptor.clone();
        variadic.invocation.variadic = true;
        assert!(variadic.validate(&program).is_err());

        let mut contract = descriptor.clone();
        contract.requires = ResolvedFfiContractRoot::Deferred {
            node_id: None,
            errors: vec![ResolvedBodyError::new(
                descriptor.declaration.clone(),
                "forged Deferred contract",
            )],
        };
        assert!(contract.validate(&program).is_err());
    }

    #[test]
    fn typed_ffi_declaration_descriptor_is_bound_to_its_checked_program_roots() {
        let original = checked(
            r#"
            extern "C" {
                func foreign(value: i32) -> i32 requires: value >= 0 ensures: result >= value;
            }
            func main() -> i32 { foreign(1) }
        "#,
        );
        let changed = checked(
            r#"
            extern "C" {
                func foreign(value: i32) -> i32 requires: value >= 0 ensures: result >= value + 1;
            }
            func main() -> i32 { foreign(1) }
        "#,
        );
        let descriptor = original.ffi_declarations().values().next().unwrap();
        descriptor
            .validate(&original)
            .expect("descriptor remains valid in its originating CheckedProgram");
        assert!(descriptor.validate(&changed).is_err());
    }
    #[test]
    fn typed_ffi_declaration_descriptor_call_context_binds_actuals_and_retains_postcondition() {
        let program = checked(
            r#"
            extern "C" { func foreign(s: string, value: i32) -> i32 requires: value >= 0 ensures: result >= value; }
            func main() -> i32 { foreign("ok", 1) }
        "#,
        );
        let descriptor = program.ffi_declarations().values().next().unwrap();
        let body = program
            .resolved_bodies()
            .get(&NodeId("function:main".into()))
            .unwrap();
        let expression = body.root.result.as_ref().unwrap();
        let context = descriptor.call_context(&program, body, expression).unwrap();
        assert_eq!(context.inputs.len(), 2);
        assert!(context.requires.as_ref().unwrap().is_some());
        assert!(context.ensures.as_ref().unwrap().is_some());
        assert!(context.pending_effects.is_empty());
        assert_eq!(
            context.contract_result.0 .0,
            format!("{}/contract-result", descriptor.declaration.0)
        );
        let mut forged = expression.as_ref().clone();
        if let super::super::ResolvedExprKind::Call(call) = &mut forged.kind {
            call.arguments[0].parameter = ResolvedParameterId(NodeId("forged:parameter".into()));
        } else {
            panic!("call expression");
        }
        assert!(descriptor.call_context(&program, body, &forged).is_err());
        let deferred_program = checked(
            r#"
            extern "C" {
                func foreign(s: string, value: i32) -> i32
                    requires: value >= 0
                    ensures: unsupported_contract(result);
            }
            func main() -> i32 { foreign("ok", 1) }
        "#,
        );
        let deferred = deferred_program.ffi_declarations().values().next().unwrap();
        let deferred_body = deferred_program
            .resolved_bodies()
            .get(&NodeId("function:main".into()))
            .unwrap();
        let deferred_expression = deferred_body.root.result.as_ref().unwrap();
        let context = deferred
            .call_context(&deferred_program, deferred_body, deferred_expression)
            .unwrap();
        assert!(context.requires.is_ok());
        assert!(context.ensures.is_err());
    }
    #[test]
    fn typed_ffi_declaration_descriptor_distinct_calls_have_distinct_foreign_results() {
        let program = checked(
            r#"
            extern "C" { func foreign(s: string, value: i32) -> i32 ensures: result >= value; }
            func main() -> i32 {
                let earlier = foreign("first", 1)
                foreign("second", 2)
            }
        "#,
        );
        let descriptor = program.ffi_declarations().values().next().unwrap();
        let body = program
            .resolved_bodies()
            .get(&NodeId("function:main".into()))
            .unwrap();
        let super::super::ResolvedStmtKind::Bind {
            initializer: Some(first),
            ..
        } = &body.root.statements[0].kind
        else {
            panic!("first foreign result binding");
        };
        let first = descriptor.call_context(&program, body, first).unwrap();
        let second = descriptor
            .call_context(&program, body, body.root.result.as_ref().unwrap())
            .unwrap();
        assert_ne!(first.foreign_result, second.foreign_result);
        assert_eq!(first.contract_result, second.contract_result);
        assert!(first.ensures.as_ref().unwrap().is_some());
        assert!(second.ensures.as_ref().unwrap().is_some());
    }

    #[test]
    fn typed_ffi_variadic_context_records_c_default_promotion_receipts() {
        let program = variadic_checked();
        let (declaration, body, expression) = variadic_context_parts(&program);
        let context = declaration
            .call_context(&program, body, expression)
            .expect("typed variadic source receipt");
        let super::super::ResolvedExprKind::Call(call) = &expression.kind else {
            panic!("variadic call expression");
        };
        let receipts = context.variadic_tail();
        assert_eq!(receipts.len(), 6);
        assert!(context
            .pending_effects
            .contains(&ResolvedFfiPendingEffect::VariadicAbiAndPromotions));

        let i32_type = super::super::primitive_type_id(PrimitiveType::I32);
        let f64_type = super::super::primitive_type_id(PrimitiveType::F64);
        let expected = [
            ResolvedFfiPromotionState::Ready {
                target_type: i32_type.clone(),
                kind: ResolvedFfiDefaultPromotionKind::IntegerPromotionToI32,
            },
            ResolvedFfiPromotionState::Ready {
                target_type: f64_type.clone(),
                kind: ResolvedFfiDefaultPromotionKind::FloatPromotionToF64,
            },
            ResolvedFfiPromotionState::Ready {
                target_type: i32_type.clone(),
                kind: ResolvedFfiDefaultPromotionKind::Identity,
            },
            ResolvedFfiPromotionState::Ready {
                target_type: super::super::primitive_type_id(PrimitiveType::I64),
                kind: ResolvedFfiDefaultPromotionKind::Identity,
            },
            ResolvedFfiPromotionState::Ready {
                target_type: f64_type,
                kind: ResolvedFfiDefaultPromotionKind::Identity,
            },
            ResolvedFfiPromotionState::Deferred {
                reason: format!(
                    "no canonical C default-promotion rule for primitive {}",
                    PrimitiveType::String.language_name()
                ),
            },
        ];
        for (offset, (receipt, expected_promotion)) in receipts.iter().zip(expected).enumerate() {
            let ordinal = offset + 1;
            let argument = &call.arguments[ordinal];
            assert_eq!(receipt.call_node, expression.node_id);
            assert_eq!(receipt.ordinal, ordinal);
            assert_eq!(receipt.source_node, argument.value.node_id);
            assert_eq!(receipt.synthetic_parameter, argument.parameter);
            assert_eq!(
                receipt.synthetic_parameter.0 .0,
                format!("{}/variadic:{ordinal}", declaration.declaration.0)
            );
            assert_eq!(receipt.source_type, argument.value.ty);
            assert_eq!(
                program.resolved_node_type(&receipt.source_node),
                Some(&receipt.source_type)
            );
            assert_eq!(receipt.promotion, expected_promotion);
        }
    }

    #[test]
    fn typed_ffi_variadic_context_accepts_fixed_prefix_without_tail() {
        let program = checked(
            r#"
            extern "C" { func foreign(head: i32 ...) -> i32; }
            func main() -> i32 { foreign(7) }
        "#,
        );
        let (declaration, body, expression) = variadic_context_parts(&program);
        let context = declaration
            .call_context(&program, body, expression)
            .expect("a variadic call may have no tail operands");
        assert!(context.variadic_tail().is_empty());
    }

    #[test]
    fn typed_ffi_variadic_f32_tail_has_canonical_f64_target() {
        let program = checked(
            r#"
            extern "C" { func foreign(head: i32 ...) -> i32; }
            func main() -> i32 {
                let single: f32 = 1.0 as f32
                foreign(7, single)
            }
        "#,
        );
        let (declaration, body, expression) = variadic_context_parts(&program);
        let context = declaration
            .call_context(&program, body, expression)
            .expect("f32 variadic source has a known C promotion");
        assert_eq!(
            context.variadic_tail()[0].promotion,
            ResolvedFfiPromotionState::Ready {
                target_type: super::super::primitive_type_id(PrimitiveType::F64),
                kind: ResolvedFfiDefaultPromotionKind::FloatPromotionToF64,
            }
        );
    }

    #[test]
    fn typed_ffi_call_context_at_finds_nested_calls_and_rejects_bad_lookup() {
        let program = checked(
            r#"
            extern "C" { func foreign(head: i32 ...) -> i32; }
            func main() -> i32 { foreign(7, 4, foreign(9)) }
        "#,
        );
        let declaration = program.ffi_declarations().values().next().unwrap();
        let body = program
            .resolved_bodies()
            .get(&NodeId("function:main".into()))
            .unwrap();
        let outer = body.root.result.as_deref().unwrap();
        let super::super::ResolvedExprKind::Call(outer_call) = &outer.kind else {
            panic!("outer variadic call expression");
        };
        let nested = &outer_call.arguments[2].value;
        let context = declaration
            .call_context_at(&program, body, &nested.node_id)
            .expect("nested call is found without rebuilding source syntax");
        assert_eq!(context.call_node, &nested.node_id);
        assert!(context.variadic_tail().is_empty());

        let non_call = &outer_call.arguments[1].value.node_id;
        assert!(declaration
            .call_context_at(&program, body, non_call)
            .is_err());
        assert!(declaration
            .call_context_at(&program, body, &NodeId("absent:call".into()))
            .is_err());

        let cloned_body = body.clone();
        assert!(declaration
            .call_context_at(&program, &cloned_body, &nested.node_id)
            .is_err());

        let mut duplicate_body = body.clone();
        duplicate_body.root.result.as_deref_mut().unwrap().node_id = nested.node_id.clone();
        let duplicate_error = declaration
            .call_context_at(&program, &duplicate_body, &nested.node_id)
            .expect_err("a duplicate source identity is ambiguous");
        assert!(duplicate_error[0].message.contains("found 2 expressions"));
    }

    #[test]
    fn typed_ffi_variadic_context_rejects_same_id_source_promotion_and_arity_forgery() {
        let program = variadic_checked();
        let (declaration, body, expression) = variadic_context_parts(&program);
        let context = declaration
            .call_context(&program, body, expression)
            .expect("typed variadic source receipt");
        let super::super::ResolvedExprKind::Call(call) = &expression.kind else {
            panic!("variadic call expression");
        };
        let signature = declaration.signature.as_ref().unwrap();
        let canonical = context.variadic_tail().to_vec();
        assert!(validate_variadic_tail_receipts(
            &program,
            &expression.node_id,
            &declaration.declaration,
            call,
            &body.place_inputs,
            signature,
            true,
            &canonical,
        )
        .is_ok());
        assert!(validate_variadic_tail_receipts(
            &program,
            &expression.node_id,
            &declaration.declaration,
            call,
            &body.place_inputs,
            signature,
            false,
            &canonical,
        )
        .is_err());

        let i64_type = super::super::primitive_type_id(PrimitiveType::I64);
        let mut forged_source = canonical.clone();
        let original_source_node = forged_source[0].source_node.clone();
        forged_source[0].source_type = i64_type.clone();
        assert_eq!(forged_source[0].source_node, original_source_node);
        assert!(validate_variadic_tail_receipts(
            &program,
            &expression.node_id,
            &declaration.declaration,
            call,
            &body.place_inputs,
            signature,
            true,
            &forged_source,
        )
        .is_err());

        let mut forged_promotion = canonical.clone();
        let same_source_node = forged_promotion[0].source_node.clone();
        forged_promotion[0].promotion = ResolvedFfiPromotionState::Ready {
            target_type: i64_type.clone(),
            kind: ResolvedFfiDefaultPromotionKind::Identity,
        };
        assert_eq!(forged_promotion[0].source_node, same_source_node);
        assert!(validate_variadic_tail_receipts(
            &program,
            &expression.node_id,
            &declaration.declaration,
            call,
            &body.place_inputs,
            signature,
            true,
            &forged_promotion,
        )
        .is_err());

        let mut forged_ordinal = canonical.clone();
        forged_ordinal[0].ordinal += 1;
        assert!(validate_variadic_tail_receipts(
            &program,
            &expression.node_id,
            &declaration.declaration,
            call,
            &body.place_inputs,
            signature,
            true,
            &forged_ordinal,
        )
        .is_err());

        let mut incomplete = canonical;
        incomplete.pop();
        assert!(validate_variadic_tail_receipts(
            &program,
            &expression.node_id,
            &declaration.declaration,
            call,
            &body.place_inputs,
            signature,
            true,
            &incomplete,
        )
        .is_err());

        let mut forged_source_expr = expression.clone();
        let super::super::ResolvedExprKind::Call(forged_call) = &mut forged_source_expr.kind else {
            panic!("variadic call expression");
        };
        let argument_node = forged_call.arguments[1].value.node_id.clone();
        forged_call.arguments[1].value.ty = i64_type.clone();
        forged_call.arguments[1].conversion.from = i64_type.clone();
        forged_call.arguments[1].conversion.to = i64_type;
        assert_eq!(forged_call.arguments[1].value.node_id, argument_node);
        assert!(declaration
            .call_context(&program, body, &forged_source_expr)
            .is_err());

        let mut forged_arity = expression.clone();
        let super::super::ResolvedExprKind::Call(forged_call) = &mut forged_arity.kind else {
            panic!("variadic call expression");
        };
        forged_call.arguments.pop();
        assert!(declaration
            .call_context(&program, body, &forged_arity)
            .is_err());
    }
}
