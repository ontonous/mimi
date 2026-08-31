//! Initial lowering from checker-owned ResolvedBody to canonical MIR.
//!
//! This is intentionally a narrow, fail-closed slice. It proves the
//! architectural boundary for scalar expressions, structured branch control
//! flow, Copy record aggregates, and recursive Move-owned tuple/record product
//! glue shapes (for example `(string, i32)` or `{ name: string, count: i32 }`).
//! Unsupported shapes return a structured error and must not
//! silently select the legacy emitter.

use std::collections::{BTreeMap, HashMap};

use crate::core::ir::{
    NominalTypeId, ResolvedBlock, ResolvedCall, ResolvedCallee, ResolvedExpr, ResolvedExprKind,
    ResolvedPattern, ResolvedPatternKind, ResolvedStmtKind, ResolvedUnaryOp,
};
use crate::core::{
    CanonicalActionKind, CheckedProgram, NodeId, ResolvedBody, ResolvedLocalId, ResourceAnalysis,
};

use super::types::MirTypeCatalog;
use super::{
    MirAggregateKind, MirBlock, MirBlockId, MirBlockParameter, MirEdgeId, MirFunction,
    MirInstruction, MirInstructionId, MirInstructionKind, MirOwnershipEvent, MirOwnershipEventKind,
    MirOwnershipSummary, MirSwitchArm, MirSwitchBinding, MirSwitchCase, MirTerminator, MirValue,
    MirValueId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirLoweringError {
    pub node_id: NodeId,
    pub message: String,
}

impl std::fmt::Display for MirLoweringError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "cannot lower MIR node '{}': {}",
            self.node_id.0, self.message
        )
    }
}

impl std::error::Error for MirLoweringError {}

/// Lower the currently supported expression/statement subset.
///
/// Supported forms are deliberately small: literals, local loads, unary and
/// binary expressions, calls, casts, binds, expression statements, returns,
/// and branch/match expressions with explicit MIR blocks. Copy-only tuple and
/// record construction/projection/update are also represented, as is the
/// materialized recursive tuple product `(string, i32)` ownership shape.
/// Direct local reads become explicit `Clone` nodes, while root drops become
/// explicit `Drop` nodes.  With a TypeDesc catalog, the narrow
/// ownership-safe record field move becomes `MoveProject`; general partial
/// moves and projected drops remain rejected until their residual contracts
/// are represented in MIR.
pub fn lower_body(body: &ResolvedBody) -> Result<MirFunction, Vec<MirLoweringError>> {
    lower_body_impl(body, None)
}

/// Lower a body with the checker-derived TypeDesc catalog available.  The
/// catalog is used only to choose an already-defined ownership operation;
/// consumers still validate the resulting MIR before execution.
pub fn lower_body_with_type_catalog(
    body: &ResolvedBody,
    type_catalog: &MirTypeCatalog,
) -> Result<MirFunction, Vec<MirLoweringError>> {
    lower_body_impl(body, Some(type_catalog))
}

fn lower_body_impl(
    body: &ResolvedBody,
    type_catalog: Option<&MirTypeCatalog>,
) -> Result<MirFunction, Vec<MirLoweringError>> {
    let mut lowerer = Lowerer {
        body,
        type_catalog,
        values: BTreeMap::new(),
        locals: HashMap::new(),
        blocks: BTreeMap::new(),
        current: MirBlockId::new("bb.entry").expect("static MIR block id"),
        loops: Vec::new(),
        errors: Vec::new(),
    };
    let entry = lowerer.current.clone();
    lowerer.blocks.insert(
        entry.clone(),
        BlockDraft {
            id: entry.clone(),
            parameters: Vec::new(),
            instructions: Vec::new(),
            terminator: None,
        },
    );

    let mut parameters = Vec::with_capacity(body.parameters.len());
    for parameter in &body.parameters {
        let value = lowerer.local_value(parameter)?;
        parameters.push(value);
    }

    lowerer.lower_root(&body.root);
    if !lowerer.current_is_terminated() && lowerer.errors.is_empty() {
        if let Some(result) = body.root.result.as_deref() {
            let value = lowerer.lower_expr(result);
            if lowerer.errors.is_empty() {
                lowerer.terminate(MirTerminator::Return { value: Some(value) });
            }
        } else {
            lowerer.terminate(MirTerminator::Return { value: None });
        }
    }

    if !lowerer.errors.is_empty() {
        return Err(lowerer.errors);
    }

    let blocks = lowerer.finish_blocks();
    if !lowerer.errors.is_empty() {
        return Err(lowerer.errors);
    }
    let function = MirFunction {
        owner: body.owner.clone(),
        parameters,
        result: body.root.ty.clone(),
        entry: entry.clone(),
        values: lowerer.values,
        blocks,
        ownership: MirOwnershipSummary::default(),
    };
    function.validate().map_err(|errors| {
        errors
            .into_iter()
            .map(|error| MirLoweringError {
                node_id: body.owner.clone(),
                message: error.to_string(),
            })
            .collect::<Vec<_>>()
    })?;
    Ok(function)
}

/// Lower one checker-owned callable, including its canonical ownership facts.
/// This is the production entry point used by `lower_program` and tooling;
/// `lower_body` remains useful for focused lowering tests without a resource
/// analysis attachment.
pub fn lower_callable(
    callable: &crate::core::ResolvedCallable,
) -> Result<MirFunction, Vec<MirLoweringError>> {
    let mut function = lower_body(&callable.body)?;
    function.ownership = ownership_summary(&callable.resources);
    function.validate().map_err(|errors| {
        errors
            .into_iter()
            .map(|error| MirLoweringError {
                node_id: callable.owner.clone(),
                message: error.to_string(),
            })
            .collect::<Vec<_>>()
    })?;
    Ok(function)
}

