//! Surface-independent construction of typed callable bodies.
//!
//! This module is deliberately separate from `resolved`: the latter owns the
//! stable identity catalog, while this lowering consumes those identities and
//! checker-finalized types.  It must never infer a type or resolve a name.

use super::{
    CheckedConversion, CheckedConversionKind, EffectId, MatchArm as ResolvedMatchArm,
    ResolvedArgument, ResolvedBinaryOp, ResolvedBlock, ResolvedBody, ResolvedBodyError,
    ResolvedCall, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedLiteral, ResolvedLocal,
    ResolvedLocalId, ResolvedPattern, ResolvedPatternKind, ResolvedPlace, ResolvedProjection,
    ResolvedRecordField, ResolvedSignature, ResolvedStmt, ResolvedStmtKind, ResolvedType,
    ResolvedTypeId, ResolvedTypeTable, ResolvedUnaryOp,
};
use crate::ast::{
    AstOrigin, BinOp, Expr, File, FuncDef, Item, Lit, Pattern, PatternKind, Stmt, UnOp,
};
use crate::core::resolved::{
    expr_kind, expr_sibling_role, map_entry_role, match_arm_role, nested_function_owner,
    pattern_kind, pattern_sibling_role, stable_id_fragment, stmt_anchor, stmt_kind,
    stmt_sibling_role, NodeIdBuilder,
};
use crate::core::{
    CheckedProgram, NodeId, NodeMeta, Origin, ResolvedCallKind, ResolvedCallSite, ResolvedConstant,
    ResolvedExternBlock, ResolvedFunction, ResolvedTypeDef,
};
use crate::diagnostic::Diagnostic;
use crate::span::{SourceRegistry, Span};
use std::collections::{BTreeMap, HashMap};

const BLOCK_NORMALIZATION_RULE: &str = "resolved_body.structured_block";

/// Inputs already finalized by the checker and stable resolved walker.
pub struct FunctionBodyInput<'a> {
    pub function: &'a FuncDef,
    pub signature: &'a ResolvedSignature,
    pub signatures: &'a BTreeMap<NodeId, ResolvedSignature>,
    pub functions: &'a HashMap<NodeId, ResolvedFunction>,
    pub type_defs: &'a HashMap<NodeId, ResolvedTypeDef>,
    pub field_types: &'a BTreeMap<NodeId, ResolvedTypeId>,
    pub call_sites: &'a HashMap<NodeId, ResolvedCallSite>,
    pub extern_blocks: &'a HashMap<NodeId, ResolvedExternBlock>,
    pub constants: &'a HashMap<NodeId, ResolvedConstant>,
    pub node_types: &'a BTreeMap<NodeId, ResolvedTypeId>,
    pub types: &'a ResolvedTypeTable,
    pub node_meta: &'a HashMap<NodeId, NodeMeta>,
    pub sources: &'a SourceRegistry,
}

/// Lower the structural core of a checked function without re-running name or
/// type resolution. Unsupported constructs are errors, never omitted nodes.
pub fn lower_function_body(
    input: FunctionBodyInput<'_>,
) -> Result<ResolvedBody, Vec<ResolvedBodyError>> {
    let unit = input
        .types
        .iter()
        .find_map(|(id, ty)| {
            matches!(ty, ResolvedType::Primitive(super::PrimitiveType::Unit)).then(|| id.clone())
        })
        .ok_or_else(|| {
            vec![ResolvedBodyError::new(
                input.signature.owner.clone(),
                "canonical type table has no unit type",
            )]
        })?;
    let mut lowerer = BodyLowerer {
        owner: input.signature.owner.clone(),
        fallback: input.function.meta.span,
        signature: input.signature,
        signatures: input.signatures,
        functions: input.functions,
        type_defs: input.type_defs,
        field_types: input.field_types,
        call_sites: input.call_sites,
        extern_blocks: input.extern_blocks,
        constants: input.constants,
        node_types: input.node_types,
        types: input.types,
        node_meta: input.node_meta,
        ids: NodeIdBuilder::new(input.sources),
        unit,
        locals: BTreeMap::new(),
        place_inputs: BTreeMap::new(),
        scopes: vec![BTreeMap::new()],
    };
    lowerer.install_parameters()?;
    let root = lowerer.lower_block(
        &input.function.body,
        "body",
        input.signature.result.clone(),
        true,
    )?;
    let body = ResolvedBody {
        owner: input.signature.owner.clone(),
        locals: lowerer.locals,
        place_inputs: lowerer.place_inputs,
        root,
    };
    body.validate(input.types)?;
    Ok(body)
}

/// Lower every function currently represented by the canonical signature
/// catalog. The operation is transactional: no partial body map is returned.
pub fn lower_checked_function_bodies(
    file: &File,
    program: &CheckedProgram,
) -> Result<BTreeMap<NodeId, ResolvedBody>, Vec<ResolvedBodyError>> {
    let mut syntax = BTreeMap::new();
    collect_function_syntax(&file.items, "", &mut syntax);
    let mut bodies = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, signature) in program.resolved_signatures() {
        let Some(function) = syntax.get(owner) else {
            errors.push(ResolvedBodyError::new(
                owner.clone(),
                "canonical signature has no normalized function body",
            ));
            continue;
        };
        match lower_function_body(FunctionBodyInput {
            function,
            signature,
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        }) {
            Ok(body) => {
                bodies.insert(owner.clone(), body);
            }
            Err(mut body_errors) => errors.append(&mut body_errors),
        }
    }
    if errors.is_empty() {
        Ok(bodies)
    } else {
        Err(errors)
    }
}

fn collect_function_syntax<'a>(
    items: &'a [Item],
    module: &str,
    out: &mut BTreeMap<NodeId, &'a FuncDef>,
) {
    for item in items {
        match item {
            Item::Module(module_def) => {
                let qualified = if module.is_empty() {
                    module_def.name.clone()
                } else {
                    format!("{module}::{}", module_def.name)
                };
                collect_function_syntax(&module_def.items, &qualified, out);
            }
            Item::Func(function) => {
                let qualified = if module.is_empty() {
                    function.name.clone()
                } else {
                    format!("{module}::{}", function.name)
                };
                out.insert(NodeId(format!("function:{qualified}")), function);
            }
            _ => {}
        }
    }
}

struct BodyLowerer<'a> {
    owner: NodeId,
    fallback: Span,
    signature: &'a ResolvedSignature,
    signatures: &'a BTreeMap<NodeId, ResolvedSignature>,
    functions: &'a HashMap<NodeId, ResolvedFunction>,
    type_defs: &'a HashMap<NodeId, ResolvedTypeDef>,
    field_types: &'a BTreeMap<NodeId, ResolvedTypeId>,
    call_sites: &'a HashMap<NodeId, ResolvedCallSite>,
    extern_blocks: &'a HashMap<NodeId, ResolvedExternBlock>,
    constants: &'a HashMap<NodeId, ResolvedConstant>,
    node_types: &'a BTreeMap<NodeId, ResolvedTypeId>,
    types: &'a ResolvedTypeTable,
    node_meta: &'a HashMap<NodeId, NodeMeta>,
    ids: NodeIdBuilder<'a>,
    unit: ResolvedTypeId,
    locals: BTreeMap<ResolvedLocalId, ResolvedLocal>,
    place_inputs: BTreeMap<NodeId, ResolvedExpr>,
    scopes: Vec<BTreeMap<String, ResolvedLocalId>>,
}

