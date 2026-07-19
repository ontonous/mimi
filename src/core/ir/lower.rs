//! Surface-independent construction of typed callable bodies.
//!
//! This module is deliberately separate from `resolved`: the latter owns the
//! stable identity catalog, while this lowering consumes those identities and
//! checker-finalized types.  It must never infer a type or resolve a name.

use super::{
    CheckedConversion, CheckedConversionKind, ResolvedBinaryOp, ResolvedBlock, ResolvedBody,
    ResolvedBodyError, ResolvedExpr, ResolvedExprKind, ResolvedLiteral, ResolvedLocal,
    ResolvedLocalId, ResolvedPattern, ResolvedPatternKind, ResolvedPlace, ResolvedSignature,
    ResolvedStmt, ResolvedStmtKind, ResolvedType, ResolvedTypeId, ResolvedTypeTable,
    ResolvedUnaryOp,
};
use crate::ast::{AstOrigin, BinOp, Expr, FuncDef, Lit, Pattern, PatternKind, Stmt, UnOp};
use crate::core::resolved::{
    expr_kind, expr_sibling_role, pattern_kind, stmt_anchor, stmt_kind, stmt_sibling_role,
    NodeIdBuilder,
};
use crate::core::{NodeId, NodeMeta, Origin};
use crate::diagnostic::Diagnostic;
use crate::span::{SourceRegistry, Span};
use std::collections::{BTreeMap, HashMap};

const BLOCK_NORMALIZATION_RULE: &str = "resolved_body.structured_block";

/// Inputs already finalized by the checker and stable resolved walker.
pub struct FunctionBodyInput<'a> {
    pub function: &'a FuncDef,
    pub signature: &'a ResolvedSignature,
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
        node_types: input.node_types,
        node_meta: input.node_meta,
        ids: NodeIdBuilder::new(input.sources),
        unit,
        locals: BTreeMap::new(),
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
        root,
    };
    body.validate(input.types)?;
    Ok(body)
}

struct BodyLowerer<'a> {
    owner: NodeId,
    fallback: Span,
    signature: &'a ResolvedSignature,
    node_types: &'a BTreeMap<NodeId, ResolvedTypeId>,
    node_meta: &'a HashMap<NodeId, NodeMeta>,
    ids: NodeIdBuilder<'a>,
    unit: ResolvedTypeId,
    locals: BTreeMap<ResolvedLocalId, ResolvedLocal>,
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
            Stmt::Desc(..) | Stmt::Rule(..) | Stmt::MmsBlock { .. } => return Ok(None),
            Stmt::WhileLet { .. }
            | Stmt::For { .. }
            | Stmt::Arena(_)
            | Stmt::Math(_)
            | Stmt::SharedLet { .. }
            | Stmt::Delegate { .. }
            | Stmt::Pinned { .. }
            | Stmt::Parasteps(_)
            | Stmt::Func(_)
            | Stmt::Alloc { .. }
            | Stmt::Ellipsis => return self.unsupported(&node_id, stmt_kind(stmt)),
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
                let local = self.lookup_local(name).ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        format!("identifier '{name}' has no resolved local identity"),
                    )]
                })?;
                ResolvedExprKind::Load(ResolvedPlace::root(local))
            }
            Expr::Binary(op, left, right) => ResolvedExprKind::Binary {
                op: self.lower_binary(&node_id, *op)?,
                left: Box::new(self.lower_expr(left, &format!("{role}.left"))?),
                right: Box::new(self.lower_expr(right, &format!("{role}.right"))?),
            },
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
            Expr::Call(_, _)
            | Expr::Field(_, _)
            | Expr::Index(_, _)
            | Expr::TupleIndex(_, _)
            | Expr::Comprehension { .. }
            | Expr::Match(_, _)
            | Expr::Record { .. }
            | Expr::Try(_)
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
            | Expr::MapLiteral { .. }
            | Expr::NamedArg(_, _)
            | Expr::Cast(_, _) => return self.unsupported(&node_id, expr_kind(expr)),
            Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
        };
        Ok(ResolvedExpr {
            node_id,
            origin,
            ty,
            effects: Vec::new(),
            backend_requirements: Vec::new(),
            kind,
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
            PatternKind::Constructor(_, _)
            | PatternKind::Tuple(_)
            | PatternKind::Array(_)
            | PatternKind::Slice(_, _) => return self.unsupported(&node_id, pattern_kind(pattern)),
        };
        Ok(ResolvedPattern {
            node_id,
            origin,
            ty,
            kind,
        })
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
            _ => self.unsupported(&node_id, "projected place lowering"),
        }
    }

    fn place_type(
        &self,
        node_id: &NodeId,
        place: &ResolvedPlace,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        self.locals
            .get(&place.base)
            .map(|local| local.ty.clone())
            .ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "place references an unknown local",
                )]
            })
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
}