/// Lower one callable with the canonical TypeDesc catalog.  This is the
/// production path for ownership-sensitive MIR shapes.
pub fn lower_callable_with_type_catalog(
    callable: &crate::core::ResolvedCallable,
    type_catalog: &MirTypeCatalog,
) -> Result<MirFunction, Vec<MirLoweringError>> {
    let mut function = lower_body_with_type_catalog(&callable.body, type_catalog)?;
    function.ownership = ownership_summary(&callable.resources);
    function.validate().map_err(|errors| {
        errors
            .into_iter()
            .map(|error| MirLoweringError {
                node_id: callable.owner.clone(),
                message: error.to_string(),
            })
            .collect::<Vec<_>>()
    })?;
    Ok(function)
}

/// Lower every checker-owned callable that has a ResolvedCallable body.
/// Errors are aggregated so callers can report the complete migration gap in
/// one pass instead of falling back one function at a time.
pub fn lower_program(
    program: &CheckedProgram,
) -> Result<BTreeMap<NodeId, MirFunction>, Vec<MirLoweringError>> {
    let mut lowered = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, callable) in program.callables() {
        match lower_callable(callable) {
            Ok(function) => {
                lowered.insert(owner.clone(), function);
            }
            Err(mut body_errors) => errors.append(&mut body_errors),
        }
    }
    if errors.is_empty() {
        Ok(lowered)
    } else {
        Err(errors)
    }
}

/// Lower every callable with the canonical TypeDesc catalog.  This is the
/// production entry point used by `MirProgram`; all ownership-sensitive
/// projection decisions therefore use checker-derived facts before backend
/// emission.
pub fn lower_program_with_type_catalog(
    program: &CheckedProgram,
    type_catalog: &MirTypeCatalog,
) -> Result<BTreeMap<NodeId, MirFunction>, Vec<MirLoweringError>> {
    let mut lowered = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, callable) in program.callables() {
        match lower_callable_with_type_catalog(callable, type_catalog) {
            Ok(function) => {
                lowered.insert(owner.clone(), function);
            }
            Err(mut body_errors) => errors.append(&mut body_errors),
        }
    }
    if errors.is_empty() {
        Ok(lowered)
    } else {
        Err(errors)
    }
}

fn ownership_summary(analysis: &ResourceAnalysis) -> MirOwnershipSummary {
    MirOwnershipSummary {
        events: analysis
            .actions
            .iter()
            .map(|action| MirOwnershipEvent {
                kind: match action.kind {
                    CanonicalActionKind::Read => MirOwnershipEventKind::Read,
                    CanonicalActionKind::Write => MirOwnershipEventKind::Write,
                    CanonicalActionKind::Introduce => MirOwnershipEventKind::Introduce,
                    CanonicalActionKind::Move => MirOwnershipEventKind::Move,
                    CanonicalActionKind::Drop => MirOwnershipEventKind::Drop,
                    CanonicalActionKind::Return => MirOwnershipEventKind::Return,
                    CanonicalActionKind::TransferSession => MirOwnershipEventKind::TransferSession,
                    CanonicalActionKind::TransferChild => MirOwnershipEventKind::TransferChild,
                    CanonicalActionKind::BorrowShared => MirOwnershipEventKind::BorrowShared,
                    CanonicalActionKind::BorrowMut => MirOwnershipEventKind::BorrowMut,
                    CanonicalActionKind::BorrowEnd => MirOwnershipEventKind::BorrowEnd,
                },
                resource: action.resource.0 .0.clone(),
                value: action
                    .resource
                    .0
                     .0
                    .ends_with("/local")
                    .then(|| MirValueId::new(format!("local:{}", action.resource.0 .0)))
                    .and_then(Result::ok),
                source: action.source.as_ref().map(|place| place.display()),
                target: action.target.as_ref().map(|place| place.display()),
                point: action.location.point.clone(),
            })
            .collect(),
    }
}

struct BlockDraft {
    id: MirBlockId,
    parameters: Vec<MirBlockParameter>,
    instructions: Vec<MirInstruction>,
    terminator: Option<MirTerminator>,
}

struct LoopTargets {
    header: MirBlockId,
    exit: MirBlockId,
}

struct Lowerer<'a> {
    body: &'a ResolvedBody,
    type_catalog: Option<&'a MirTypeCatalog>,
    values: BTreeMap<MirValueId, MirValue>,
    locals: HashMap<ResolvedLocalId, MirValueId>,
    blocks: BTreeMap<MirBlockId, BlockDraft>,
    current: MirBlockId,
    loops: Vec<LoopTargets>,
    errors: Vec<MirLoweringError>,
}

impl<'a> Lowerer<'a> {
    fn error(&mut self, node_id: &NodeId, message: impl Into<String>) {
        self.errors.push(MirLoweringError {
            node_id: node_id.clone(),
            message: message.into(),
        });
    }

    fn id(&mut self, prefix: &str, node_id: &NodeId) -> Option<MirValueId> {
        match super::MirValueId::new(format!("{prefix}:{}", node_id.0)) {
            Ok(id) => Some(id),
            Err(error) => {
                self.error(node_id, error.to_string());
                None
            }
        }
    }

    fn instruction_id(&mut self, node_id: &NodeId, role: &str) -> Option<MirInstructionId> {
        match super::MirInstructionId::new(format!("inst:{role}:{}", node_id.0)) {
            Ok(id) => Some(id),
            Err(error) => {
                self.error(node_id, error.to_string());
                None
            }
        }
    }

    fn current_is_terminated(&self) -> bool {
        self.blocks
            .get(&self.current)
            .and_then(|block| block.terminator.as_ref())
            .is_some()
    }

    fn block_id(&mut self, role: &str, node_id: &NodeId) -> Option<MirBlockId> {
        match MirBlockId::new(format!("bb:{role}:{}", node_id.0)) {
            Ok(id) => Some(id),
            Err(error) => {
                self.error(node_id, error.to_string());
                None
            }
        }
    }