impl BodyLowerer<'_> {
    fn install_parameters(&mut self) -> Result<(), Vec<ResolvedBodyError>> {
        if self.signature.parameters.is_empty() {
            return Ok(());
        }
        for parameter in &self.signature.parameters {
            let origin = self.origin(&parameter.id.0)?;
            let local_id = ResolvedLocalId(NodeId(format!("{}/local", parameter.id.0 .0)));
            self.insert_local(
                parameter.name.clone(),
                ResolvedLocal {
                    id: local_id,
                    display_name: parameter.name.clone(),
                    ty: parameter.ty.clone(),
                    mutable: parameter.mutable,
                    origin,
                },
                &parameter.id.0,
            )?;
        }
        Ok(())
    }

    fn lower_block(
        &mut self,
        block: &[Stmt],
        role: &str,
        block_ty: ResolvedTypeId,
        has_tail_result: bool,
    ) -> Result<ResolvedBlock, Vec<ResolvedBodyError>> {
        let (node_id, origin) = self.block_identity(role, block);
        self.scopes.push(BTreeMap::new());
        let tail_index = has_tail_result
            .then(|| block.len().checked_sub(1))
            .flatten()
            .filter(|index| matches!(block[*index].unlocated(), Stmt::Expr(_)));
        let mut statements = Vec::with_capacity(block.len());
        let mut result = None;
        for index in 0..block.len() {
            let stmt_role = stmt_sibling_role(role, block, index);
            if Some(index) == tail_index {
                let Stmt::Expr(expression) = block[index].unlocated() else {
                    unreachable!("tail index only selects expression statements")
                };
                let lowered = self.lower_expr(expression, &format!("{stmt_role}.expression"))?;
                if lowered.ty != block_ty {
                    self.scopes.pop();
                    return Err(vec![ResolvedBodyError::new(
                        lowered.node_id,
                        "tail expression type disagrees with checker-finalized block type",
                    )]);
                }
                result = Some(Box::new(lowered));
            } else if let Some(statement) = self.lower_stmt(&block[index], &stmt_role)? {
                statements.push(statement);
            }
        }
        self.scopes.pop();
        Ok(ResolvedBlock {
            node_id,
            origin,
            ty: block_ty,
            statements,
            result,
        })
    }

    fn lower_stmt(
        &mut self,
        stmt: &Stmt,
        role: &str,
    ) -> Result<Option<ResolvedStmt>, Vec<ResolvedBodyError>> {
        let node_id = self.stmt_id(stmt, role)?;
        let origin = self.origin(&node_id)?;
        let kind = match stmt.unlocated() {
            Stmt::Let {
                pat,
                init,
                mut_,
                ref_,
                ..
            } => {
                if *ref_ {
                    return self.unsupported(&node_id, "arena reference binding");
                }
                let initializer = init
                    .as_ref()
                    .map(|expr| self.lower_expr(expr, &format!("{role}.initializer")))
                    .transpose()?
                    .ok_or_else(|| {
                        vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "binding without an initializer has no checker-persisted value type",
                        )]
                    })?;
                let pattern = self.lower_binding_pattern(
                    pat,
                    &format!("{role}.pattern"),
                    initializer.ty.clone(),
                    *mut_,
                )?;
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                }
            }
            Stmt::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|expr| self.lower_expr(expr, &format!("{role}.value")))
                    .transpose()?;
                let conversion = value
                    .as_ref()
                    .map(|value| {
                        self.identity_conversion(&node_id, &value.ty, &self.signature.result)
                    })
                    .transpose()?;
                ResolvedStmtKind::Return { value, conversion }
            }
            Stmt::Break(value) => ResolvedStmtKind::Break(
                value
                    .as_ref()
                    .map(|expr| self.lower_expr(expr, &format!("{role}.value")))
                    .transpose()?,
            ),
            Stmt::Continue => ResolvedStmtKind::Continue,
            Stmt::Expr(expr) => {
                ResolvedStmtKind::Expr(self.lower_expr(expr, &format!("{role}.expression"))?)
            }
            Stmt::Assign { target, value } => {
                let place = self.lower_place(target, &format!("{role}.target"))?;
                let value = self.lower_expr(value, &format!("{role}.value"))?;
                let target_ty = self.place_type(&node_id, &place)?;
                let conversion = self.identity_conversion(&node_id, &value.ty, &target_ty)?;
                ResolvedStmtKind::Assign {
                    target: place,
                    value,
                    conversion,
                }
            }
            Stmt::If { cond, then_, else_ } => {
                let condition = self.lower_expr(cond, &format!("{role}.condition"))?;
                let then_block =
                    self.lower_block(then_, &format!("{role}.then"), self.unit.clone(), false)?;
                let else_block = self.lower_block(
                    else_.as_deref().unwrap_or_default(),
                    &format!("{role}.else"),
                    self.unit.clone(),
                    false,
                )?;
                let control_id = NodeId(format!("{}/control-expression", node_id.0));
                ResolvedStmtKind::Expr(ResolvedExpr {
                    node_id: control_id,
                    origin: origin.clone(),
                    ty: self.unit.clone(),
                    effects: Vec::new(),
                    backend_requirements: Vec::new(),
                    kind: ResolvedExprKind::If {
                        condition: Box::new(condition),
                        then_block: Box::new(then_block),
                        else_block: Box::new(else_block),
                    },
                })
            }
            Stmt::While { cond, body } => ResolvedStmtKind::While {
                condition: self.lower_expr(cond, &format!("{role}.condition"))?,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Loop(body) => ResolvedStmtKind::Loop(self.lower_block(
                body,
                &format!("{role}.body"),
                self.unit.clone(),
                false,
            )?),
            Stmt::For {
                var,
                iterable,
                body,
            } => {
                let iterable = self.lower_expr(iterable, &format!("{role}.iterable"))?;
                let element_ty = self.iterable_element_type(&node_id, &iterable.ty)?;
                let pattern_id = NodeId(format!("{}/for-pattern", node_id.0));
                let local_id = ResolvedLocalId(NodeId(format!("{}/local", pattern_id.0)));
                let pattern_origin = Origin::Desugared {
                    parent: node_id.clone(),
                    rule: "resolved_body.for_binding".into(),
                    span: origin.user_span(),
                };
                self.scopes.push(BTreeMap::new());
                self.insert_local(
                    var.clone(),
                    ResolvedLocal {
                        id: local_id.clone(),
                        display_name: var.clone(),
                        ty: element_ty.clone(),
                        mutable: false,
                        origin: pattern_origin.clone(),
                    },
                    &pattern_id,
                )?;
                let lowered_body =
                    self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false);
                self.scopes.pop();
                ResolvedStmtKind::For {
                    pattern: ResolvedPattern {
                        node_id: pattern_id,
                        origin: pattern_origin,
                        ty: element_ty,
                        kind: ResolvedPatternKind::Binding {
                            local: local_id,
                            by_reference: None,
                        },
                    },
                    iterable,
                    body: lowered_body?,
                }
            }
            Stmt::Block(body) | Stmt::Do(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Lexical,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Unsafe(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Unsafe,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::OnFailure(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::FailureGuard,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Arena(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Arena,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Parasteps(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Parallel,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Alloc { kind, body } => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Allocator(match kind {
                    crate::ast::AllocKind::System => super::AllocatorKind::System,
                    crate::ast::AllocKind::Arena => super::AllocatorKind::Arena,
                    crate::ast::AllocKind::Bump => super::AllocatorKind::Bump,
                }),
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Func(function) => {
                ResolvedStmtKind::NestedCallable(nested_function_owner(&self.owner, function))
            }
            Stmt::Requires(expr, _) => ResolvedStmtKind::Contract {
                kind: super::ContractKind::Requires,
                condition: self.lower_expr(expr, &format!("{role}.expression"))?,
            },
            Stmt::Ensures(expr, _) => ResolvedStmtKind::Contract {
                kind: super::ContractKind::Ensures,
                condition: self.lower_expr(expr, &format!("{role}.expression"))?,
            },
            Stmt::Invariant(expr, _) => ResolvedStmtKind::Contract {
                kind: super::ContractKind::Invariant,
                condition: self.lower_expr(expr, &format!("{role}.expression"))?,
            },
            Stmt::Drop(expr) => {
                ResolvedStmtKind::Drop(self.lower_place(expr, &format!("{role}.expression"))?)
            }
            Stmt::SharedLet {
                kind, name, init, ..
            } => {
                let value = self.lower_expr(init, &format!("{role}.initializer"))?;
                let local_ty = self.shared_binding_type(&node_id, *kind, &value.ty)?;
                let pattern_id = NodeId(format!("{}/shared-pattern", node_id.0));
                let local_id = ResolvedLocalId(NodeId(format!("{}/local", pattern_id.0)));
                let pattern_origin = Origin::Desugared {
                    parent: node_id.clone(),
                    rule: "resolved_body.shared_binding".into(),
                    span: origin.user_span(),
                };
                let initializer = ResolvedExpr {
                    node_id: NodeId(format!("{}/ownership-wrap", node_id.0)),
                    origin: pattern_origin.clone(),
                    ty: local_ty.clone(),
                    effects: Vec::new(),
                    backend_requirements: Vec::new(),
                    kind: ResolvedExprKind::Cast {
                        conversion: CheckedConversion {
                            kind: CheckedConversionKind::OwnershipWrap,
                            from: value.ty.clone(),
                            to: local_ty.clone(),
                        },
                        value: Box::new(value),
                    },
                };
                self.insert_local(
                    name.clone(),
                    ResolvedLocal {
                        id: local_id.clone(),
                        display_name: name.clone(),
                        ty: local_ty.clone(),
                        mutable: false,
                        origin: pattern_origin.clone(),
                    },
                    &pattern_id,
                )?;
                ResolvedStmtKind::Bind {
                    pattern: ResolvedPattern {
                        node_id: pattern_id,
                        origin: pattern_origin,
                        ty: local_ty,
                        kind: ResolvedPatternKind::Binding {
                            local: local_id,
                            by_reference: None,
                        },
                    },
                    initializer: Some(initializer),
                }
            }
            Stmt::Delegate { kind, expr, target } => {
                let source = self.lower_place(expr, &format!("{role}.expression"))?;
                let target = if let Some(local) = self.lookup_local(target) {
                    super::DelegateTarget::Local(local)
                } else {
                    let candidates = self
                        .functions
                        .values()
                        .filter(|function| {
                            function.qualified_name == *target
                                || function
                                    .qualified_name
                                    .rsplit_once("::")
                                    .is_some_and(|(_, short)| short == target)
                        })
                        .collect::<Vec<_>>();
                    let [function] = candidates.as_slice() else {
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            format!(
                                "delegate target '{target}' does not resolve to exactly one local or callable"
                            ),
                        )]);
                    };
                    super::DelegateTarget::Callable(function.node_id.clone())
                };
                ResolvedStmtKind::Delegate {
                    permission: match kind {
                        crate::ast::DelegateKind::View => super::Permission::View,
                        crate::ast::DelegateKind::Mutate => super::Permission::Mutate,
                        crate::ast::DelegateKind::Consume => super::Permission::Consume,
                    },
                    source,
                    target,
                }
            }
            Stmt::Desc(..) | Stmt::Rule(..) | Stmt::MmsBlock { .. } => return Ok(None),
            Stmt::WhileLet { .. } | Stmt::Math(_) | Stmt::Pinned { .. } | Stmt::Ellipsis => {
                return self.unsupported(&node_id, stmt_kind(stmt))
            }
            Stmt::Located { .. } => unreachable!("Stmt::unlocated returned Located"),
        };
        Ok(Some(ResolvedStmt {
            node_id,
            origin,
            ty: self.unit.clone(),
            backend_requirements: Vec::new(),
            kind,
        }))
    }

    fn lower_expr(
        &mut self,
        expr: &Expr,
        role: &str,
    ) -> Result<ResolvedExpr, Vec<ResolvedBodyError>> {
        let node_id = self.expr_id(expr, role)?;
        let origin = self.origin(&node_id)?;
        let ty = self.node_types.get(&node_id).cloned().ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                "expression has no checker-finalized canonical type",
            )]
        })?;
        let kind = match expr.unlocated() {
            Expr::Literal(literal) => {
                ResolvedExprKind::Literal(self.lower_literal(&node_id, literal)?)
            }
            Expr::Ident(name) => {
                if let Some(local) = self.lookup_local(name) {
                    ResolvedExprKind::Load(ResolvedPlace::root(local))
                } else if name == "None" {
                    ResolvedExprKind::Constant(NodeId("builtin:value:None".into()))
                } else {
                    let candidates = self
                        .constants
                        .values()
                        .filter(|constant| {
                            constant.qualified_name == *name
                                || constant
                                    .qualified_name
                                    .rsplit_once("::")
                                    .is_some_and(|(_, short)| short == name)
                        })
                        .collect::<Vec<_>>();
                    let [constant] = candidates.as_slice() else {
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            format!(
                                "identifier '{name}' does not resolve to exactly one local or constant"
                            ),
                        )]);
                    };
                    ResolvedExprKind::Constant(constant.node_id.clone())
                }
            }
            Expr::Binary(op, left, right) => ResolvedExprKind::Binary {
                op: self.lower_binary(&node_id, *op)?,
                left: Box::new(self.lower_expr(left, &format!("{role}.left"))?),
                right: Box::new(self.lower_expr(right, &format!("{role}.right"))?),
            },
            Expr::Unary(UnOp::Deref, _)
            | Expr::Field(_, _)
            | Expr::Index(_, _)
            | Expr::TupleIndex(_, _) => ResolvedExprKind::Load(self.lower_place(expr, role)?),
            Expr::Unary(op, operand) => ResolvedExprKind::Unary {
                op: self.lower_unary(*op),
                operand: Box::new(self.lower_expr(operand, &format!("{role}.inner"))?),
            },
            Expr::Tuple(values) => {
                ResolvedExprKind::Tuple(self.lower_expr_list(values, &format!("{role}.element"))?)
            }
            Expr::List(values) => {
                ResolvedExprKind::List(self.lower_expr_list(values, &format!("{role}.element"))?)
            }
            Expr::SetLiteral(values) => {
                ResolvedExprKind::Set(self.lower_expr_list(values, &format!("{role}.element"))?)
            }
            Expr::Block(block) => ResolvedExprKind::Block(Box::new(self.lower_block(
                block,
                &format!("{role}.block"),
                ty.clone(),
                true,
            )?)),
            Expr::If { cond, then_, else_ } => {
                let else_ = else_.as_ref().ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        "value-producing if expression has no else branch",
                    )]
                })?;
                ResolvedExprKind::If {
                    condition: Box::new(self.lower_expr(cond, &format!("{role}.condition"))?),
                    then_block: Box::new(self.lower_block(
                        then_,
                        &format!("{role}.then"),
                        ty.clone(),
                        true,
                    )?),
                    else_block: Box::new(self.lower_block(
                        else_,
                        &format!("{role}.else"),
                        ty.clone(),
                        true,
                    )?),
                }
            }
            Expr::Range { start, end } => ResolvedExprKind::Range {
                start: Box::new(self.lower_expr(start, &format!("{role}.start"))?),
                end: Box::new(self.lower_expr(end, &format!("{role}.end"))?),
            },
            Expr::SliceExpr { target, start, end } => ResolvedExprKind::Slice {
                target: Box::new(self.lower_expr(target, &format!("{role}.target"))?),
                start: start
                    .as_ref()
                    .map(|value| {
                        self.lower_expr(value, &format!("{role}.start"))
                            .map(Box::new)
                    })
                    .transpose()?,
                end: end
                    .as_ref()
                    .map(|value| self.lower_expr(value, &format!("{role}.end")).map(Box::new))
                    .transpose()?,
            },
            Expr::Spawn(value) => {
                ResolvedExprKind::Spawn(Box::new(self.lower_expr(value, &format!("{role}.inner"))?))
            }
            Expr::Await(value) => {
                ResolvedExprKind::Await(Box::new(self.lower_expr(value, &format!("{role}.inner"))?))
            }
            Expr::Call(_, arguments) => {
                ResolvedExprKind::Call(self.lower_call(&node_id, arguments, role)?)
            }
            Expr::Match(scrutinee, arms) => {
                let scrutinee = self.lower_expr(scrutinee, &format!("{role}.scrutinee"))?;
                let arms = self.lower_match_arms(arms, role, &scrutinee.ty, &ty)?;
                ResolvedExprKind::Match {
                    scrutinee: Box::new(scrutinee),
                    arms,
                }
            }
            Expr::Record { fields, .. } => self.lower_record(&node_id, fields, role, &ty)?,
            Expr::MapLiteral { entries } => {
                let mut lowered = Vec::with_capacity(entries.len());
                for index in 0..entries.len() {
                    let entry_role = map_entry_role(&format!("{role}.entry"), entries, index);
                    lowered.push((
                        self.lower_expr(&entries[index].0, &format!("{entry_role}.key"))?,
                        self.lower_expr(&entries[index].1, &format!("{entry_role}.value"))?,
                    ));
                }
                ResolvedExprKind::Map(lowered)
            }
            Expr::Try(value) => ResolvedExprKind::Try {
                value: Box::new(self.lower_expr(value, &format!("{role}.inner"))?),
                propagation_target: self.owner.clone(),
            },
            Expr::Cast(value, _) => {
                let value = self.lower_expr(value, &format!("{role}.inner"))?;
                let conversion = self.checked_explicit_conversion(&node_id, &value.ty, &ty)?;
                ResolvedExprKind::Cast {
                    value: Box::new(value),
                    conversion,
                }
            }
            Expr::Comprehension { .. }
            | Expr::OptionalChain(_, _)
            | Expr::Quote(_)
            | Expr::QuoteInterpolate(_)
            | Expr::Comptime(_)
            | Expr::TypeOf(_)
            | Expr::TypeInfo(_)
            | Expr::Lambda { .. }
            | Expr::Old(_)
            | Expr::Turbofish(_, _, _)
            | Expr::Arena(_)
            | Expr::NamedArg(_, _) => return self.unsupported(&node_id, expr_kind(expr)),
            Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
        };
        let effects = match &kind {
            ResolvedExprKind::Call(call) => call.effects.clone(),
            _ => Vec::new(),
        };
        Ok(ResolvedExpr {
            node_id,
            origin,
            ty,
            effects,
            backend_requirements: Vec::new(),
            kind,
        })
    }

    fn lower_record(
        &mut self,
        node_id: &NodeId,
        fields: &[crate::ast::RecordFieldExpr],
        role: &str,
        ty: &ResolvedTypeId,
    ) -> Result<ResolvedExprKind, Vec<ResolvedBodyError>> {
        let nominal = match self.types.get(ty) {
            Some(ResolvedType::Nominal { item, .. }) => item.clone(),
            _ => return self.unsupported(node_id, "record construction without nominal type"),
        };
        let owner = NodeId(nominal.as_str().to_string());
        let definition = self.type_defs.get(&owner).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("record owner '{}' has no resolved definition", owner.0),
            )]
        })?;
        let declared = match &definition.declaration.kind {
            crate::ast::TypeDefKind::Record(fields) => fields,
            _ => return self.unsupported(node_id, "non-record nominal construction"),
        };
        let mut surface = BTreeMap::new();
        for field in fields {
            if surface.insert(field.name.as_str(), field).is_some() {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("record field '{}' is supplied more than once", field.name),
                )]);
            }
        }
        let mut lowered = Vec::with_capacity(declared.len());
        for declaration in declared {
            let value = surface.remove(declaration.name.as_str()).ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("record field '{}' has no checked value", declaration.name),
                )]
            })?;
            let field_id = self.resolve_field(node_id, ty, &declaration.name)?;
            let target_ty = self.field_types.get(&field_id).ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    field_id.clone(),
                    "record field has no canonical declaration type",
                )]
            })?;
            let value = self.lower_expr(
                &value.value,
                &format!(
                    "{role}.field.{}.value",
                    stable_id_fragment(&declaration.name)
                ),
            )?;
            let conversion = self.identity_conversion(node_id, &value.ty, target_ty)?;
            lowered.push(ResolvedRecordField {
                field: field_id,
                value,
                conversion,
            });
        }
        if let Some(extra) = surface.keys().next() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("record field '{extra}' has no declaration"),
            )]);
        }
        Ok(ResolvedExprKind::Record {
            nominal,
            fields: lowered,
        })
    }

    fn lower_match_arms(
        &mut self,
        arms: &[crate::ast::MatchArm],
        role: &str,
        pattern_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
    ) -> Result<Vec<ResolvedMatchArm>, Vec<ResolvedBodyError>> {
        let mut lowered = Vec::with_capacity(arms.len());
        for index in 0..arms.len() {
            let arm = &arms[index];
            let arm_role = match_arm_role(&format!("{role}.arm"), arms, index);
            let mut diagnostics = Vec::new();
            let node_id = self.ids.anonymous(
                &self.owner,
                "match.arm",
                &arm_role,
                usable_span(arm.meta.span),
                arm.meta.origin,
                &mut diagnostics,
            );
            if !diagnostics.is_empty() || !self.node_meta.contains_key(&node_id) {
                return Err(vec![ResolvedBodyError::new(
                    node_id,
                    "match arm has no stable semantic identity",
                )]);
            }
            let origin = self.origin(&node_id)?;
            self.scopes.push(BTreeMap::new());
            let arm_result = (|| {
                let pattern = self.lower_binding_pattern(
                    &arm.pat,
                    &format!("{arm_role}.pattern"),
                    pattern_ty.clone(),
                    false,
                )?;
                let guard = arm
                    .guard
                    .as_ref()
                    .map(|guard| self.lower_expr(guard, &format!("{arm_role}.guard")))
                    .transpose()?;
                let body = self.lower_expr(&arm.body, &format!("{arm_role}.body"))?;
                if &body.ty != result_ty {
                    return Err(vec![ResolvedBodyError::new(
                        body.node_id.clone(),
                        "match arm body type disagrees with match result type",
                    )]);
                }
                Ok(ResolvedMatchArm {
                    node_id,
                    origin,
                    pattern,
                    guard,
                    body,
                })
            })();
            self.scopes.pop();
            lowered.push(arm_result?);
        }
        Ok(lowered)
    }

    fn lower_call(
        &mut self,
        node_id: &NodeId,
        arguments: &[Expr],
        role: &str,
    ) -> Result<ResolvedCall, Vec<ResolvedBodyError>> {
        let site = self.call_sites.get(node_id).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                "call has no checker-resolved call-site record",
            )]
        })?;
        if site.kind == ResolvedCallKind::Builtin {
            let builtin =
                super::BuiltinId::new(site.callee.clone()).map_err(|error| vec![error])?;
            let mut lowered = Vec::with_capacity(arguments.len());
            for index in 0..arguments.len() {
                if matches!(arguments[index].unlocated(), Expr::NamedArg(_, _)) {
                    return self.unsupported(node_id, "named arguments for builtin call");
                }
                let argument_role =
                    expr_sibling_role(&format!("{role}.argument"), arguments, index);
                let value = self.lower_expr(&arguments[index], &argument_role)?;
                lowered.push(ResolvedArgument {
                    parameter: super::ResolvedParameterId(NodeId(format!(
                        "builtin:{}/parameter:{index}",
                        site.callee
                    ))),
                    conversion: CheckedConversion {
                        kind: CheckedConversionKind::Identity,
                        from: value.ty.clone(),
                        to: value.ty.clone(),
                    },
                    value,
                });
            }
            return Ok(ResolvedCall {
                callee: ResolvedCallee::Builtin(builtin),
                arguments: lowered,
                permission: None,
                effects: Vec::new(),
                session: Vec::new(),
            });
        }
        if site.kind == ResolvedCallKind::Extern {
            let candidates = self
                .extern_blocks
                .values()
                .flat_map(|block| block.signatures.iter())
                .filter(|function| function.name == site.callee)
                .collect::<Vec<_>>();
            let [function] = candidates.as_slice() else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "extern call '{}' does not resolve to exactly one declaration",
                        site.callee
                    ),
                )]);
            };
            let arity_valid = if function.variadic {
                arguments.len() >= function.parameter_ids.len()
            } else {
                arguments.len() == function.parameter_ids.len()
            };
            if !arity_valid {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "extern argument count disagrees with canonical declaration",
                )]);
            }
            let mut lowered = Vec::with_capacity(arguments.len());
            for index in 0..arguments.len() {
                if matches!(arguments[index].unlocated(), Expr::NamedArg(_, _)) {
                    return self.unsupported(node_id, "named arguments for extern call");
                }
                let argument_role =
                    expr_sibling_role(&format!("{role}.argument"), arguments, index);
                let value = self.lower_expr(&arguments[index], &argument_role)?;
                let parameter = function
                    .parameter_ids
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| NodeId(format!("{}/variadic:{index}", function.node_id.0)));
                lowered.push(ResolvedArgument {
                    parameter: super::ResolvedParameterId(parameter),
                    conversion: CheckedConversion {
                        kind: CheckedConversionKind::Identity,
                        from: value.ty.clone(),
                        to: value.ty.clone(),
                    },
                    value,
                });
            }
            return Ok(ResolvedCall {
                callee: ResolvedCallee::Extern(function.node_id.clone()),
                arguments: lowered,
                permission: None,
                effects: Vec::new(),
                session: Vec::new(),
            });
        }
        if site.kind != ResolvedCallKind::Function {
            return self.unsupported(node_id, &format!("closed {:?} call target", site.kind));
        }
        let mut candidates = self
            .functions
            .values()
            .filter(|function| function.qualified_name == site.callee)
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let [function] = candidates.as_slice() else {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "function call '{}' does not resolve to exactly one callable identity",
                    site.callee
                ),
            )]);
        };
        let signature = self.signatures.get(&function.node_id).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("callee '{}' has no canonical signature", function.node_id.0),
            )]
        })?;
        if arguments.len() != signature.parameters.len() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "call argument count {} does not match canonical parameter count {}",
                    arguments.len(),
                    signature.parameters.len()
                ),
            )]);
        }

        let mut slots = vec![None; signature.parameters.len()];
        let mut next_positional = 0;
        for index in 0..arguments.len() {
            let argument_role = expr_sibling_role(&format!("{role}.argument"), arguments, index);
            let (slot, value, value_role) = match arguments[index].unlocated() {
                Expr::NamedArg(name, value) => {
                    let slot = signature
                        .parameters
                        .iter()
                        .position(|parameter| parameter.name == *name)
                        .ok_or_else(|| {
                            vec![ResolvedBodyError::new(
                                node_id.clone(),
                                format!("named argument '{name}' has no canonical parameter"),
                            )]
                        })?;
                    (slot, value.as_ref(), format!("{argument_role}.inner"))
                }
                _ => {
                    while next_positional < slots.len() && slots[next_positional].is_some() {
                        next_positional += 1;
                    }
                    let slot = next_positional;
                    next_positional += 1;
                    (slot, &arguments[index], argument_role)
                }
            };
            if slots[slot].replace((value, value_role)).is_some() {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "parameter '{}' is supplied more than once",
                        signature.parameters[slot].name
                    ),
                )]);
            }
        }

        let mut lowered = Vec::with_capacity(slots.len());
        for (parameter, slot) in signature.parameters.iter().zip(slots) {
            let (value, value_role) = slot.ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("parameter '{}' has no checked argument", parameter.name),
                )]
            })?;
            let value = self.lower_expr(value, &value_role)?;
            let conversion = self.identity_conversion(node_id, &value.ty, &parameter.ty)?;
            lowered.push(ResolvedArgument {
                parameter: parameter.id.clone(),
                value,
                conversion,
            });
        }
        let effects = site
            .effects
            .iter()
            .map(|effect| {
                EffectId::new(effect.clone()).map_err(|error| {
                    vec![ResolvedBodyError::new(node_id.clone(), error.to_string())]
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedCall {
            callee: ResolvedCallee::Function(function.node_id.clone()),
            arguments: lowered,
            permission: None,
            effects,
            session: Vec::new(),
        })
    }

    fn lower_expr_list(
        &mut self,
        values: &[Expr],
        role: &str,
    ) -> Result<Vec<ResolvedExpr>, Vec<ResolvedBodyError>> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| self.lower_expr(value, &expr_sibling_role(role, values, index)))
            .collect()
    }

    fn lower_binding_pattern(
        &mut self,
        pattern: &Pattern,
        role: &str,
        ty: ResolvedTypeId,
        mutable: bool,
    ) -> Result<ResolvedPattern, Vec<ResolvedBodyError>> {
        let node_id = self.pattern_id(pattern, role)?;
        let origin = self.origin(&node_id)?;
        let kind = match &pattern.kind {
            PatternKind::Wildcard => ResolvedPatternKind::Wildcard,
            PatternKind::Variable(name) => {
                let local_id = ResolvedLocalId(NodeId(format!("{}/local", node_id.0)));
                self.insert_local(
                    name.clone(),
                    ResolvedLocal {
                        id: local_id.clone(),
                        display_name: name.clone(),
                        ty: ty.clone(),
                        mutable,
                        origin: origin.clone(),
                    },
                    &node_id,
                )?;
                ResolvedPatternKind::Binding {
                    local: local_id,
                    by_reference: None,
                }
            }
            PatternKind::Literal(literal) => {
                ResolvedPatternKind::Literal(self.lower_literal(&node_id, literal)?)
            }
            PatternKind::Tuple(patterns) => {
                let element_types = match self.types.get(&ty) {
                    Some(ResolvedType::Tuple(elements)) if elements.len() == patterns.len() => {
                        elements.clone()
                    }
                    _ => {
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "tuple pattern shape disagrees with canonical scrutinee type",
                        )])
                    }
                };
                ResolvedPatternKind::Tuple(self.lower_pattern_list(
                    patterns,
                    role,
                    &element_types,
                    mutable,
                )?)
            }
            PatternKind::Array(patterns) => {
                let element = match self.types.get(&ty) {
                    Some(ResolvedType::Array { element, length }) if *length == patterns.len() => {
                        element.clone()
                    }
                    _ => {
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "array pattern shape disagrees with canonical scrutinee type",
                        )])
                    }
                };
                let element_types = vec![element; patterns.len()];
                ResolvedPatternKind::Array(self.lower_pattern_list(
                    patterns,
                    role,
                    &element_types,
                    mutable,
                )?)
            }
            PatternKind::Slice(patterns, rest) => {
                let element = match self.types.get(&ty) {
                    Some(ResolvedType::Array { element, .. })
                    | Some(ResolvedType::Slice(element)) => element.clone(),
                    _ => return self.unsupported(&node_id, "slice pattern on non-sequence type"),
                };
                let element_types = vec![element; patterns.len()];
                let prefix = self.lower_pattern_list(patterns, role, &element_types, mutable)?;
                let rest = rest
                    .as_ref()
                    .map(|rest| {
                        self.lower_binding_pattern(
                            rest,
                            &format!("{role}.rest"),
                            ty.clone(),
                            mutable,
                        )
                        .map(Box::new)
                    })
                    .transpose()?;
                ResolvedPatternKind::Slice { prefix, rest }
            }
            PatternKind::Constructor(_, _) => {
                return self.unsupported(&node_id, pattern_kind(pattern))
            }
        };
        Ok(ResolvedPattern {
            node_id,
            origin,
            ty,
            kind,
        })
    }

    fn lower_pattern_list(
        &mut self,
        patterns: &[Pattern],
        role: &str,
        types: &[ResolvedTypeId],
        mutable: bool,
    ) -> Result<Vec<ResolvedPattern>, Vec<ResolvedBodyError>> {
        patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                self.lower_binding_pattern(
                    pattern,
                    &pattern_sibling_role(&format!("{role}.element"), patterns, index),
                    types[index].clone(),
                    mutable,
                )
            })
            .collect()
    }

    fn lower_place(
        &mut self,
        expr: &Expr,
        role: &str,
    ) -> Result<ResolvedPlace, Vec<ResolvedBodyError>> {
        let node_id = self.expr_id(expr, role)?;
        match expr.unlocated() {
            Expr::Ident(name) => {
                self.lookup_local(name)
                    .map(ResolvedPlace::root)
                    .ok_or_else(|| {
                        vec![ResolvedBodyError::new(
                            node_id,
                            format!("place base '{name}' has no resolved local identity"),
                        )]
                    })
            }
            Expr::TupleIndex(base, index) => {
                let mut place = self.lower_place(base, &format!("{role}.inner"))?;
                let ty = self.expression_type(&node_id)?;
                place
                    .projections
                    .push(ResolvedProjection::Tuple { index: *index, ty });
                Ok(place)
            }
            Expr::Field(base, name) => {
                let mut place = self.lower_place(base, &format!("{role}.inner"))?;
                let base_ty = self.place_type(&node_id, &place)?;
                let field = self.resolve_field(&node_id, &base_ty, name)?;
                let ty = self.expression_type(&node_id)?;
                place
                    .projections
                    .push(ResolvedProjection::Field { field, ty });
                Ok(place)
            }
            Expr::Index(base, index) => {
                let mut place = self.lower_place(base, &format!("{role}.left"))?;
                let ty = self.expression_type(&node_id)?;
                let index = match index.unlocated() {
                    Expr::Literal(Lit::Int(value)) => super::ResolvedIndex::Constant(*value),
                    _ => {
                        let input = self.lower_expr(index, &format!("{role}.right"))?;
                        let input_id = input.node_id.clone();
                        if self.place_inputs.insert(input_id.clone(), input).is_some() {
                            return Err(vec![ResolvedBodyError::new(
                                input_id,
                                "dynamic place input identity collision",
                            )]);
                        }
                        super::ResolvedIndex::Dynamic(input_id)
                    }
                };
                place
                    .projections
                    .push(ResolvedProjection::Index { index, ty });
                Ok(place)
            }
            Expr::Unary(UnOp::Deref, base) => {
                let mut place = self.lower_place(base, &format!("{role}.inner"))?;
                let ty = self.expression_type(&node_id)?;
                place.projections.push(ResolvedProjection::Deref { ty });
                Ok(place)
            }
            _ => self.unsupported(&node_id, "projected place lowering"),
        }
    }

    fn expression_type(&self, node_id: &NodeId) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        self.node_types.get(node_id).cloned().ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                "place expression has no checker-finalized canonical type",
            )]
        })
    }

    fn resolve_field(
        &self,
        node_id: &NodeId,
        base_ty: &ResolvedTypeId,
        name: &str,
    ) -> Result<NodeId, Vec<ResolvedBodyError>> {
        let nominal = match self.types.get(base_ty) {
            Some(ResolvedType::Nominal { item, .. }) | Some(ResolvedType::Newtype { item, .. }) => {
                item
            }
            _ => return self.unsupported(node_id, "field projection on non-nominal type"),
        };
        let owner = NodeId(nominal.as_str().to_string());
        let definition = self.type_defs.get(&owner).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "nominal owner '{}' has no resolved type definition",
                    owner.0
                ),
            )]
        })?;
        let fields = match &definition.declaration.kind {
            crate::ast::TypeDefKind::Record(fields) | crate::ast::TypeDefKind::Union(fields) => {
                fields
            }
            _ => return self.unsupported(node_id, "field projection on non-record nominal type"),
        };
        let field = fields
            .iter()
            .find(|field| field.name == name)
            .ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("field '{name}' is absent from nominal owner '{}'", owner.0),
                )]
            })?;
        let mut diagnostics = Vec::new();
        let field_id = self.ids.anonymous(
            &owner,
            "decl.field",
            &format!("field.{}", stable_id_fragment(name)),
            usable_span(field.meta.span),
            field.meta.origin,
            &mut diagnostics,
        );
        if !diagnostics.is_empty() || !self.node_meta.contains_key(&field_id) {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("field '{name}' has no stable declaration identity"),
            )]);
        }
        Ok(field_id)
    }

    fn place_type(
        &self,
        node_id: &NodeId,
        place: &ResolvedPlace,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        self.locals
            .get(&place.base)
            .map(|local| place.projected_type(local).clone())
            .ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "place references an unknown local",
                )]
            })
    }

    fn iterable_element_type(
        &self,
        node_id: &NodeId,
        iterable: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        match self.types.get(iterable) {
            Some(ResolvedType::Array { element, .. }) | Some(ResolvedType::Slice(element)) => {
                Ok(element.clone())
            }
            Some(ResolvedType::Nominal { item, arguments })
                if matches!(
                    item.as_str(),
                    "builtin:type:List" | "builtin:type:Set" | "builtin:type:Range"
                ) && arguments.len() == 1 =>
            {
                Ok(arguments[0].clone())
            }
            _ => self.unsupported(node_id, "for loop over non-canonical iterable type"),
        }
    }

    fn shared_binding_type(
        &self,
        node_id: &NodeId,
        kind: crate::ast::SharedKind,
        initializer: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        let expected = match kind {
            crate::ast::SharedKind::Shared => super::OwnershipTypeKind::Shared,
            crate::ast::SharedKind::LocalShared => super::OwnershipTypeKind::LocalShared,
            crate::ast::SharedKind::Weak => super::OwnershipTypeKind::Weak,
            crate::ast::SharedKind::WeakLocal => super::OwnershipTypeKind::WeakLocal,
        };
        let matches = self
            .types
            .iter()
            .filter_map(|(id, ty)| match ty {
                ResolvedType::Ownership { kind, target }
                    if *kind == expected && target == initializer =>
                {
                    Some(id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let [ty] = matches.as_slice() else {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "shared binding has no unique canonical ownership type",
            )]);
        };
        Ok(ty.clone())
    }

    fn lower_literal(
        &self,
        node_id: &NodeId,
        literal: &Lit,
    ) -> Result<ResolvedLiteral, Vec<ResolvedBodyError>> {
        Ok(match literal {
            Lit::Int(value) => ResolvedLiteral::Int(*value),
            Lit::Float(value) => ResolvedLiteral::float(*value),
            Lit::Bool(value) => ResolvedLiteral::Bool(*value),
            Lit::String(value) => ResolvedLiteral::String(value.clone()),
            Lit::Unit => ResolvedLiteral::Unit,
            Lit::FString(_) => return self.unsupported(node_id, "interpolated string literal"),
        })
    }

    fn lower_binary(
        &self,
        node_id: &NodeId,
        op: BinOp,
    ) -> Result<ResolvedBinaryOp, Vec<ResolvedBodyError>> {
        Ok(match op {
            BinOp::Add => ResolvedBinaryOp::Add,
            BinOp::Sub => ResolvedBinaryOp::Subtract,
            BinOp::Mul => ResolvedBinaryOp::Multiply,
            BinOp::Div => ResolvedBinaryOp::Divide,
            BinOp::Mod => ResolvedBinaryOp::Remainder,
            BinOp::Pow => ResolvedBinaryOp::Power,
            BinOp::EqCmp => ResolvedBinaryOp::Equal,
            BinOp::NeCmp => ResolvedBinaryOp::NotEqual,
            BinOp::Lt => ResolvedBinaryOp::Less,
            BinOp::Gt => ResolvedBinaryOp::Greater,
            BinOp::Le => ResolvedBinaryOp::LessEqual,
            BinOp::Ge => ResolvedBinaryOp::GreaterEqual,
            BinOp::And => ResolvedBinaryOp::LogicalAnd,
            BinOp::Or => ResolvedBinaryOp::LogicalOr,
            BinOp::BitAnd => ResolvedBinaryOp::BitAnd,
            BinOp::BitOr => ResolvedBinaryOp::BitOr,
            BinOp::BitXor => ResolvedBinaryOp::BitXor,
            BinOp::Shl => ResolvedBinaryOp::ShiftLeft,
            BinOp::Shr => ResolvedBinaryOp::ShiftRight,
            BinOp::Assign | BinOp::Range => return self.unsupported(node_id, "binary sugar"),
        })
    }

    fn lower_unary(&self, op: UnOp) -> ResolvedUnaryOp {
        match op {
            UnOp::Neg => ResolvedUnaryOp::Negate,
            UnOp::Not => ResolvedUnaryOp::Not,
            UnOp::Ref => ResolvedUnaryOp::BorrowShared,
            UnOp::RefMut => ResolvedUnaryOp::BorrowMutable,
            UnOp::Deref => ResolvedUnaryOp::Dereference,
        }
    }

    fn identity_conversion(
        &self,
        node_id: &NodeId,
        from: &ResolvedTypeId,
        to: &ResolvedTypeId,
    ) -> Result<CheckedConversion, Vec<ResolvedBodyError>> {
        if from != to {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "explicit checked conversion is required from '{}' to '{}'",
                    from.as_str(),
                    to.as_str()
                ),
            )]);
        }
        Ok(CheckedConversion {
            kind: CheckedConversionKind::Identity,
            from: from.clone(),
            to: to.clone(),
        })
    }

    fn checked_explicit_conversion(
        &self,
        node_id: &NodeId,
        from: &ResolvedTypeId,
        to: &ResolvedTypeId,
    ) -> Result<CheckedConversion, Vec<ResolvedBodyError>> {
        if from == to {
            return self.identity_conversion(node_id, from, to);
        }
        let numeric = |ty: &ResolvedTypeId| match self.types.get(ty) {
            Some(ResolvedType::Primitive(primitive)) => numeric_width(*primitive),
            _ => None,
        };
        let (from_width, to_width) = match (numeric(from), numeric(to)) {
            (Some(from), Some(to)) => (from, to),
            _ => {
                return self.unsupported(
                    node_id,
                    &format!(
                        "checked conversion from '{}' to '{}'",
                        from.as_str(),
                        to.as_str()
                    ),
                )
            }
        };
        Ok(CheckedConversion {
            kind: if to_width > from_width {
                CheckedConversionKind::NumericWiden
            } else {
                CheckedConversionKind::NumericNarrowChecked
            },
            from: from.clone(),
            to: to.clone(),
        })
    }

    fn insert_local(
        &mut self,
        name: String,
        local: ResolvedLocal,
        owner: &NodeId,
    ) -> Result<(), Vec<ResolvedBodyError>> {
        let scope = self.scopes.last_mut().expect("body always has a scope");
        if scope.insert(name.clone(), local.id.clone()).is_some() {
            return Err(vec![ResolvedBodyError::new(
                owner.clone(),
                format!("duplicate local binding '{name}' in one lexical scope"),
            )]);
        }
        if self.locals.insert(local.id.clone(), local).is_some() {
            return Err(vec![ResolvedBodyError::new(
                owner.clone(),
                "stable local identity collision",
            )]);
        }
        Ok(())
    }

    fn lookup_local(&self, name: &str) -> Option<ResolvedLocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    fn expr_id(&self, expr: &Expr, role: &str) -> Result<NodeId, Vec<ResolvedBodyError>> {
        let meta = expr.meta();
        self.catalogued_id(
            expr_kind(expr),
            role,
            meta.and_then(|meta| usable_span(meta.span)),
            meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User),
        )
    }

    fn stmt_id(&self, stmt: &Stmt, role: &str) -> Result<NodeId, Vec<ResolvedBodyError>> {
        let meta = stmt.meta();
        self.catalogued_id(
            stmt_kind(stmt),
            role,
            stmt_anchor(stmt, self.fallback).map(|(span, _)| span),
            meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User),
        )
    }

    fn pattern_id(&self, pattern: &Pattern, role: &str) -> Result<NodeId, Vec<ResolvedBodyError>> {
        self.catalogued_id(
            pattern_kind(pattern),
            role,
            usable_span(pattern.meta.span),
            pattern.meta.origin,
        )
    }

    fn catalogued_id(
        &self,
        kind: &str,
        role: &str,
        span: Option<Span>,
        origin: AstOrigin,
    ) -> Result<NodeId, Vec<ResolvedBodyError>> {
        let mut diagnostics = Vec::<Diagnostic>::new();
        let id = self
            .ids
            .anonymous(&self.owner, kind, role, span, origin, &mut diagnostics);
        if diagnostics.is_empty() {
            Ok(id)
        } else {
            Err(diagnostics
                .into_iter()
                .map(|diagnostic| ResolvedBodyError::new(id.clone(), diagnostic.message))
                .collect())
        }
    }

    fn block_identity(&self, role: &str, block: &[Stmt]) -> (NodeId, Origin) {
        let mut diagnostics = Vec::new();
        let node_id = self.ids.anonymous(
            &self.owner,
            "body.block",
            role,
            None,
            AstOrigin::Desugared(BLOCK_NORMALIZATION_RULE),
            &mut diagnostics,
        );
        debug_assert!(diagnostics.is_empty());
        let span = block
            .first()
            .and_then(|stmt| stmt_anchor(stmt, self.fallback).map(|(span, _)| span))
            .unwrap_or(self.fallback);
        (
            node_id,
            Origin::Desugared {
                parent: self.owner.clone(),
                rule: BLOCK_NORMALIZATION_RULE.into(),
                span,
            },
        )
    }

    fn origin(&self, node_id: &NodeId) -> Result<Origin, Vec<ResolvedBodyError>> {
        self.node_meta
            .get(node_id)
            .map(|meta| meta.origin.clone())
            .ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "stable node identity is absent from NodeMeta",
                )]
            })
    }

    fn unsupported<T>(
        &self,
        node_id: &NodeId,
        construct: &str,
    ) -> Result<T, Vec<ResolvedBodyError>> {
        Err(vec![ResolvedBodyError::new(
            node_id.clone(),
            format!("typed body lowering does not yet support {construct}"),
        )])
    }
}

