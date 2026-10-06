//! AST-free lowering for the closed synchronous scalar Actor graph.
//!
//! The checker descriptor, lexical environments and operation receipts are
//! constructed together. Unsupported bodies retain the existing compatibility
//! disposition; a graph admitted by this contract never calls a legacy body.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::core::ir::{
    CheckedConversionKind, ResolvedBlock, ResolvedCall, ResolvedCallee, ResolvedExpr,
    ResolvedExprKind, ResolvedLiteral, ResolvedLocalId, ResolvedPatternKind, ResolvedPlace,
    ResolvedProjection, ResolvedStmtKind, ResolvedTypeId,
};
use crate::core::{CheckedProgram, NodeId, PrimitiveType, ResolvedCallable};
use crate::span::SourceId;

use super::actor::{MirActorCatalog, MirActorOperation, MirActorOperationReceipts};
use super::actor_execution::MirActorExecutionPlan;
use super::closure_environment::CheckedActorEnvironmentPlan;
use super::reference::{MirProgram, MirProgramBuildError};
use super::types::{MirBuiltinKind, MirTypeCatalog};
use super::{
    MirBlock, MirBlockId, MirBlockParameter, MirEdgeId, MirFunction, MirInstruction,
    MirInstructionId, MirInstructionKind, MirOwnershipSummary, MirTerminator, MirValue, MirValueId,
};

fn selected_actor_ids(
    program: &CheckedProgram,
    excluded: Option<&HashSet<SourceId>>,
) -> BTreeSet<NodeId> {
    program
        .actor_descriptors()
        .iter()
        .filter(|(_, descriptor)| {
            descriptor.methods.iter().any(|method| {
                program.callable(&method.owner).is_some_and(|callable| {
                    excluded.is_none_or(|sources| {
                        !sources.contains(&callable.body.root.origin.user_span().source_id)
                    })
                })
            })
        })
        .map(|(owner, _)| owner.clone())
        .collect()
}

pub(crate) fn has_actor_candidate(
    program: &CheckedProgram,
    excluded: Option<&HashSet<SourceId>>,
) -> bool {
    !selected_actor_ids(program, excluded).is_empty()
}

/// Return whether a selected Actor method contains a direct nested callable
/// declaration.  These declarations are the historically reachable Actor
/// fallback shape (capture/shadow); if the finite lexical-environment receipt
/// cannot be materialized, production dispatch must reject the graph rather
/// than hand the method to the legacy AST emitter.
pub(crate) fn has_nested_callable_candidate(
    program: &CheckedProgram,
    excluded: Option<&HashSet<SourceId>>,
) -> bool {
    selected_actor_ids(program, excluded).iter().any(|actor| {
        program
            .actor_descriptors()
            .get(actor)
            .is_some_and(|descriptor| {
                descriptor.methods.iter().any(|method| {
                    program.callable(&method.owner).is_some_and(|callable| {
                        callable.body.root.statements.iter().any(|statement| {
                            matches!(
                                statement.kind,
                                crate::core::ir::ResolvedStmtKind::NestedCallable(_)
                            )
                        })
                    })
                })
            })
    })
}

/// A NoContracts Actor graph cannot discard an obligation in any checked
/// user callable or contract declaration, even when that owner is unreachable.
pub(crate) fn contract_boundary_reason(
    program: &CheckedProgram,
    excluded: Option<&HashSet<SourceId>>,
) -> Option<String> {
    let included = |source| excluded.is_none_or(|sources| !sources.contains(&source));
    for (owner, callable) in program.callables() {
        if included(callable.body.root.origin.user_span().source_id)
            && callable.has_proof_obligations()
        {
            return Some(format!("scalar Actor NoContracts profile cannot discard proof obligations in checked user callable '{}'", owner.0));
        }
    }
    use crate::core::ir::lower::ResolvedDeclarationExpressionKind as K;
    for root in program.declaration_expression_bodies().values() {
        let has_obligations = matches!(root.kind, K::ExternRequires | K::ExternEnsures)
            || match &root.body {
                Ok(body) => body.has_proof_obligations(),
                Err(_) => true,
            };
        if !has_obligations {
            continue;
        }
        let source = program
            .node_meta()
            .get(&root.node_id)
            .or_else(|| program.node_meta().get(&root.owner))
            .map(|meta| meta.origin.user_span().source_id)
            .or_else(|| {
                root.body
                    .as_ref()
                    .ok()
                    .map(|body| body.root.origin.user_span().source_id)
            });
        if source.is_none_or(included) {
            return Some(format!(
                "scalar Actor NoContracts profile cannot discard checked contract declaration '{}'",
                root.node_id.0
            ));
        }
    }
    None
}