    fn edge_id(&mut self, role: &str, node_id: &NodeId) -> Option<MirEdgeId> {
        match MirEdgeId::new(format!("edge:{role}:{}", node_id.0)) {
            Ok(id) => Some(id),
            Err(error) => {
                self.error(node_id, error.to_string());
                None
            }
        }
    }

    fn add_block(&mut self, id: MirBlockId, parameters: Vec<MirBlockParameter>) {
        if self.blocks.contains_key(&id) {
            self.error(
                &NodeId(id.as_str().to_string()),
                "MIR block identity is generated more than once",
            );
            return;
        }
        self.blocks.insert(
            id.clone(),
            BlockDraft {
                id,
                parameters,
                instructions: Vec::new(),
                terminator: None,
            },
        );
    }

    fn switch_to(&mut self, id: MirBlockId) {
        if self.blocks.contains_key(&id) {
            self.current = id;
        } else {
            self.error(
                &NodeId(self.body.owner.0.clone()),
                "attempted to switch to a missing MIR block",
            );
        }
    }

    fn terminate(&mut self, terminator: MirTerminator) {
        let block_id = self.current.clone();
        let already_terminated = self
            .blocks
            .get(&block_id)
            .is_some_and(|block| block.terminator.is_some());
        if already_terminated {
            self.error(
                &NodeId(block_id.as_str().to_string()),
                "MIR block receives more than one terminator",
            );
        } else if let Some(block) = self.blocks.get_mut(&block_id) {
            block.terminator = Some(terminator);
        } else {
            self.error(
                &NodeId(block_id.as_str().to_string()),
                "cannot terminate a missing MIR block",
            );
        }
    }

    fn finish_blocks(&mut self) -> BTreeMap<MirBlockId, MirBlock> {
        let mut finished = BTreeMap::new();
        for (id, draft) in std::mem::take(&mut self.blocks) {
            let Some(terminator) = draft.terminator else {
                self.error(
                    &NodeId(id.as_str().to_string()),
                    "MIR block has no terminator",
                );
                continue;
            };
            finished.insert(
                id,
                MirBlock {
                    id: draft.id,
                    parameters: draft.parameters,
                    instructions: draft.instructions,
                    terminator,
                },
            );
        }
        finished
    }

    fn insert_value(&mut self, id: MirValueId, ty: crate::core::ResolvedTypeId, node: &NodeId) {
        if let Some(existing) = self.values.get(&id) {
            if existing.ty != ty {
                self.error(
                    node,
                    format!("value '{}' is assigned incompatible types", id),
                );
            }
            return;
        }
        self.values.insert(id.clone(), MirValue { id, ty });
    }

    fn local_value(
        &mut self,
        local: &ResolvedLocalId,
    ) -> Result<MirValueId, Vec<MirLoweringError>> {
        if let Some(value) = self.locals.get(local) {
            return Ok(value.clone());
        }
        let Some(definition) = self.body.locals.get(local) else {
            return Err(vec![MirLoweringError {
                node_id: local.0.clone(),
                message: "local is absent from ResolvedBody catalog".into(),
            }]);
        };
        let value = super::MirValueId::new(format!("local:{}", local.0 .0)).map_err(|error| {
            vec![MirLoweringError {
                node_id: local.0.clone(),
                message: error.to_string(),
            }]
        })?;
        self.insert_value(value.clone(), definition.ty.clone(), &local.0);
        self.locals.insert(local.clone(), value.clone());
        Ok(value)
    }