fn usable_span(span: Span) -> Option<Span> {
    (span.start_line > 0 && span.start_col > 0).then_some(span)
}

fn numeric_width(primitive: super::PrimitiveType) -> Option<u16> {
    use super::PrimitiveType;
    Some(match primitive {
        PrimitiveType::I8 | PrimitiveType::U8 => 8,
        PrimitiveType::I16 | PrimitiveType::U16 => 16,
        PrimitiveType::I32 | PrimitiveType::U32 | PrimitiveType::F32 => 32,
        PrimitiveType::I64
        | PrimitiveType::U64
        | PrimitiveType::Isize
        | PrimitiveType::Usize
        | PrimitiveType::F64 => 64,
        PrimitiveType::I128 | PrimitiveType::U128 => 128,
        PrimitiveType::Bool | PrimitiveType::Char | PrimitiveType::String | PrimitiveType::Unit => {
            return None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{File, Item};

    fn parse(source: &str) -> File {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse")
    }

    fn function<'a>(file: &'a File, name: &str) -> &'a FuncDef {
        file.items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == name => Some(function),
                _ => None,
            })
            .expect("function")
    }

    #[test]
    fn lowers_basic_checked_function_with_stable_locals() {
        let file = parse(
            "func add(left: i32, right: i32) -> i32 {\n  let mut total = left + right;\n  total = total + 1;\n  total\n}",
        );
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("add").expect("resolved function");
        let signature = program
            .resolved_signature(&resolved.node_id)
            .expect("signature");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "add"),
            signature,
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower body");

        assert_eq!(body.locals.len(), 3);
        assert_eq!(body.root.statements.len(), 2);
        assert!(body.root.result.is_some());
        assert!(body.validate(program.resolved_types()).is_ok());
        assert!(body
            .locals
            .keys()
            .all(|local| local.0 .0.contains("/local")));
    }

    #[test]
    fn missing_expression_type_fails_closed() {
        let file = parse("func identity(value: i32) -> i32 { value }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("identity").expect("resolved function");
        let signature = program
            .resolved_signature(&resolved.node_id)
            .expect("signature");
        let empty = BTreeMap::new();
        let errors = lower_function_body(FunctionBodyInput {
            function: function(&file, "identity"),
            signature,
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: &empty,
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect_err("missing type must fail");
        assert!(errors
            .iter()
            .any(|error| error.message.contains("checker-finalized canonical type")));
    }

    #[test]
    fn dynamic_index_is_retained_as_a_typed_place_input() {
        let file = parse("func read(values: List<i32>, index: i32) -> i32 {\n  values[index]\n}");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("read").expect("resolved function");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "read"),
            signature: program
                .resolved_signature(&resolved.node_id)
                .expect("signature"),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower body");

        assert_eq!(body.place_inputs.len(), 1);
        let result = body.root.result.as_ref().expect("tail result");
        let ResolvedExprKind::Load(place) = &result.kind else {
            panic!("index expression must lower to a place load");
        };
        assert!(matches!(
            place.projections.as_slice(),
            [ResolvedProjection::Index {
                index: crate::core::ResolvedIndex::Dynamic(_),
                ..
            }]
        ));
        body.validate(program.resolved_types())
            .expect("place input is a body node");
    }

    #[test]
    fn indexed_assignment_uses_projected_target_type() {
        let file = parse(
            "func replace(mut values: List<i32>, index: i32) -> i32 {\n  values[index] = 7;\n  values[index]\n}",
        );
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("replace").expect("resolved function");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "replace"),
            signature: program
                .resolved_signature(&resolved.node_id)
                .expect("signature"),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower body");

        let ResolvedStmtKind::Assign {
            target, conversion, ..
        } = &body.root.statements[0].kind
        else {
            panic!("first statement must be assignment");
        };
        assert_eq!(conversion.to, target.projections[0].ty().clone());
        assert_ne!(conversion.to, body.locals[&target.base].ty);
        body.validate(program.resolved_types())
            .expect("indexed assignment validates");
    }

    #[test]
    fn function_call_is_closed_and_named_arguments_use_parameter_order() {
        let file = parse(
            "func subtract(left: i32, right: i32) -> i32 { left - right }\nfunc main() -> i32 { subtract(right = 2, left = 7) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("main").expect("resolved main");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "main"),
            signature: program
                .resolved_signature(&resolved.node_id)
                .expect("signature"),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower body");

        let result = body.root.result.as_ref().expect("tail result");
        let ResolvedExprKind::Call(call) = &result.kind else {
            panic!("tail must be a resolved call");
        };
        assert_eq!(
            call.arguments
                .iter()
                .map(|argument| argument.parameter.clone())
                .collect::<Vec<_>>(),
            program
                .resolved_signature(&NodeId("function:subtract".into()))
                .expect("callee signature")
                .parameters
                .iter()
                .map(|parameter| parameter.id.clone())
                .collect::<Vec<_>>()
        );
        let values = call
            .arguments
            .iter()
            .map(|argument| match argument.value.kind {
                ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => value,
                _ => panic!("argument must be literal"),
            })
            .collect::<Vec<_>>();
        assert_eq!(values, vec![7, 2]);
        assert!(matches!(
            call.callee,
            ResolvedCallee::Function(ref node) if node == &NodeId("function:subtract".into())
        ));
    }

    #[test]
    fn field_place_reuses_the_declaration_identity() {
        let file = parse("type Point { x: i32 }\nfunc read(point: Point) -> i32 { point.x }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("read").expect("resolved read");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "read"),
            signature: program.resolved_signature(&resolved.node_id).unwrap(),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower field load");
        let ResolvedExprKind::Load(place) = &body.root.result.as_ref().unwrap().kind else {
            panic!("field read must be a place load");
        };
        let ResolvedProjection::Field { field, ty } = &place.projections[0] else {
            panic!("field projection expected");
        };
        assert!(field.0.starts_with("type:Point/node:decl.field@"));
        assert!(program.node_meta().contains_key(field));
        assert_eq!(program.resolved_field_type(field), Some(ty));
    }

    #[test]
    fn match_arms_own_lexical_pattern_bindings() {
        let file =
            parse("func choose(value: i32) -> i32 { match value { 0 => 1, other => other } }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("choose").expect("resolved choose");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "choose"),
            signature: program.resolved_signature(&resolved.node_id).unwrap(),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower match");
        let ResolvedExprKind::Match { arms, .. } = &body.root.result.as_ref().unwrap().kind else {
            panic!("match result expected");
        };
        assert_eq!(arms.len(), 2);
        let ResolvedPatternKind::Binding { local, .. } = &arms[1].pattern.kind else {
            panic!("second arm must bind");
        };
        let ResolvedExprKind::Block(block) = &arms[1].body.kind else {
            panic!("arm body block expected");
        };
        let ResolvedExprKind::Load(place) = &block.result.as_ref().unwrap().kind else {
            panic!("arm body must load binding");
        };
        assert_eq!(&place.base, local);
        body.validate(program.resolved_types())
            .expect("valid match body");
    }

    #[test]
    fn tuple_binding_uses_canonical_element_types() {
        let file = parse("func pick(pair: (i32, i64)) -> i64 { let (left, right) = pair; right }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("pick").expect("resolved pick");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "pick"),
            signature: program.resolved_signature(&resolved.node_id).unwrap(),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower tuple binding");
        let ResolvedStmtKind::Bind { pattern, .. } = &body.root.statements[0].kind else {
            panic!("tuple bind expected");
        };
        let ResolvedPatternKind::Tuple(elements) = &pattern.kind else {
            panic!("tuple pattern expected");
        };
        assert_ne!(elements[0].ty, elements[1].ty);
        body.validate(program.resolved_types())
            .expect("valid tuple bind");
    }

    #[test]
    fn record_fields_are_sorted_by_declaration_identity() {
        let file =
            parse("type Point { x: i32, y: i32 }\nfunc make() -> Point { Point { y: 2, x: 1 } }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("make").expect("resolved make");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "make"),
            signature: program.resolved_signature(&resolved.node_id).unwrap(),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower record");
        let ResolvedExprKind::Record { fields, .. } = &body.root.result.as_ref().unwrap().kind
        else {
            panic!("record expected");
        };
        let values = fields
            .iter()
            .map(|field| match field.value.kind {
                ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => value,
                _ => panic!("literal field expected"),
            })
            .collect::<Vec<_>>();
        assert_eq!(values, vec![1, 2]);
        body.validate(program.resolved_types())
            .expect("valid record");
    }

    #[test]
    fn explicit_numeric_cast_records_checked_conversion() {
        let file = parse("func widen(value: i32) -> i64 { value as i64 }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("widen").expect("resolved widen");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "widen"),
            signature: program.resolved_signature(&resolved.node_id).unwrap(),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            field_types: program.resolved_field_types(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower cast");
        let ResolvedExprKind::Cast { conversion, .. } = &body.root.result.as_ref().unwrap().kind
        else {
            panic!("cast expected");
        };
        assert_eq!(conversion.kind, CheckedConversionKind::NumericWiden);
    }

    #[test]
    fn program_body_lowering_is_complete_and_transactional() {
        let file = parse(
            "func increment(value: i32) -> i32 { value + 1 }\nfunc main() -> i32 { increment(4) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("all bodies lower");
        assert_eq!(bodies.len(), program.resolved_signatures().len());
        assert!(bodies.contains_key(&NodeId("function:increment".into())));
        assert!(bodies.contains_key(&NodeId("function:main".into())));

        let builtin_file = parse("func main() { println(1) }");
        let checked = crate::core::check_program(&builtin_file).expect("checker accepts builtin");
        let bodies = lower_checked_function_bodies(&builtin_file, &checked)
            .expect("builtin identity is canonical");
        let main = &bodies[&NodeId("function:main".into())];
        let expression = main.root.result.as_ref().expect("builtin tail expression");
        assert!(matches!(
            expression.kind,
            ResolvedExprKind::Call(ResolvedCall {
                callee: ResolvedCallee::Builtin(_),
                ..
            })
        ));
    }

    #[test]
    fn extern_call_uses_declaration_and_parameter_identities() {
        let file = parse(
            "extern \"C\" { func c_abs(value: i32) -> i32 }\nfunc main() -> i32 { c_abs(-4) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower extern call");
        let result = bodies[&NodeId("function:main".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        let ResolvedExprKind::Call(call) = &result.kind else {
            panic!("extern call expected");
        };
        let ResolvedCallee::Extern(callee) = &call.callee else {
            panic!("extern identity expected");
        };
        assert!(callee.0.contains("/function:c_abs:"));
        assert!(call.arguments[0]
            .parameter
            .0
             .0
            .contains("decl.extern_parameter"));
    }

    #[test]
    fn for_binding_uses_canonical_iterable_element_type() {
        let file = parse(
            "func total(values: List<i32>) -> i32 { let mut sum = 0; for value in values { sum = sum + value; } sum }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower for loop");
        let function_body = &bodies[&NodeId("function:total".into())];
        let ResolvedStmtKind::For { pattern, body, .. } = &function_body.root.statements[1].kind
        else {
            panic!("for statement expected");
        };
        let ResolvedPatternKind::Binding { local, .. } = &pattern.kind else {
            panic!("for binding expected");
        };
        assert_eq!(function_body.locals[local].ty, pattern.ty);
        assert_eq!(body.statements.len(), 1);
        function_body
            .validate(program.resolved_types())
            .expect("valid for body");
    }

    #[test]
    fn constants_and_language_constructors_have_closed_identities() {
        let file = parse(
            "const ANSWER: i32 = 42;\nfunc answer() -> i32 { ANSWER }\nfunc some(value: i32) -> Option<i32> { Some(value) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower identities");
        let answer = bodies[&NodeId("function:answer".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        assert!(matches!(answer.kind, ResolvedExprKind::Constant(_)));
        let some = bodies[&NodeId("function:some".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        assert!(matches!(
            some.kind,
            ResolvedExprKind::Call(ResolvedCall {
                callee: ResolvedCallee::Builtin(_),
                ..
            })
        ));
    }

    #[test]
    fn shared_binding_has_explicit_ownership_wrap() {
        let file = parse("func main() { shared value = 42; println(value) }");
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower shared");
        let body = &bodies[&NodeId("function:main".into())];
        let ResolvedStmtKind::Bind {
            pattern,
            initializer: Some(initializer),
        } = &body.root.statements[0].kind
        else {
            panic!("shared binding expected");
        };
        let ResolvedExprKind::Cast { conversion, .. } = &initializer.kind else {
            panic!("ownership wrap expected");
        };
        assert_eq!(conversion.kind, CheckedConversionKind::OwnershipWrap);
        assert_eq!(conversion.to, pattern.ty);
        body.validate(program.resolved_types())
            .expect("valid shared body");
    }

    #[test]
    fn delegate_target_is_closed_to_callable_identity() {
        let file = parse(
            "func child(value: i32) { println(value) }\nfunc main() { let value = 1; delegate view(value) to child }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower delegate");
        let body = &bodies[&NodeId("function:main".into())];
        let ResolvedStmtKind::Delegate { target, .. } = &body.root.statements[1].kind else {
            panic!("delegate expected");
        };
        assert!(matches!(
            target,
            crate::core::ir::DelegateTarget::Callable(node)
                if node == &NodeId("function:child".into())
        ));
    }
}