pub(crate) fn materialize(
    program: &CheckedProgram,
    excluded: Option<&HashSet<SourceId>>,
) -> Result<MirProgram, MirProgramBuildError> {
    // Actor's reachable graph must not silently discard a declared export
    // wrapper. Preserve the same source-filtered ABI boundary as ordinary MIR.
    super::lower::reject_exported_abi_bodies(program, excluded)
        .map_err(MirProgramBuildError::Lowering)?;
    if let Some(reason) = contract_boundary_reason(program, excluded) {
        return Err(validation(vec![reason]));
    }
    let actors =
        MirActorCatalog::from_checked_program(program, &selected_actor_ids(program, excluded))
            .map_err(validation)?;
    let mut types =
        MirTypeCatalog::from_checked_program(program).map_err(MirProgramBuildError::Types)?;
    types
        .materialize_actor_catalog(&actors)
        .map_err(validation)?;
    let environments = CheckedActorEnvironmentPlan::derive(program, &actors).map_err(validation)?;
    let root = NodeId("function:main".into());
    let mut selected = BTreeSet::from([root.clone()]);
    selected.extend(
        actors
            .iter()
            .flat_map(|(_, actor)| actor.methods.iter().map(|method| method.owner.clone())),
    );
    selected.extend(environments.helpers().cloned());
    let mut pending = selected.iter().cloned().collect::<Vec<_>>();
    let mut functions = BTreeMap::new();
    let mut operations = MirActorOperationReceipts::default();
    while let Some(owner) = pending.pop() {
        if functions.contains_key(&owner) {
            continue;
        }
        let callable = program.callable(&owner).ok_or_else(|| {
            validation(vec![format!(
                "Actor graph function '{}' has no checker body",
                owner.0
            )])
        })?;
        if excluded.is_some_and(|sources| {
            sources.contains(&callable.body.root.origin.user_span().source_id)
        }) {
            return Err(validation(vec![format!(
                "Actor graph reaches excluded function '{}'",
                owner.0
            )]));
        }
        let function = ActorLowerer::new(callable, &actors, &environments, &mut operations)
            .and_then(ActorLowerer::finish)
            .map_err(validation)?;
        for instruction in function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
        {
            if let MirInstructionKind::Call {
                callee: ResolvedCallee::Function(target),
                ..
            } = &instruction.kind
            {
                if selected.insert(target.clone()) {
                    pending.push(target.clone());
                }
            }
        }
        functions.insert(owner, function);
    }
    let receipts = environments.materialize(&functions).map_err(validation)?;
    operations.validate(&functions).map_err(validation)?;
    actors
        .validate_functions(&types, &functions)
        .map_err(validation)?;
    let execution =
        MirActorExecutionPlan::preflight(&actors, &types, &functions, &root).map_err(validation)?;
    MirProgram::from_actor_components(functions, types, actors, execution, receipts, operations)
        .map_err(MirProgramBuildError::Validation)
}

fn validation(messages: Vec<String>) -> MirProgramBuildError {
    MirProgramBuildError::Validation(
        messages
            .into_iter()
            .map(|message| super::MirValidationError {
                subject: "actor-graph".into(),
                message,
            })
            .collect(),
    )
}

struct ActorLowerer<'a> {
    callable: &'a ResolvedCallable,
    actors: &'a MirActorCatalog,
    environments: &'a CheckedActorEnvironmentPlan,
    operations: &'a mut MirActorOperationReceipts,
    function: MirFunction,
    current: MirBlockId,
    locals: BTreeMap<ResolvedLocalId, MirValueId>,
    declared: BTreeSet<NodeId>,
    sequence: usize,
}