    fn lower_root(&mut self, root: &ResolvedBlock) {
        for statement in &root.statements {
            if self.current_is_terminated() {
                self.error(
                    &statement.node_id,
                    "statement appears after a terminating MIR instruction",
                );
                continue;
            }
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                } => {
                    let value = self.lower_expr(initializer);
                    if let ResolvedPatternKind::Binding { local, .. } = &pattern.kind {
                        if let Ok(destination) = self.local_value(local) {
                            self.emit(
                                &statement.node_id,
                                "bind",
                                MirInstructionKind::Move {
                                    result: destination,
                                    source: value,
                                },
                            );
                        }
                    } else {
                        self.error(
                            &statement.node_id,
                            "only a direct binding pattern is supported in MIR Phase 0",
                        );
                    }
                }
                ResolvedStmtKind::Bind { .. } => {
                    self.error(
                        &statement.node_id,
                        "uninitialized bind is not in MIR Phase 0",
                    );
                }
                ResolvedStmtKind::Expr(expression) => {
                    let _ = self.lower_expr(expression);
                }
                ResolvedStmtKind::Return { value, .. } => {
                    let value = value.as_ref().map(|value| self.lower_expr(value));
                    self.terminate(MirTerminator::Return { value });
                }
                ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_) => {}
                ResolvedStmtKind::While { condition, body } => {
                    self.lower_while_stmt(&statement.node_id, condition, body);
                }
                ResolvedStmtKind::Break(value) => {
                    self.lower_break(&statement.node_id, value.as_ref());
                }
                ResolvedStmtKind::Continue => {
                    self.lower_continue(&statement.node_id);
                }
                ResolvedStmtKind::Drop(places) => {
                    for (index, place) in places.iter().enumerate() {
                        if !place.projections.is_empty() {
                            self.error(
                                &statement.node_id,
                                "projected drop requires aggregate glue and remains fail-closed",
                            );
                            continue;
                        }
                        match self.local_value(&place.base) {
                            Ok(value) => self.emit(
                                &statement.node_id,
                                &format!("drop.{index}"),
                                MirInstructionKind::Drop { value },
                            ),
                            Err(errors) => self.errors.extend(errors),
                        }
                    }
                }
                _ => self.error(
                    &statement.node_id,
                    "structured control flow is not lowered by MIR Phase 0",
                ),
            }
        }
    }

    fn lower_expr(&mut self, expression: &ResolvedExpr) -> MirValueId {
        let Some(result) = self.id("expr", &expression.node_id) else {
            return self.fallback_value(expression);
        };
        self.insert_value(result.clone(), expression.ty.clone(), &expression.node_id);
        match &expression.kind {
            ResolvedExprKind::Literal(literal) => {
                self.emit(
                    &expression.node_id,
                    "const",
                    MirInstructionKind::Const {
                        result: result.clone(),
                        literal: literal.clone(),
                    },
                );
            }
            ResolvedExprKind::Constant(item) if item.0.as_str() == "builtin:value:None" => {
                let move_owned = self.type_catalog.is_some_and(|catalog| {
                    catalog.get(&expression.ty).is_some_and(|descriptor| {
                        descriptor.ownership != super::types::MirOwnership::Copy
                    })
                });
                let instruction = if move_owned {
                    MirInstructionKind::ConstructVariantMove {
                        result: result.clone(),
                        nominal: NominalTypeId::new("builtin:type:Option")
                            .expect("static Option nominal"),
                        variant: NodeId("builtin:variant:Option::None".into()),
                        fields: Vec::new(),
                    }
                } else {
                    MirInstructionKind::ConstructVariant {
                        result: result.clone(),
                        nominal: NominalTypeId::new("builtin:type:Option")
                            .expect("static Option nominal"),
                        variant: NodeId("builtin:variant:Option::None".into()),
                        fields: Vec::new(),
                    }
                };
                self.emit(&expression.node_id, "construct_variant", instruction);
            }
            ResolvedExprKind::Load(place) => {
                if let Ok(local) = self.local_value(&place.base) {
                    if place.projections.is_empty() {
                        self.emit(
                            &expression.node_id,
                            "clone",
                            MirInstructionKind::Clone {
                                result: result.clone(),
                                source: local,
                            },
                        );
                    } else if let Some(projection) =
                        self.move_projection_for_place(&local, &expression.ty, place)
                    {
                        self.emit(
                            &expression.node_id,
                            "move_project",
                            MirInstructionKind::MoveProject {
                                result: result.clone(),
                                base: local,
                                projection,
                            },
                        );
                    } else {
                        self.emit(
                            &expression.node_id,
                            "load",
                            MirInstructionKind::Load {
                                result: result.clone(),
                                place: place.clone(),
                            },
                        );
                    }
                } else {
                    self.error(
                        &expression.node_id,
                        "load base is absent from ResolvedBody local catalog",
                    );
                }
            }
            ResolvedExprKind::Unary { op, operand } => {
                let operand = self.lower_expr(operand);
                if matches!(
                    op,
                    ResolvedUnaryOp::BorrowShared | ResolvedUnaryOp::BorrowMutable
                ) {
                    self.emit(
                        &expression.node_id,
                        "borrow",
                        MirInstructionKind::Borrow {
                            result: result.clone(),
                            source: operand,
                            mutable: matches!(op, ResolvedUnaryOp::BorrowMutable),
                        },
                    );
                } else {
                    self.emit(
                        &expression.node_id,
                        "unary",
                        MirInstructionKind::Unary {
                            result: result.clone(),
                            op: *op,
                            operand,
                        },
                    );
                }
            }
            ResolvedExprKind::Binary { op, left, right } => {
                let left = self.lower_expr(left);
                let right = self.lower_expr(right);
                self.emit(
                    &expression.node_id,
                    "binary",
                    MirInstructionKind::Binary {
                        result: result.clone(),
                        op: *op,
                        left,
                        right,
                    },
                );
            }
            ResolvedExprKind::Project { value, projection } => {
                let base = self.lower_expr(value);
                let projection = match projection {
                    crate::core::ir::ResolvedValueProjection::Field(field) => {
                        super::MirProjection::Field(field.clone())
                    }
                    crate::core::ir::ResolvedValueProjection::Tuple(index) => {
                        super::MirProjection::Tuple(*index)
                    }
                    crate::core::ir::ResolvedValueProjection::Index(index) => {
                        let index = self.lower_expr(index);
                        super::MirProjection::Index(index)
                    }
                    crate::core::ir::ResolvedValueProjection::Dereference => {
                        super::MirProjection::Dereference
                    }
                };
                if self.can_move_project(&base, &result, &projection) {
                    self.emit(
                        &expression.node_id,
                        "move_project",
                        MirInstructionKind::MoveProject {
                            result: result.clone(),
                            base,
                            projection,
                        },
                    );
                } else {
                    self.emit(
                        &expression.node_id,
                        "project",
                        MirInstructionKind::Project {
                            result: result.clone(),
                            base,
                            projection,
                        },
                    );
                }
            }
            ResolvedExprKind::Tuple(elements) => {
                let fields = elements
                    .iter()
                    .map(|element| self.lower_expr(element))
                    .collect();
                self.emit(
                    &expression.node_id,
                    "construct",
                    MirInstructionKind::Construct {
                        result: result.clone(),
                        kind: MirAggregateKind::Tuple,
                        fields,
                    },
                );
            }
            ResolvedExprKind::Record {
                nominal,
                fields,
                rest,
            } => {
                let values = fields
                    .iter()
                    .map(|field| self.lower_expr(&field.value))
                    .collect();
                let kind = MirAggregateKind::Record {
                    nominal: nominal.clone(),
                    fields: fields.iter().map(|field| field.field.clone()).collect(),
                };
                if let Some(rest) = rest {
                    let base = self.lower_expr(rest);
                    self.emit(
                        &expression.node_id,
                        "update_record",
                        MirInstructionKind::UpdateRecord {
                            result: result.clone(),
                            base,
                            kind,
                            fields: values,
                        },
                    );
                } else {
                    self.emit(
                        &expression.node_id,
                        "construct",
                        MirInstructionKind::Construct {
                            result: result.clone(),
                            kind,
                            fields: values,
                        },
                    );
                }
            }
            ResolvedExprKind::Call(call) => {
                let arguments: Vec<MirValueId> = call
                    .arguments
                    .iter()
                    .map(|argument| self.lower_expr(&argument.value))
                    .collect();
                if let Some((nominal, variant, field_ids)) = builtin_variant(call) {
                    if field_ids.len() != arguments.len() {
                        self.error(
                            &expression.node_id,
                            format!(
                                "variant constructor carries {} payloads but its canonical schema expects {}",
                                arguments.len(),
                                field_ids.len()
                            ),
                        );
                    }
                    let fields = field_ids.into_iter().zip(arguments).collect();
                    let move_owned = self.type_catalog.is_some_and(|catalog| {
                        catalog.get(&expression.ty).is_some_and(|descriptor| {
                            descriptor.ownership != super::types::MirOwnership::Copy
                        })
                    });
                    let instruction = if move_owned {
                        MirInstructionKind::ConstructVariantMove {
                            result: result.clone(),
                            nominal,
                            variant,
                            fields,
                        }
                    } else {
                        MirInstructionKind::ConstructVariant {
                            result: result.clone(),
                            nominal,
                            variant,
                            fields,
                        }
                    };
                    self.emit(&expression.node_id, "construct_variant", instruction);
                } else if let Some(contract) = call_builtin_contract(call) {
                    self.emit(
                        &expression.node_id,
                        "builtin_call",
                        MirInstructionKind::BuiltinCall {
                            result: result.clone(),
                            kind: contract.kind,
                            arguments,
                        },
                    );
                } else {
                    self.emit(
                        &expression.node_id,
                        "call",
                        MirInstructionKind::Call {
                            result: Some(result.clone()),
                            callee: call.callee.clone(),
                            arguments,
                        },
                    );
                }
            }
            ResolvedExprKind::Cast { value, .. } => {
                let source = self.lower_expr(value);
                self.emit(
                    &expression.node_id,
                    "convert",
                    MirInstructionKind::Convert {
                        result: result.clone(),
                        source,
                    },
                );
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.lower_if_expr(
                    &expression.node_id,
                    result.clone(),
                    condition,
                    then_block,
                    else_block,
                );
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.lower_match_expr(&expression.node_id, result.clone(), scrutinee, arms);
            }
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                if let Some(value) = self.lower_block_expr(block) {
                    self.emit(
                        &expression.node_id,
                        "block",
                        MirInstructionKind::Move {
                            result: result.clone(),
                            source: value,
                        },
                    );
                }
            }
            _ => self.error(
                &expression.node_id,
                "expression shape is not lowered by MIR Phase 0",
            ),
        }
        result
    }

    fn lower_match_expr(
        &mut self,
        node: &NodeId,
        result: MirValueId,
        scrutinee: &ResolvedExpr,
        arms: &[crate::core::ir::MatchArm],
    ) {
        if arms.is_empty() {
            self.error(node, "match expression has no arms");
            return;
        }
        let consume_scrutinee = self.type_catalog.is_some_and(|catalog| {
            catalog
                .get(&scrutinee.ty)
                .is_some_and(|descriptor| descriptor.ownership != super::types::MirOwnership::Copy)
        });
        let scrutinee = if consume_scrutinee {
            self.lower_consuming_expr(scrutinee)
        } else {
            self.lower_expr(scrutinee)
        };
        let Some(join_id) = self.block_id("match.join", node) else {
            return;
        };
        self.add_block(
            join_id.clone(),
            vec![MirBlockParameter {
                value: result.clone(),
            }],
        );

        let mut switch_arms = Vec::with_capacity(arms.len());
        let mut arm_blocks = Vec::with_capacity(arms.len());
        for arm in arms {
            let Some(case) = self.lower_switch_case(&arm.pattern.kind, &arm.node_id) else {
                continue;
            };
            if arm.guard.is_some() {
                self.error(
                    &arm.node_id,
                    "match guards require CFG lowering and are deferred to a later MIR phase",
                );
                continue;
            }
            let Some(block_id) = self.block_id("match.arm", &arm.node_id) else {
                continue;
            };
            let Some(edge) = self.edge_id("match.arm", &arm.node_id) else {
                continue;
            };
            let Some(join_edge) = self.edge_id("match.arm.join", &arm.node_id) else {
                continue;
            };
            let Some(bindings) = self.lower_switch_bindings(&arm.pattern, &arm.node_id) else {
                continue;
            };
            self.add_block(
                block_id.clone(),
                bindings
                    .iter()
                    .map(|binding| MirBlockParameter {
                        value: binding.parameter.clone(),
                    })
                    .collect(),
            );
            switch_arms.push(MirSwitchArm {
                edge,
                target: block_id.clone(),
                arguments: Vec::new(),
                bindings,
                case,
            });
            arm_blocks.push((block_id, join_edge, &arm.body));
        }
        if switch_arms.is_empty() {
            self.error(node, "match has no MIR-lowerable arms");
            return;
        }
        let terminator = if consume_scrutinee {
            MirTerminator::SwitchMove {
                scrutinee: scrutinee.clone(),
                arms: switch_arms,
            }
        } else {
            MirTerminator::Switch {
                scrutinee: scrutinee.clone(),
                arms: switch_arms,
            }
        };
        self.terminate(terminator);

        for (block_id, join_edge, body) in arm_blocks {
            self.switch_to(block_id);
            let value = self.lower_expr(body);
            if !self.current_is_terminated() {
                self.terminate(MirTerminator::Goto {
                    edge: join_edge,
                    target: join_id.clone(),
                    arguments: vec![value],
                });
            }
        }
        self.switch_to(join_id);
    }

    /// Lower a match scrutinee that is consumed by `SwitchMove`.  Direct local
    /// loads are moved into the terminator value so the original local is not
    /// silently cloned and left alive beside the consumed variant.  Rvalues
    /// and already ownership-aware projections keep their ordinary lowering;
    /// their result is fresh and is consumed by the terminator.
    fn lower_consuming_expr(&mut self, expression: &ResolvedExpr) -> MirValueId {
        if let ResolvedExprKind::Load(place) = &expression.kind {
            if place.projections.is_empty() {
                let Some(result) = self.id("expr", &expression.node_id) else {
                    return self.fallback_value(expression);
                };
                self.insert_value(result.clone(), expression.ty.clone(), &expression.node_id);
                match self.local_value(&place.base) {
                    Ok(source) => self.emit(
                        &expression.node_id,
                        "move",
                        MirInstructionKind::Move {
                            result: result.clone(),
                            source,
                        },
                    ),
                    Err(errors) => self.errors.extend(errors),
                }
                return result;
            }
        }
        self.lower_expr(expression)
    }

    fn lower_switch_bindings(
        &mut self,
        pattern: &ResolvedPattern,
        node: &NodeId,
    ) -> Option<Vec<MirSwitchBinding>> {
        let ResolvedPatternKind::Constructor { fields, .. } = &pattern.kind else {
            return Some(Vec::new());
        };
        let mut bindings = Vec::new();
        for (field, payload) in fields {
            match &payload.kind {
                ResolvedPatternKind::Wildcard => {}
                ResolvedPatternKind::Binding {
                    local,
                    by_reference: None,
                } => {
                    let parameter = match self.local_value(local) {
                        Ok(value) => value,
                        Err(errors) => {
                            self.errors.extend(errors);
                            return None;
                        }
                    };
                    bindings.push(MirSwitchBinding {
                        parameter,
                        field: field.clone(),
                    });
                }
                ResolvedPatternKind::Binding { .. } => {
                    self.error(
                        node,
                        "variant payload reference bindings require ownership lowering",
                    );
                    return None;
                }
                _ => {
                    self.error(
                        node,
                        "nested variant payload patterns require destructuring MIR lowering",
                    );
                    return None;
                }
            }
        }
        Some(bindings)
    }

    fn lower_switch_case(
        &mut self,
        pattern: &ResolvedPatternKind,
        node: &NodeId,
    ) -> Option<MirSwitchCase> {
        match pattern {
            ResolvedPatternKind::Wildcard => Some(MirSwitchCase::Default),
            ResolvedPatternKind::Literal(literal) => Some(MirSwitchCase::Literal(literal.clone())),
            ResolvedPatternKind::Constructor { variant, .. } => {
                Some(MirSwitchCase::Variant(variant.clone()))
            }
            _ => {
                self.error(
                    node,
                    "tuple/array/slice patterns require destructuring MIR lowering",
                );
                None
            }
        }
    }

    fn lower_if_expr(
        &mut self,
        node: &NodeId,
        result: MirValueId,
        condition: &ResolvedExpr,
        then_block: &ResolvedBlock,
        else_block: &ResolvedBlock,
    ) {
        let condition = self.lower_expr(condition);
        let Some(then_id) = self.block_id("if.then", node) else {
            return;
        };
        let Some(else_id) = self.block_id("if.else", node) else {
            return;
        };
        let Some(join_id) = self.block_id("if.join", node) else {
            return;
        };
        let Some(then_edge) = self.edge_id("if.then", node) else {
            return;
        };
        let Some(else_edge) = self.edge_id("if.else", node) else {
            return;
        };
        let Some(then_join_edge) = self.edge_id("if.then.join", node) else {
            return;
        };
        let Some(else_join_edge) = self.edge_id("if.else.join", node) else {
            return;
        };

        self.add_block(then_id.clone(), Vec::new());
        self.add_block(else_id.clone(), Vec::new());
        self.add_block(
            join_id.clone(),
            vec![MirBlockParameter {
                value: result.clone(),
            }],
        );
        self.terminate(MirTerminator::Branch {
            condition,
            then_edge,
            then_target: then_id.clone(),
            then_arguments: Vec::new(),
            else_edge,
            else_target: else_id.clone(),
            else_arguments: Vec::new(),
        });

        self.switch_to(then_id);
        let then_value = self.lower_block_expr(then_block);
        if !self.current_is_terminated() {
            match then_value.or_else(|| self.unit_branch_value(node, &result, then_block, "then")) {
                Some(value) => self.terminate(MirTerminator::Goto {
                    edge: then_join_edge,
                    target: join_id.clone(),
                    arguments: vec![value],
                }),
                None => self.error(node, "if then branch has no value"),
            }
        }

        self.switch_to(else_id);
        let else_value = self.lower_block_expr(else_block);
        if !self.current_is_terminated() {
            match else_value.or_else(|| self.unit_branch_value(node, &result, else_block, "else")) {
                Some(value) => self.terminate(MirTerminator::Goto {
                    edge: else_join_edge,
                    target: join_id.clone(),
                    arguments: vec![value],
                }),
                None => self.error(node, "if else branch has no value"),
            }
        }
        self.switch_to(join_id);
    }

    /// Lower a statement-shaped while into a canonical header/body/exit CFG.
    /// The header owns the condition so every back edge re-evaluates it; this
    /// is the key distinction from lowering a loop as a repeated AST walk.
    fn lower_while_stmt(&mut self, node: &NodeId, condition: &ResolvedExpr, body: &ResolvedBlock) {
        let Some(header_id) = self.block_id("while.header", node) else {
            return;
        };
        let Some(body_id) = self.block_id("while.body", node) else {
            return;
        };
        let Some(exit_id) = self.block_id("while.exit", node) else {
            return;
        };
        let Some(entry_edge) = self.edge_id("while.entry", node) else {
            return;
        };
        let Some(body_edge) = self.edge_id("while.body", node) else {
            return;
        };
        let Some(exit_edge) = self.edge_id("while.exit", node) else {
            return;
        };
        let Some(back_edge) = self.edge_id("while.back", node) else {
            return;
        };

        self.add_block(header_id.clone(), Vec::new());
        self.add_block(body_id.clone(), Vec::new());
        self.add_block(exit_id.clone(), Vec::new());
        self.terminate(MirTerminator::Goto {
            edge: entry_edge,
            target: header_id.clone(),
            arguments: Vec::new(),
        });

        self.switch_to(header_id.clone());
        let condition_value = self.lower_expr(condition);
        self.terminate(MirTerminator::Branch {
            condition: condition_value,
            then_edge: body_edge,
            then_target: body_id.clone(),
            then_arguments: Vec::new(),
            else_edge: exit_edge,
            else_target: exit_id.clone(),
            else_arguments: Vec::new(),
        });

        self.loops.push(LoopTargets {
            header: header_id.clone(),
            exit: exit_id.clone(),
        });
        self.switch_to(body_id);
        self.lower_block_expr(body);
        if !self.current_is_terminated() {
            self.terminate(MirTerminator::Goto {
                edge: back_edge,
                target: header_id,
                arguments: Vec::new(),
            });
        }
        self.loops.pop();
        self.switch_to(exit_id);
    }

    fn lower_break(&mut self, node: &NodeId, value: Option<&ResolvedExpr>) {
        if value.is_some() {
            self.error(node, "value-carrying break requires loop-result lowering");
            return;
        }
        let Some(exit) = self.loops.last().map(|targets| targets.exit.clone()) else {
            self.error(node, "break appears outside a loop");
            return;
        };
        let Some(edge) = self.edge_id("while.break", node) else {
            return;
        };
        self.terminate(MirTerminator::Goto {
            edge,
            target: exit,
            arguments: Vec::new(),
        });
    }

    fn lower_continue(&mut self, node: &NodeId) {
        let Some(header) = self.loops.last().map(|targets| targets.header.clone()) else {
            self.error(node, "continue appears outside a loop");
            return;
        };
        let Some(edge) = self.edge_id("while.continue", node) else {
            return;
        };
        self.terminate(MirTerminator::Goto {
            edge,
            target: header,
            arguments: Vec::new(),
        });
    }

    /// A statement-shaped `if` still has a value in canonical MIR: unit. The
    /// surface ResolvedBlock represents that as `result = None`, so materialize
    /// an explicit unit constant at each branch. We only accept it when the
    /// checker already gave the branch the same type as the enclosing result;
    /// malformed/non-unit omissions therefore remain fail-closed.
    fn unit_branch_value(
        &mut self,
        node: &NodeId,
        result: &MirValueId,
        branch: &ResolvedBlock,
        role: &str,
    ) -> Option<MirValueId> {
        let expected_ty = self.values.get(result).map(|value| value.ty.clone());
        if expected_ty.as_ref() != Some(&branch.ty) {
            self.error(node, format!("if {role} branch has no value"));
            return None;
        }
        let value = match MirValueId::new(format!("unit:{role}:{}", node.0)) {
            Ok(value) => value,
            Err(error) => {
                self.error(node, error.to_string());
                return None;
            }
        };
        self.insert_value(value.clone(), branch.ty.clone(), node);
        self.emit(
            node,
            &format!("if.{role}.unit"),
            MirInstructionKind::Const {
                result: value.clone(),
                literal: crate::core::ir::ResolvedLiteral::Unit,
            },
        );
        Some(value)
    }

    fn lower_block_expr(&mut self, block: &ResolvedBlock) -> Option<MirValueId> {
        for statement in &block.statements {
            if self.current_is_terminated() {
                self.error(
                    &statement.node_id,
                    "statement appears after a terminating MIR instruction",
                );
                continue;
            }
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                } => {
                    let value = self.lower_expr(initializer);
                    if let ResolvedPatternKind::Binding { local, .. } = &pattern.kind {
                        if let Ok(destination) = self.local_value(local) {
                            self.emit(
                                &statement.node_id,
                                "bind",
                                MirInstructionKind::Move {
                                    result: destination,
                                    source: value,
                                },
                            );
                        }
                    }
                }
                ResolvedStmtKind::Expr(expression) => {
                    self.lower_expr(expression);
                }
                ResolvedStmtKind::While { condition, body } => {
                    self.lower_while_stmt(&statement.node_id, condition, body);
                }
                ResolvedStmtKind::Break(value) => {
                    self.lower_break(&statement.node_id, value.as_ref());
                }
                ResolvedStmtKind::Continue => {
                    self.lower_continue(&statement.node_id);
                }
                ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_) => {}
                _ => self.error(
                    &statement.node_id,
                    "nested block statement is not lowered by MIR Phase 0",
                ),
            }
        }
        block
            .result
            .as_deref()
            .map(|result| self.lower_expr(result))
    }

    fn fallback_value(&mut self, expression: &ResolvedExpr) -> MirValueId {
        let value = MirValueId::new(format!("error:{}", expression.node_id.0))
            .unwrap_or_else(|_| MirValueId::new("error:fallback").expect("static MIR id"));
        self.insert_value(value.clone(), expression.ty.clone(), &expression.node_id);
        value
    }

    fn emit(&mut self, node: &NodeId, role: &str, kind: MirInstructionKind) {
        let Some(id) = self.instruction_id(node, role) else {
            return;
        };
        if let Some(block) = self.blocks.get_mut(&self.current) {
            block.instructions.push(MirInstruction { id, kind });
        } else {
            self.error(node, "cannot emit into a missing MIR block");
        }
    }

    fn can_move_project(
        &self,
        base: &MirValueId,
        result: &MirValueId,
        projection: &super::MirProjection,
    ) -> bool {
        let Some(type_catalog) = self.type_catalog else {
            return false;
        };
        let (Some(base_value), Some(result_value)) =
            (self.values.get(base), self.values.get(result))
        else {
            return false;
        };
        type_catalog
            .validate_move_projection(&base_value.ty, &result_value.ty, projection)
            .is_ok()
    }

    fn move_projection_for_place(
        &self,
        base: &MirValueId,
        result_ty: &crate::core::ResolvedTypeId,
        place: &crate::core::ResolvedPlace,
    ) -> Option<super::MirProjection> {
        let [projection] = place.projections.as_slice() else {
            return None;
        };
        let projection = match projection {
            crate::core::ir::ResolvedProjection::Field { field, .. } => {
                super::MirProjection::Field(field.clone())
            }
            _ => return None,
        };
        let type_catalog = self.type_catalog?;
        let base_ty = self.values.get(base)?.ty.clone();
        type_catalog
            .validate_move_projection(&base_ty, result_ty, &projection)
            .is_ok()
            .then_some(projection)
    }
}

fn builtin_variant(call: &ResolvedCall) -> Option<(NominalTypeId, NodeId, Vec<NodeId>)> {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    let (nominal, variant, fields) = match builtin.as_str() {
        "Some" => (
            "builtin:type:Option",
            "builtin:variant:Option::Some",
            vec![NodeId("builtin:variant:Option::Some/payload:0".into())],
        ),
        "None" => (
            "builtin:type:Option",
            "builtin:variant:Option::None",
            Vec::new(),
        ),
        "Ok" => (
            "builtin:type:Result",
            "builtin:variant:Result::Ok",
            vec![NodeId("builtin:variant:Result::Ok/payload:0".into())],
        ),
        "Err" => (
            "builtin:type:Result",
            "builtin:variant:Result::Err",
            vec![NodeId("builtin:variant:Result::Err/payload:0".into())],
        ),
        _ => return None,
    };
    Some((
        NominalTypeId::new(nominal).expect("static builtin nominal"),
        NodeId(variant.into()),
        fields,
    ))
}

fn call_builtin_contract(call: &ResolvedCall) -> Option<super::types::MirBuiltinContract> {
    let crate::core::ir::ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    super::types::MirBuiltinContract::from_builtin(builtin)
}

#[cfg(test)]
mod tests {
    use super::{lower_body, lower_body_with_type_catalog, lower_program};
    use crate::core::mir::types::MirTypeCatalog;
    use crate::core::mir::{MirInstructionKind, MirTerminator};
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    #[test]
    fn checked_scalar_function_lowers_without_backend_dependency() {
        let source = "func main() -> i32 { 40 + 2 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("MIR lowering");
        assert!(mir.validate().is_ok());
        assert!(mir.canonical_text().contains("binary"));
        let program_mir = lower_program(&program).expect("program MIR lowering");
        assert!(program_mir.contains_key(&callable.owner));
    }