impl<'a> ActorLowerer<'a> {
    fn new(
        callable: &'a ResolvedCallable,
        actors: &'a MirActorCatalog,
        environments: &'a CheckedActorEnvironmentPlan,
        operations: &'a mut MirActorOperationReceipts,
    ) -> Result<Self, Vec<String>> {
        if callable.owner != callable.body.owner
            || callable.owner != callable.signature.owner
            || !callable.signature.generic_parameters.is_empty()
            || !callable.signature.effects.is_empty()
            || !callable.contracts.is_empty()
            || !callable.body.default_values.is_empty()
            || callable
                .signature
                .parameters
                .iter()
                .enumerate()
                .any(|(index, parameter)| {
                    parameter.has_default || {
                        if index == 0 {
                            if let Some((_, method)) = actors.method(&callable.owner) {
                                parameter.permission != method.source_receiver_permission
                            } else {
                                parameter.permission.is_some()
                            }
                        } else {
                            parameter.permission.is_some()
                        }
                    }
                })
            || callable.signature.parameters.len() != callable.body.parameters.len()
            || (!callable.body.captures.is_empty()
                && environments.target(&callable.owner).is_none())
        {
            return Err(vec![format!(
                "Actor graph function '{}' has no concrete finite scalar signature",
                callable.owner.0
            )]);
        }
        let entry = MirBlockId::new("bb.entry").map_err(|e| vec![e.to_string()])?;
        let function = MirFunction {
            owner: callable.owner.clone(),
            parameters: Vec::new(),
            parameter_permissions: Some(Vec::new()),
            result: callable.signature.result.clone(),
            entry: entry.clone(),
            values: BTreeMap::new(),
            blocks: BTreeMap::from([(
                entry.clone(),
                MirBlock {
                    id: entry.clone(),
                    parameters: Vec::new(),
                    instructions: Vec::new(),
                    terminator: MirTerminator::Unreachable,
                },
            )]),
            contracts: Vec::new(),
            proof_worklist: None,
            ownership: MirOwnershipSummary::default(),
        };
        let mut this = Self {
            callable,
            actors,
            environments,
            operations,
            function,
            current: entry,
            locals: BTreeMap::new(),
            declared: BTreeSet::new(),
            sequence: 0,
        };
        for (index, local) in callable.body.parameters.iter().enumerate() {
            let definition = callable
                .body
                .locals
                .get(local)
                .ok_or_else(|| this.error("parameter local is absent"))?;
            let parameter = &callable.signature.parameters[index];
            if definition.id != *local || definition.ty != parameter.ty {
                return Err(
                    this.error("parameter local identity/type differs from checker signature")
                );
            }
            let mut ty = parameter.ty.clone();
            if index == 0 {
                if let Some((actor, method)) = actors.method(&callable.owner) {
                    if &method.self_local != local {
                        return Err(this.error("Actor self parameter identity differs"));
                    }
                    ty = actor.state_type.clone();
                }
            }
            let value = value_id(format!("local:{}", local.0 .0))?;
            this.function.values.insert(
                value.clone(),
                MirValue {
                    id: value.clone(),
                    ty,
                },
            );
            this.function.parameters.push(value.clone());
            this.locals.insert(local.clone(), value);
        }
        for slot in environments.slots(&callable.owner) {
            this.function.values.insert(
                slot.callee_parameter.clone(),
                MirValue {
                    id: slot.callee_parameter.clone(),
                    ty: slot.ty.clone(),
                },
            );
            this.function.parameters.push(slot.callee_parameter.clone());
            this.locals
                .insert(slot.capture.clone(), slot.callee_parameter.clone());
        }
        // The checked source self borrow is discharged by the mailbox turn.
        // ActorState is private and turn-scoped; it is never a source handle borrow.
        this.function.parameter_permissions = Some(vec![None; this.function.parameters.len()]);
        Ok(this)
    }