    #[test]
    fn parameter_read_is_lowered_with_explicit_clone_identity() {
        let source = "func main(x: i32) -> i32 { x + 1 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("MIR lowering");
        assert!(mir.canonical_text().contains("clone"));
        assert!(mir.canonical_text().contains("local:"));
    }

    #[test]
    fn if_expression_lowers_to_branch_and_join_blocks() {
        let source = "func main() -> i32 { if true { 1 } else { 2 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("if lowering");
        assert_eq!(mir.blocks.len(), 4);
        let text = mir.canonical_text();
        assert!(text.contains("branch"));
        assert!(text.contains("bb:if.join"));
    }

    #[test]
    fn literal_match_lowers_to_switch_with_explicit_cases() {
        let source = "func main(x: i32) -> i32 { match x { 0 => 1, _ => 2 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("match lowering");
        assert_eq!(mir.blocks.len(), 4);
        let text = mir.canonical_text();
        assert!(text.contains("switch"));
        assert!(text.contains("Default"));
    }

    #[test]
    fn while_statement_lowers_to_header_body_exit_and_back_edge() {
        let source = "func main() -> i32 { while false { 1 } 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("while lowering");
        assert_eq!(mir.blocks.len(), 4);
        let text = mir.canonical_text();
        assert!(text.contains("bb:while.header"));
        assert!(text.contains("edge:while.back"));
    }

    #[test]
    fn tuple_literal_lowers_to_an_explicit_construct_instruction() {
        let source = "func main() -> (i32, i32) { (1, 2) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("tuple lowering");
        assert!(mir.canonical_text().contains("construct"));
    }

    #[test]
    fn record_projection_and_update_lower_to_explicit_mir_nodes() {
        let source = "type Point { x: i32, y: bool }\nfunc main() -> i32 { let p = Point { x: 1, y: true }; let q = Point { y: false, ..p }; Point { x: q.x, y: false }.x }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("record lowering");
        let text = mir.canonical_text();
        assert!(text.contains("construct"));
        assert!(text.contains("update_record"));
        assert!(text.contains("Field(NodeId"));
    }

    #[test]
    fn option_match_lowers_to_variant_construction_and_payload_binding() {
        let source =
            "func main() -> i32 { let value: Option<i32> = Some(41); match value { Some(v) => v, None => 0 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("Option MIR lowering");
        let text = mir.canonical_text();
        assert!(text.contains("construct_variant"));
        assert!(text.contains("Variant"));
        assert!(text.contains("bind="), "{text}");
        assert!(mir.validate().is_ok(), "{:?}", mir.validate());
    }

    #[test]
    fn non_copy_option_match_lowers_to_consuming_switch_move() {
        let source =
            "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(v) => v, None => \"fallback\" } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("TypeDesc");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body_with_type_catalog(&callable.body, &catalog)
            .expect("consuming Option match lowering");
        let switch = mir
            .blocks
            .values()
            .find_map(|block| match &block.terminator {
                MirTerminator::SwitchMove { arms, .. } => Some(arms),
                _ => None,
            })
            .expect("non-Copy match must use SwitchMove");
        assert_eq!(switch.len(), 2);
        assert_eq!(switch[0].bindings.len(), 1);
        assert!(mir
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| matches!(instruction.kind, MirInstructionKind::Move { .. })));
        assert!(mir.validate().is_ok(), "{:?}", mir.validate());
    }
}