    fn error(&self, message: &str) -> Vec<String> {
        vec![format!(
            "Actor lowering '{}': {message}",
            self.callable.owner.0
        )]
    }
    fn fresh(&mut self, role: &str) -> Result<MirValueId, Vec<String>> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| self.error("identity sequence overflow"))?;
        value_id(format!(
            "actor:{role}:{}:{}",
            self.callable.owner.0, self.sequence
        ))
    }
    fn value(&mut self, ty: &ResolvedTypeId, role: &str) -> Result<MirValueId, Vec<String>> {
        let result = self.fresh(role)?;
        self.function.values.insert(
            result.clone(),
            MirValue {
                id: result.clone(),
                ty: ty.clone(),
            },
        );
        Ok(result)
    }
    fn emit(
        &mut self,
        node: &NodeId,
        role: &str,
        kind: MirInstructionKind,
    ) -> Result<(), Vec<String>> {
        let id = MirInstructionId::new(format!("inst:actor_{role}:{}:{}", node.0, self.sequence))
            .map_err(|e| vec![e.to_string()])?;
        if let MirInstructionKind::Actor { operation } = &kind {
            self.operations
                .record(&self.callable.owner, &id, operation)
                .map_err(|e| vec![e])?;
        }
        self.function
            .blocks
            .get_mut(&self.current)
            .ok_or_else(|| vec!["Actor lowering current block is absent".into()])?
            .instructions
            .push(MirInstruction { id, kind });
        Ok(())
    }
    fn emit_capture(&mut self, callee: &NodeId, declaration: &NodeId) -> Result<(), Vec<String>> {
        if self.environments.declaration(&self.callable.owner, callee) != Some(declaration)
            || !self.declared.insert(callee.clone())
        {
            return Err(self.error("nested declaration lacks its exact Actor lexical receipt"));
        }
        for slot in self.environments.slots(callee) {
            if self.locals.get(&slot.capture) != Some(&slot.source) {
                return Err(self.error("capture source is not its declaration-time local"));
            }
            self.function.values.insert(
                slot.snapshot.clone(),
                MirValue {
                    id: slot.snapshot.clone(),
                    ty: slot.ty.clone(),
                },
            );
            self.function
                .blocks
                .get_mut(&self.current)
                .expect("current block exists")
                .instructions
                .push(MirInstruction {
                    id: slot.declaration_copy.clone(),
                    kind: MirInstructionKind::Copy {
                        result: slot.snapshot.clone(),
                        source: slot.source.clone(),
                    },
                });
        }
        Ok(())
    }
    fn local(&self, local: &ResolvedLocalId) -> Result<MirValueId, Vec<String>> {
        self.locals
            .get(local)
            .cloned()
            .ok_or_else(|| self.error("local was used before definition"))
    }
    fn terminated(&self) -> bool {
        !matches!(
            self.function.blocks[&self.current].terminator,
            MirTerminator::Unreachable
        )
    }
    fn terminate(&mut self, terminator: MirTerminator) {
        self.function
            .blocks
            .get_mut(&self.current)
            .expect("current block exists")
            .terminator = terminator;
    }

    fn statements(&mut self, block: &ResolvedBlock) -> Result<(), Vec<String>> {
        for statement in &block.statements {
            if self.terminated() {
                return Err(self.error("statement follows a terminated path"));
            }
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                } => {
                    let ResolvedPatternKind::Binding { local, .. } = &pattern.kind else {
                        return Err(self.error("only direct scalar bindings are admitted"));
                    };
                    let source = self.expr(initializer)?;
                    let definition =
                        self.callable.body.locals.get(local).ok_or_else(|| {
                            self.error("binding local has no checker catalog entry")
                        })?;
                    if self.function.values[&source].ty != definition.ty {
                        return Err(self.error("binding requires an unsupported conversion"));
                    }
                    let result = value_id(format!("local:{}", local.0 .0))?;
                    if self.function.values.contains_key(&result) {
                        return Err(self.error("binding identity is duplicated"));
                    }
                    self.function.values.insert(
                        result.clone(),
                        MirValue {
                            id: result.clone(),
                            ty: definition.ty.clone(),
                        },
                    );
                    self.emit(
                        &statement.node_id,
                        "bind",
                        MirInstructionKind::Move {
                            result: result.clone(),
                            source,
                        },
                    )?;
                    self.locals.insert(local.clone(), result);
                }
                ResolvedStmtKind::Assign {
                    target,
                    value,
                    conversion,
                } => {
                    if conversion.kind != CheckedConversionKind::Identity {
                        return Err(self.error("assignment requires an unsupported conversion"));
                    }
                    let source = self.expr(value)?;
                    if target.projections.is_empty() {
                        let definition = self
                            .callable
                            .body
                            .locals
                            .get(&target.base)
                            .ok_or_else(|| self.error("assignment local is absent"))?;
                        if !definition.mutable || definition.ty != self.function.values[&source].ty
                        {
                            return Err(self.error("assignment mutability/type disagrees"));
                        }
                        let result = self.value(&definition.ty, "assign")?;
                        self.emit(
                            &statement.node_id,
                            "assign",
                            MirInstructionKind::Move {
                                result: result.clone(),
                                source,
                            },
                        )?;
                        self.locals.insert(target.base.clone(), result);
                    } else {
                        let (actor, field, state) = self.field(target)?;
                        self.emit(
                            &statement.node_id,
                            "field_store",
                            MirInstructionKind::Actor {
                                operation: MirActorOperation::FieldStore {
                                    actor,
                                    field,
                                    state,
                                    value: source,
                                },
                            },
                        )?;
                    }
                }
                ResolvedStmtKind::Expr(expression) => {
                    self.expr(expression)?;
                }
                ResolvedStmtKind::Return {
                    value: Some(value),
                    conversion,
                } => {
                    if conversion.as_ref().is_some_and(|conversion| {
                        conversion.kind != CheckedConversionKind::Identity
                    }) {
                        return Err(self.error("return conversion is outside the scalar Actor ABI"));
                    }
                    let value = self.expr(value)?;
                    self.terminate(MirTerminator::Return { value: Some(value) });
                }
                ResolvedStmtKind::NestedCallable(callee) => {
                    self.emit_capture(callee, &statement.node_id)?
                }
                _ => {
                    return Err(self.error("statement has no finite scalar Actor lowering contract"))
                }
            }
        }
        Ok(())
    }

    fn field(&self, place: &ResolvedPlace) -> Result<(NodeId, NodeId, MirValueId), Vec<String>> {
        let Some((actor, method)) = self.actors.method(&self.callable.owner) else {
            return Err(self.error("state field access is outside an Actor method"));
        };
        let [ResolvedProjection::Field { field, ty, .. }] = place.projections.as_slice() else {
            return Err(self.error("state access requires exactly one field projection"));
        };
        if place.base != method.self_local
            || !actor
                .fields
                .iter()
                .any(|entry| entry.field == *field && entry.ty == *ty)
        {
            return Err(self.error("state field identity/type is foreign to the method"));
        }
        let state = self.local(&place.base)?;
        if state != method.self_value {
            return Err(self.error("state receiver has been rebound"));
        }
        Ok((actor.actor.clone(), field.clone(), state))
    }

    fn expr(&mut self, expression: &ResolvedExpr) -> Result<MirValueId, Vec<String>> {
        if !expression.effects.is_empty() || !expression.backend_requirements.is_empty() {
            return Err(self.error("expression has an unmodeled effect or backend requirement"));
        }
        match &expression.kind {
            ResolvedExprKind::Literal(
                literal @ (ResolvedLiteral::Int(_)
                | ResolvedLiteral::Bool(_)
                | ResolvedLiteral::Unit),
            ) => {
                let result = self.value(&expression.ty, "literal")?;
                self.emit(
                    &expression.node_id,
                    "literal",
                    MirInstructionKind::Const {
                        result: result.clone(),
                        literal: literal.clone(),
                    },
                )?;
                Ok(result)
            }
            ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                self.local(&place.base)
            }
            ResolvedExprKind::Load(place) => {
                let (actor, field, state) = self.field(place)?;
                let result = self.value(&expression.ty, "field")?;
                self.emit(
                    &expression.node_id,
                    "field_load",
                    MirInstructionKind::Actor {
                        operation: MirActorOperation::FieldLoad {
                            result: result.clone(),
                            actor,
                            field,
                            state,
                        },
                    },
                )?;
                Ok(result)
            }
            ResolvedExprKind::Binary { op, left, right } => {
                if matches!(
                    op,
                    crate::core::ir::ResolvedBinaryOp::LogicalAnd
                        | crate::core::ir::ResolvedBinaryOp::LogicalOr
                ) {
                    return self.logical(expression, *op, left, right);
                }
                let left = self.expr(left)?;
                let right = self.expr(right)?;
                let result = self.value(&expression.ty, "binary")?;
                self.emit(
                    &expression.node_id,
                    "binary",
                    MirInstructionKind::Binary {
                        result: result.clone(),
                        op: *op,
                        left,
                        right,
                    },
                )?;
                Ok(result)
            }
            ResolvedExprKind::Unary { op, operand }
                if matches!(
                    op,
                    crate::core::ir::ResolvedUnaryOp::Negate
                        | crate::core::ir::ResolvedUnaryOp::Not
                ) =>
            {
                let operand = self.expr(operand)?;
                let result = self.value(&expression.ty, "unary")?;
                self.emit(
                    &expression.node_id,
                    "unary",
                    MirInstructionKind::Unary {
                        result: result.clone(),
                        op: *op,
                        operand,
                    },
                )?;
                Ok(result)
            }
            ResolvedExprKind::Call(call) => self.call(expression, call),
            ResolvedExprKind::Block(block) => {
                self.statements(block)?;
                match &block.result {
                    Some(result) => self.expr(result),
                    None => self.unit(&expression.node_id),
                }
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => self.branch(expression, condition, then_block, else_block),
            _ => Err(self.error("expression has no finite scalar Actor lowering contract")),
        }
    }

    fn unit(&mut self, node: &NodeId) -> Result<MirValueId, Vec<String>> {
        let ty = crate::core::ir::primitive_type_id(PrimitiveType::Unit);
        let result = self.value(&ty, "unit")?;
        self.emit(
            node,
            "unit",
            MirInstructionKind::Const {
                result: result.clone(),
                literal: ResolvedLiteral::Unit,
            },
        )?;
        Ok(result)
    }

    fn call(
        &mut self,
        expression: &ResolvedExpr,
        call: &ResolvedCall,
    ) -> Result<MirValueId, Vec<String>> {
        if matches!(call.callee, ResolvedCallee::ActorMethod { .. }) {
            self.actors
                .validate_source_mailbox_call(call)
                .map_err(|message| self.error(&message))?;
        } else if !call.effects.is_empty() || !call.session.is_empty() || call.permission.is_some()
        {
            return Err(self.error("call has an unmodeled effect/session/borrow"));
        }
        if let ResolvedCallee::Builtin(builtin) = &call.callee {
            if builtin.as_str() == "actor.spawn" {
                let actor = self
                    .actors
                    .iter()
                    .find(|(_, actor)| actor.handle_type == expression.ty)
                    .map(|(owner, _)| owner.clone())
                    .ok_or_else(|| {
                        self.error("spawn result has no exact Actor handle descriptor")
                    })?;
                if !call.arguments.is_empty() || call.type_arguments != vec![expression.ty.clone()]
                {
                    return Err(self.error("spawn arguments/type receipt disagrees"));
                }
                let result = self.value(&expression.ty, "spawn")?;
                self.emit(
                    &expression.node_id,
                    "spawn",
                    MirInstructionKind::Actor {
                        operation: MirActorOperation::Spawn {
                            result: result.clone(),
                            actor,
                        },
                    },
                )?;
                return Ok(result);
            }
        }
        let mut arguments = Vec::new();
        for argument in &call.arguments {
            if argument.conversion.kind != CheckedConversionKind::Identity {
                return Err(self.error("call argument needs an unsupported conversion"));
            }
            arguments.push(self.expr(&argument.value)?);
        }
        let result = self.value(&expression.ty, "call")?;
        match &call.callee {
            ResolvedCallee::ActorMethod { actor, method } => {
                if !call.type_arguments.is_empty() || arguments.is_empty() {
                    return Err(self.error("mailbox call has an invalid type/payload partition"));
                }
                let handle = arguments.remove(0);
                self.emit(
                    &expression.node_id,
                    "mailbox",
                    MirInstructionKind::Actor {
                        operation: MirActorOperation::Call {
                            result: result.clone(),
                            actor: actor.clone(),
                            method: NodeId(method.as_str().into()),
                            handle,
                            args: arguments,
                        },
                    },
                )?;
            }
            ResolvedCallee::Function(target) => {
                if !call.type_arguments.is_empty() {
                    return Err(self.error("generic helper calls lack a scalar Actor receipt"));
                }
                if let Some(parent) = self.environments.target(target) {
                    if parent != &self.callable.owner || !self.declared.contains(target) {
                        return Err(self.error("helper call is outside its declaration scope"));
                    }
                    arguments.extend(
                        self.environments
                            .slots(target)
                            .iter()
                            .map(|slot| slot.snapshot.clone()),
                    );
                } else if target.0.contains("/function:") {
                    return Err(
                        self.error("nested helper is missing its lexical environment receipt")
                    );
                }
                self.emit(
                    &expression.node_id,
                    "direct_call",
                    MirInstructionKind::Call {
                        result: Some(result.clone()),
                        callee: call.callee.clone(),
                        type_arguments: Vec::new(),
                        arguments,
                        effect_receipts: Vec::new(),
                        variant_call_contract: None,
                    },
                )?;
            }
            ResolvedCallee::Builtin(builtin)
                if builtin.as_str() == "println" || builtin.as_str() == "builtin:println" =>
            {
                if !call.type_arguments.is_empty() || arguments.len() != 1 {
                    return Err(self.error("println payload partition is invalid"));
                }
                let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
                let kind = if self.function.values[&arguments[0]].ty == bool_type {
                    MirBuiltinKind::PrintlnBool
                } else {
                    MirBuiltinKind::PrintlnInt
                };
                self.emit(
                    &expression.node_id,
                    "println",
                    MirInstructionKind::BuiltinCall {
                        result: result.clone(),
                        kind,
                        arguments,
                        string_field_contract: None,
                    },
                )?;
            }
            _ => {
                return Err(
                    self.error("call requires an unsupported mailbox/extern/indirect contract")
                )
            }
        }
        Ok(result)
    }

    fn new_block(&mut self, role: &str) -> Result<MirBlockId, Vec<String>> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| self.error("block identity sequence overflow"))?;
        let id = MirBlockId::new(format!("bb.actor.{role}.{}", self.sequence))
            .map_err(|e| vec![e.to_string()])?;
        self.function.blocks.insert(
            id.clone(),
            MirBlock {
                id: id.clone(),
                parameters: Vec::new(),
                instructions: Vec::new(),
                terminator: MirTerminator::Unreachable,
            },
        );
        Ok(id)
    }
    fn edge(&self, source: &MirBlockId, role: &str) -> Result<MirEdgeId, Vec<String>> {
        MirEdgeId::new(format!("edge:{}:{role}", source.as_str())).map_err(|e| vec![e.to_string()])
    }

    fn branch(
        &mut self,
        expression: &ResolvedExpr,
        condition: &ResolvedExpr,
        then_block: &ResolvedBlock,
        else_block: &ResolvedBlock,
    ) -> Result<MirValueId, Vec<String>> {
        let condition = self.expr(condition)?;
        let start = self.current.clone();
        let then_id = self.new_block("then")?;
        let else_id = self.new_block("else")?;
        let join = self.new_block("join")?;
        self.current = start.clone();
        self.terminate(MirTerminator::Branch {
            condition,
            then_edge: self.edge(&start, "then")?,
            then_target: then_id.clone(),
            then_arguments: Vec::new(),
            else_edge: self.edge(&start, "else")?,
            else_target: else_id.clone(),
            else_arguments: Vec::new(),
        });
        let entry_locals = self.locals.clone();
        let entry_declarations = self.declared.clone();
        self.current = then_id;
        self.statements(then_block)?;
        let then_value = match &then_block.result {
            Some(value) if !self.terminated() => Some(self.expr(value)?),
            _ if !self.terminated() => Some(self.unit(&expression.node_id)?),
            _ => None,
        };
        let then_end = self.current.clone();
        let then_locals = self.locals.clone();
        self.current = else_id;
        self.locals = entry_locals.clone();
        self.declared = entry_declarations.clone();
        self.statements(else_block)?;
        let else_value = match &else_block.result {
            Some(value) if !self.terminated() => Some(self.expr(value)?),
            _ if !self.terminated() => Some(self.unit(&expression.node_id)?),
            _ => None,
        };
        let else_end = self.current.clone();
        let else_locals = self.locals.clone();
        if then_value.is_none() && else_value.is_none() {
            self.function.blocks.remove(&join);
            return Err(self.error("if with two terminal arms needs statement-result lowering"));
        }
        self.current = join.clone();
        self.locals = entry_locals.clone();
        self.declared = entry_declarations;
        let result = self.value(&expression.ty, "if_result")?;
        self.function
            .blocks
            .get_mut(&join)
            .expect("join exists")
            .parameters
            .push(MirBlockParameter {
                value: result.clone(),
            });
        let mut then_args = then_value.into_iter().collect::<Vec<_>>();
        let mut else_args = else_value.into_iter().collect::<Vec<_>>();
        for (local, old) in entry_locals {
            let then_value = then_locals.get(&local).unwrap_or(&old);
            let else_value = else_locals.get(&local).unwrap_or(&old);
            if then_value == else_value {
                self.locals.insert(local, then_value.clone());
                continue;
            }
            let ty = self.function.values[&old].ty.clone();
            let value = self.value(&ty, "if_local")?;
            self.function
                .blocks
                .get_mut(&join)
                .expect("join exists")
                .parameters
                .push(MirBlockParameter {
                    value: value.clone(),
                });
            then_args.push(then_value.clone());
            else_args.push(else_value.clone());
            self.locals.insert(local, value);
        }
        if matches!(
            self.function.blocks[&then_end].terminator,
            MirTerminator::Unreachable
        ) {
            self.current = then_end.clone();
            self.terminate(MirTerminator::Goto {
                edge: self.edge(&then_end, "join")?,
                target: join.clone(),
                arguments: then_args,
            });
        }
        if matches!(
            self.function.blocks[&else_end].terminator,
            MirTerminator::Unreachable
        ) {
            self.current = else_end.clone();
            self.terminate(MirTerminator::Goto {
                edge: self.edge(&else_end, "join")?,
                target: join.clone(),
                arguments: else_args,
            });
        }
        self.current = join;
        Ok(result)
    }

    fn logical(
        &mut self,
        expression: &ResolvedExpr,
        op: crate::core::ir::ResolvedBinaryOp,
        left: &ResolvedExpr,
        right: &ResolvedExpr,
    ) -> Result<MirValueId, Vec<String>> {
        // Reuse canonical branch arguments so the right operand is evaluated
        // only on its source-language path, including its trap effects.
        let constant = ResolvedExpr {
            node_id: NodeId(format!("{}/short-circuit", expression.node_id.0)),
            origin: expression.origin.clone(),
            ty: expression.ty.clone(),
            effects: Vec::new(),
            backend_requirements: Vec::new(),
            kind: ResolvedExprKind::Literal(ResolvedLiteral::Bool(
                op == crate::core::ir::ResolvedBinaryOp::LogicalOr,
            )),
        };
        let block = |value: ResolvedExpr| ResolvedBlock {
            node_id: value.node_id.clone(),
            origin: value.origin.clone(),
            ty: value.ty.clone(),
            statements: Vec::new(),
            result: Some(Box::new(value)),
        };
        let rhs = block(right.clone());
        let shortcut = block(constant);
        if op == crate::core::ir::ResolvedBinaryOp::LogicalAnd {
            self.branch(expression, left, &rhs, &shortcut)
        } else {
            self.branch(expression, left, &shortcut, &rhs)
        }
    }

    fn finish(mut self) -> Result<MirFunction, Vec<String>> {
        self.statements(&self.callable.body.root)?;
        if !self.terminated() {
            let result = self
                .callable
                .body
                .root
                .result
                .as_deref()
                .ok_or_else(|| self.error("finite Actor callable has no i32 result"))?;
            let value = self.expr(result)?;
            self.terminate(MirTerminator::Return { value: Some(value) });
        }
        self.function.validate().map_err(|errors| {
            errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
        })?;
        Ok(self.function)
    }
}

fn value_id(value: String) -> Result<MirValueId, Vec<String>> {
    MirValueId::new(value).map_err(|error| vec![error.to_string()])
}
