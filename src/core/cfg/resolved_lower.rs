use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::core::ir::{
    MatchArm, ResolvedBlock, ResolvedExpr, ResolvedExprKind, ResolvedFStringPart, ResolvedIndex,
    ResolvedPattern, ResolvedPatternKind, ResolvedPlace, ResolvedProjection, ResolvedStmt,
    ResolvedStmtKind, ResolvedValueProjection,
};
use crate::core::{NodeId, Origin, ResolvedBody};
use crate::diagnostic::Diagnostic;

use super::{
    BasicBlock, BasicBlockId, CallableCfg, CfgEdge, CfgPoint, CfgPointKind, CfgSource, EdgeId,
    EdgeKind, Terminator,
};

#[derive(Debug)]
struct DraftBlock {
    id: BasicBlockId,
    source: CfgSource,
    points: Vec<CfgPoint>,
    terminator: Option<Terminator>,
}

#[derive(Debug, Clone)]
struct LoopContext {
    break_target: BasicBlockId,
    continue_target: BasicBlockId,
    break_seen: bool,
}

#[derive(Default)]
struct PointAccesses {
    uses: Vec<String>,
    defs: Vec<String>,
    reads: Vec<String>,
    writes: Vec<String>,
}

struct ResolvedCfgLowerer<'a> {
    owner: NodeId,
    body: &'a ResolvedBody,
    blocks: BTreeMap<BasicBlockId, DraftBlock>,
    edges: BTreeMap<EdgeId, CfgEdge>,
    loops: Vec<LoopContext>,
    orphan_roots: BTreeSet<BasicBlockId>,
    errors: Vec<Diagnostic>,
}

impl<'a> ResolvedCfgLowerer<'a> {
    fn new(body: &'a ResolvedBody) -> Self {
        Self {
            owner: body.owner.clone(),
            body,
            blocks: BTreeMap::new(),
            edges: BTreeMap::new(),
            loops: Vec::new(),
            orphan_roots: BTreeSet::new(),
            errors: Vec::new(),
        }
    }

    fn source(node: &NodeId, origin: &Origin) -> CfgSource {
        CfgSource {
            node: node.clone(),
            span: origin.user_span(),
            origin: origin.clone(),
        }
    }

    fn derived_id(node: &NodeId, category: &str, role: &str) -> NodeId {
        NodeId(format!("{}/cfg:{category}:{role}", node.0))
    }

    fn new_block(&mut self, node: &NodeId, origin: &Origin, role: &str) -> BasicBlockId {
        let id = BasicBlockId(Self::derived_id(node, "block", role));
        let source = Self::source(node, origin);
        if self.blocks.contains_key(&id) {
            self.errors.push(Diagnostic::error(
                format!("duplicate CFG block identity '{}'", id.0 .0),
                source.span,
            ));
        }
        self.blocks.insert(
            id.clone(),
            DraftBlock {
                id: id.clone(),
                source,
                points: Vec::new(),
                terminator: None,
            },
        );
        id
    }

    fn point(
        &mut self,
        block: &BasicBlockId,
        node: &NodeId,
        origin: &Origin,
        kind: CfgPointKind,
        mut accesses: PointAccesses,
    ) {
        accesses.uses.sort();
        accesses.uses.dedup();
        accesses.defs.sort();
        accesses.defs.dedup();
        accesses.reads.sort();
        accesses.reads.dedup();
        accesses.writes.sort();
        accesses.writes.dedup();
        let Some(block) = self.blocks.get_mut(block) else {
            self.errors.push(Diagnostic::error(
                "attempted to append a point to a missing CFG block".to_string(),
                origin.user_span(),
            ));
            return;
        };
        block.points.push(CfgPoint {
            source: Self::source(node, origin),
            kind,
            uses: accesses.uses,
            defs: accesses.defs,
            reads: accesses.reads,
            writes: accesses.writes,
        });
    }

    fn edge(
        &mut self,
        from: &BasicBlockId,
        to: &BasicBlockId,
        kind: EdgeKind,
        node: &NodeId,
        origin: &Origin,
        role: &str,
    ) -> EdgeId {
        let id = EdgeId(Self::derived_id(node, "edge", role));
        if self.edges.contains_key(&id) {
            self.errors.push(Diagnostic::error(
                format!("duplicate CFG edge identity '{}'", id.0 .0),
                origin.user_span(),
            ));
        }
        self.edges.insert(
            id.clone(),
            CfgEdge {
                id: id.clone(),
                from: from.clone(),
                to: to.clone(),
                kind,
                source: Self::source(node, origin),
            },
        );
        id
    }

    fn terminate(&mut self, block: &BasicBlockId, terminator: Terminator) {
        let Some(block) = self.blocks.get_mut(block) else {
            self.errors.push(Diagnostic::error(
                "attempted to terminate a missing CFG block".to_string(),
                self.body.root.origin.user_span(),
            ));
            return;
        };
        if block.terminator.replace(terminator).is_some() {
            self.errors.push(Diagnostic::error(
                format!(
                    "CFG block '{}' received more than one terminator",
                    block.id.0 .0
                ),
                block.source.span,
            ));
        }
    }

    fn goto(
        &mut self,
        from: &BasicBlockId,
        to: &BasicBlockId,
        kind: EdgeKind,
        node: &NodeId,
        origin: &Origin,
        role: &str,
    ) {
        let edge = self.edge(from, to, kind, node, origin, role);
        self.terminate(from, Terminator::Goto { edge });
    }

    fn ensure_current(
        &mut self,
        current: Option<BasicBlockId>,
        node: &NodeId,
        origin: &Origin,
    ) -> BasicBlockId {
        current.unwrap_or_else(|| {
            let block = self.new_block(node, origin, "unreachable");
            self.orphan_roots.insert(block.clone());
            block
        })
    }

    fn lower_block(
        &mut self,
        block: &ResolvedBlock,
        mut current: Option<BasicBlockId>,
    ) -> Option<BasicBlockId> {
        for statement in &block.statements {
            let block_id = self.ensure_current(current, &statement.node_id, &statement.origin);
            current = self.lower_stmt(statement, block_id);
        }
        if let Some(result) = &block.result {
            let block_id = self.ensure_current(current, &result.node_id, &result.origin);
            current = self.lower_expr(result, block_id, CfgPointKind::Expression);
        }
        current
    }

    fn lower_stmt(
        &mut self,
        statement: &ResolvedStmt,
        current: BasicBlockId,
    ) -> Option<BasicBlockId> {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                let current = match initializer {
                    Some(initializer) => {
                        self.lower_expr(initializer, current, CfgPointKind::Expression)?
                    }
                    None => current,
                };
                self.point(
                    &current,
                    &statement.node_id,
                    &statement.origin,
                    CfgPointKind::Binding,
                    PointAccesses {
                        defs: self.pattern_names(pattern),
                        ..PointAccesses::default()
                    },
                );
                Some(current)
            }
            ResolvedStmtKind::Assign { target, value, .. } => {
                let current = self.lower_expr(value, current, CfgPointKind::Expression)?;
                let current = self.lower_place_inputs(target, current)?;
                let spelling = self.place_spelling(target);
                let defs = target
                    .projections
                    .is_empty()
                    .then(|| self.local_name(&target.base))
                    .into_iter()
                    .collect();
                self.point(
                    &current,
                    &statement.node_id,
                    &statement.origin,
                    CfgPointKind::Assignment,
                    PointAccesses {
                        uses: vec![self.local_name(&target.base)],
                        defs,
                        writes: vec![spelling],
                        ..PointAccesses::default()
                    },
                );
                Some(current)
            }
            ResolvedStmtKind::Return { value, .. } => {
                let (current, value) = match value {
                    Some(value) => (
                        self.lower_expr(value, current, CfgPointKind::Expression)?,
                        Some(value.node_id.clone()),
                    ),
                    None => (current, None),
                };
                self.point(
                    &current,
                    &statement.node_id,
                    &statement.origin,
                    CfgPointKind::ResourceAction,
                    PointAccesses::default(),
                );
                self.terminate(
                    &current,
                    Terminator::Return {
                        value,
                        implicit: false,
                    },
                );
                None
            }
            ResolvedStmtKind::Break(value) => {
                let current = match value {
                    Some(value) => self.lower_expr(value, current, CfgPointKind::Expression)?,
                    None => current,
                };
                self.point(
                    &current,
                    &statement.node_id,
                    &statement.origin,
                    CfgPointKind::Statement,
                    PointAccesses::default(),
                );
                let Some(loop_) = self.loops.last().cloned() else {
                    self.terminate(&current, Terminator::Diverge);
                    return None;
                };
                if let Some(active) = self.loops.last_mut() {
                    active.break_seen = true;
                }
                let edge = self.edge(
                    &current,
                    &loop_.break_target,
                    EdgeKind::Break,
                    &statement.node_id,
                    &statement.origin,
                    "break",
                );
                self.terminate(&current, Terminator::Break { edge });
                None
            }
            ResolvedStmtKind::Continue => {
                self.point(
                    &current,
                    &statement.node_id,
                    &statement.origin,
                    CfgPointKind::Statement,
                    PointAccesses::default(),
                );
                let Some(loop_) = self.loops.last().cloned() else {
                    self.terminate(&current, Terminator::Diverge);
                    return None;
                };
                let edge = self.edge(
                    &current,
                    &loop_.continue_target,
                    EdgeKind::Continue,
                    &statement.node_id,
                    &statement.origin,
                    "continue",
                );
                self.terminate(&current, Terminator::Continue { edge });
                None
            }
            ResolvedStmtKind::Expr(expression) => {
                self.lower_expr(expression, current, CfgPointKind::Expression)
            }
            ResolvedStmtKind::While { condition, body } => {
                self.lower_conditional_loop(condition, body, None, current, statement, "while")
            }
            ResolvedStmtKind::WhileLet {
                pattern,
                initializer,
                body,
            } => self.lower_conditional_loop(
                initializer,
                body,
                Some(pattern),
                current,
                statement,
                "while-let",
            ),
            ResolvedStmtKind::For {
                pattern,
                iterable,
                body,
            } => self.lower_conditional_loop(
                iterable,
                body,
                Some(pattern),
                current,
                statement,
                "for",
            ),
            ResolvedStmtKind::Loop(body) => self.lower_infinite_loop(body, current, statement),
            ResolvedStmtKind::Drop(places) => {
                let mut current = current;
                for place in places {
                    current = self.lower_place_inputs(place, current)?;
                }
                self.point(
                    &current,
                    &statement.node_id,
                    &statement.origin,
                    CfgPointKind::ResourceAction,
                    PointAccesses {
                        reads: places
                            .iter()
                            .map(|place| self.place_spelling(place))
                            .collect(),
                        ..PointAccesses::default()
                    },
                );
                Some(current)
            }
            ResolvedStmtKind::Contract { condition, .. } => {
                self.lower_expr(condition, current, CfgPointKind::Expression)
            }
            ResolvedStmtKind::Math(expressions) => {
                let mut current = Some(current);
                for expression in expressions {
                    let block =
                        self.ensure_current(current, &expression.node_id, &expression.origin);
                    current = self.lower_expr(expression, block, CfgPointKind::Expression);
                }
                current
            }
            ResolvedStmtKind::Scope { body, .. } => self.lower_block(body, Some(current)),
            ResolvedStmtKind::Delegate { source, .. } => {
                let current = self.lower_place_inputs(source, current)?;
                self.point(
                    &current,
                    &statement.node_id,
                    &statement.origin,
                    CfgPointKind::ResourceAction,
                    PointAccesses {
                        uses: vec![self.local_name(&source.base)],
                        reads: vec![self.place_spelling(source)],
                        ..PointAccesses::default()
                    },
                );
                Some(current)
            }
            ResolvedStmtKind::Pinned {
                value,
                timeout,
                binding,
                body,
            } => {
                let mut current = self.lower_expr(value, current, CfgPointKind::Expression)?;
                if let Some(timeout) = timeout {
                    current = self.lower_expr(timeout, current, CfgPointKind::Expression)?;
                }
                if let Some(binding) = binding {
                    self.point(
                        &current,
                        &statement.node_id,
                        &statement.origin,
                        CfgPointKind::Binding,
                        PointAccesses {
                            defs: vec![self.local_name(binding)],
                            ..PointAccesses::default()
                        },
                    );
                }
                self.lower_block(body, Some(current))
            }
            ResolvedStmtKind::NestedCallable(_) => {
                self.point(
                    &current,
                    &statement.node_id,
                    &statement.origin,
                    CfgPointKind::Statement,
                    PointAccesses::default(),
                );
                Some(current)
            }
        }
    }

    fn lower_expr(
        &mut self,
        expression: &ResolvedExpr,
        current: BasicBlockId,
        point_kind: CfgPointKind,
    ) -> Option<BasicBlockId> {
        match &expression.kind {
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => self.lower_if(
                expression, condition, then_block, else_block, current, point_kind,
            ),
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.lower_match(expression, scrutinee, arms, current, point_kind)
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => {
                let current = self.lower_block(block, Some(current))?;
                self.point(
                    &current,
                    &expression.node_id,
                    &expression.origin,
                    point_kind,
                    PointAccesses::default(),
                );
                Some(current)
            }
            _ => {
                let current = self.lower_expr_children(expression, current)?;
                let (uses, reads) = self.direct_accesses(expression);
                self.point(
                    &current,
                    &expression.node_id,
                    &expression.origin,
                    point_kind,
                    PointAccesses {
                        uses,
                        reads,
                        ..PointAccesses::default()
                    },
                );
                Some(current)
            }
        }
    }

    fn lower_expr_children(
        &mut self,
        expression: &ResolvedExpr,
        mut current: BasicBlockId,
    ) -> Option<BasicBlockId> {
        macro_rules! lower {
            ($value:expr) => {{
                current = self.lower_expr($value, current, CfgPointKind::Expression)?;
            }};
        }
        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        lower!(value);
                    }
                }
            }
            ResolvedExprKind::Load(place) => {
                current = self.lower_place_inputs(place, current)?;
            }
            ResolvedExprKind::Project { value, projection } => {
                lower!(value);
                if let ResolvedValueProjection::Index(index) = projection {
                    lower!(index);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                lower!(left);
                lower!(right);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => lower!(operand),
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    lower!(&argument.value);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    lower!(value);
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    lower!(key);
                    lower!(value);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                lower!(iterable);
                if let Some(guard) = guard {
                    lower!(guard);
                }
                lower!(value);
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => lower!(receiver),
            ResolvedExprKind::Record { fields, .. } => {
                for field in fields {
                    lower!(&field.value);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                lower!(start);
                lower!(end);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                lower!(target);
                if let Some(start) = start {
                    lower!(start);
                }
                if let Some(end) = end {
                    lower!(end);
                }
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::Lambda(_)
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
            ResolvedExprKind::Block(_)
            | ResolvedExprKind::Scope { .. }
            | ResolvedExprKind::Comptime(_)
            | ResolvedExprKind::If { .. }
            | ResolvedExprKind::Match { .. }
            | ResolvedExprKind::Quote(_) => {
                unreachable!("control expressions are lowered before their child dispatcher")
            }
        }
        Some(current)
    }

    fn lower_if(
        &mut self,
        expression: &ResolvedExpr,
        condition: &ResolvedExpr,
        then_block: &ResolvedBlock,
        else_block: &ResolvedBlock,
        current: BasicBlockId,
        result_kind: CfgPointKind,
    ) -> Option<BasicBlockId> {
        let condition_end = self.lower_expr(condition, current, CfgPointKind::Condition)?;
        let then_entry = self.new_block(&then_block.node_id, &then_block.origin, "if-then");
        let else_entry = self.new_block(&else_block.node_id, &else_block.origin, "if-else");
        let join = self.new_block(&expression.node_id, &expression.origin, "if-join");
        let then_edge = self.edge(
            &condition_end,
            &then_entry,
            EdgeKind::Then,
            &expression.node_id,
            &expression.origin,
            "if-then",
        );
        let else_edge = self.edge(
            &condition_end,
            &else_entry,
            EdgeKind::Else,
            &expression.node_id,
            &expression.origin,
            "if-else",
        );
        self.terminate(
            &condition_end,
            Terminator::Branch {
                condition: condition.node_id.clone(),
                then_edge,
                else_edge,
            },
        );

        let then_end = self.lower_block(then_block, Some(then_entry));
        let else_end = self.lower_block(else_block, Some(else_entry));
        let mut falls_through = false;
        if let Some(end) = then_end {
            self.goto(
                &end,
                &join,
                EdgeKind::Fallthrough,
                &then_block.node_id,
                &then_block.origin,
                "if-then-join",
            );
            falls_through = true;
        }
        if let Some(end) = else_end {
            self.goto(
                &end,
                &join,
                EdgeKind::Fallthrough,
                &else_block.node_id,
                &else_block.origin,
                "if-else-join",
            );
            falls_through = true;
        }
        if !falls_through {
            return None;
        }
        self.point(
            &join,
            &expression.node_id,
            &expression.origin,
            result_kind,
            PointAccesses::default(),
        );
        Some(join)
    }

    fn lower_match(
        &mut self,
        expression: &ResolvedExpr,
        scrutinee: &ResolvedExpr,
        arms: &[MatchArm],
        current: BasicBlockId,
        result_kind: CfgPointKind,
    ) -> Option<BasicBlockId> {
        let dispatch = self.lower_expr(scrutinee, current, CfgPointKind::Condition)?;
        let join = self.new_block(&expression.node_id, &expression.origin, "match-join");
        let mut edges = Vec::new();
        let mut falls_through = false;
        for arm in arms {
            let arm_entry = self.new_block(&arm.node_id, &arm.origin, "match-arm");
            edges.push(self.edge(
                &dispatch,
                &arm_entry,
                EdgeKind::MatchArm,
                &arm.node_id,
                &arm.origin,
                "match-arm",
            ));
            self.point(
                &arm_entry,
                &arm.pattern.node_id,
                &arm.pattern.origin,
                CfgPointKind::Binding,
                PointAccesses {
                    defs: self.pattern_names(&arm.pattern),
                    ..PointAccesses::default()
                },
            );
            let mut arm_end = Some(arm_entry);
            if let Some(guard) = &arm.guard {
                let block = self.ensure_current(arm_end, &guard.node_id, &guard.origin);
                arm_end = self.lower_expr(guard, block, CfgPointKind::Condition);
            }
            if let Some(block) = arm_end {
                arm_end = self.lower_expr(&arm.body, block, CfgPointKind::Expression);
            }
            if let Some(end) = arm_end {
                self.goto(
                    &end,
                    &join,
                    EdgeKind::Fallthrough,
                    &arm.node_id,
                    &arm.origin,
                    "match-arm-join",
                );
                falls_through = true;
            }
        }
        if arms.is_empty() {
            self.terminate(&dispatch, Terminator::Diverge);
            return None;
        }
        self.terminate(
            &dispatch,
            Terminator::Match {
                scrutinee: scrutinee.node_id.clone(),
                arms: edges,
            },
        );
        if !falls_through {
            return None;
        }
        self.point(
            &join,
            &expression.node_id,
            &expression.origin,
            result_kind,
            PointAccesses::default(),
        );
        Some(join)
    }

    fn lower_conditional_loop(
        &mut self,
        condition: &ResolvedExpr,
        body: &ResolvedBlock,
        pattern: Option<&ResolvedPattern>,
        current: BasicBlockId,
        statement: &ResolvedStmt,
        role: &str,
    ) -> Option<BasicBlockId> {
        let header = self.new_block(
            &condition.node_id,
            &condition.origin,
            &format!("{role}-header"),
        );
        let body_entry = self.new_block(&body.node_id, &body.origin, &format!("{role}-body"));
        let exit = self.new_block(
            &statement.node_id,
            &statement.origin,
            &format!("{role}-exit"),
        );
        self.goto(
            &current,
            &header,
            EdgeKind::Fallthrough,
            &statement.node_id,
            &statement.origin,
            &format!("{role}-enter"),
        );
        let condition_end = self.lower_expr(condition, header.clone(), CfgPointKind::Condition)?;
        let body_edge = self.edge(
            &condition_end,
            &body_entry,
            EdgeKind::LoopBody,
            &statement.node_id,
            &statement.origin,
            &format!("{role}-body"),
        );
        let exit_edge = self.edge(
            &condition_end,
            &exit,
            EdgeKind::LoopExit,
            &statement.node_id,
            &statement.origin,
            &format!("{role}-exit"),
        );
        self.terminate(
            &condition_end,
            Terminator::Branch {
                condition: condition.node_id.clone(),
                then_edge: body_edge,
                else_edge: exit_edge,
            },
        );
        if let Some(pattern) = pattern {
            self.point(
                &body_entry,
                &pattern.node_id,
                &pattern.origin,
                CfgPointKind::Binding,
                PointAccesses {
                    defs: self.pattern_names(pattern),
                    ..PointAccesses::default()
                },
            );
        }
        self.loops.push(LoopContext {
            break_target: exit.clone(),
            continue_target: header.clone(),
            break_seen: false,
        });
        let body_end = self.lower_block(body, Some(body_entry));
        self.loops.pop();
        if let Some(end) = body_end {
            self.goto(
                &end,
                &header,
                EdgeKind::Backedge,
                &statement.node_id,
                &statement.origin,
                &format!("{role}-backedge"),
            );
        }
        Some(exit)
    }

    fn lower_infinite_loop(
        &mut self,
        body: &ResolvedBlock,
        current: BasicBlockId,
        statement: &ResolvedStmt,
    ) -> Option<BasicBlockId> {
        let header = self.new_block(&body.node_id, &body.origin, "loop-header");
        let exit = self.new_block(&statement.node_id, &statement.origin, "loop-exit");
        self.goto(
            &current,
            &header,
            EdgeKind::Fallthrough,
            &statement.node_id,
            &statement.origin,
            "loop-enter",
        );
        self.loops.push(LoopContext {
            break_target: exit.clone(),
            continue_target: header.clone(),
            break_seen: false,
        });
        let body_end = self.lower_block(body, Some(header.clone()));
        let break_seen = self.loops.pop().is_some_and(|loop_| loop_.break_seen);
        if let Some(end) = body_end {
            self.goto(
                &end,
                &header,
                EdgeKind::Backedge,
                &statement.node_id,
                &statement.origin,
                "loop-backedge",
            );
        }
        break_seen.then_some(exit)
    }

    fn lower_place_inputs(
        &mut self,
        place: &ResolvedPlace,
        mut current: BasicBlockId,
    ) -> Option<BasicBlockId> {
        for projection in &place.projections {
            let ResolvedProjection::Index {
                index: ResolvedIndex::Dynamic(node),
                ..
            } = projection
            else {
                continue;
            };
            let Some(input) = self.body.place_inputs.get(node) else {
                self.errors.push(Diagnostic::error(
                    format!(
                        "dynamic place input '{}' is absent from ResolvedBody",
                        node.0
                    ),
                    self.body.root.origin.user_span(),
                ));
                return None;
            };
            current = self.lower_expr(input, current, CfgPointKind::Expression)?;
        }
        Some(current)
    }

    fn direct_accesses(&self, expression: &ResolvedExpr) -> (Vec<String>, Vec<String>) {
        match &expression.kind {
            ResolvedExprKind::Load(place) => (
                vec![self.local_name(&place.base)],
                vec![self.place_spelling(place)],
            ),
            ResolvedExprKind::Lambda(lambda) => {
                let names = lambda
                    .captures
                    .iter()
                    .map(|capture| self.local_name(capture))
                    .collect::<Vec<_>>();
                (names.clone(), names)
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    fn local_name(&self, local: &crate::core::ResolvedLocalId) -> String {
        self.body
            .locals
            .get(local)
            .map(|local| local.display_name.clone())
            .unwrap_or_else(|| local.0 .0.clone())
    }

    fn pattern_names(&self, pattern: &ResolvedPattern) -> Vec<String> {
        fn collect(
            lowerer: &ResolvedCfgLowerer<'_>,
            pattern: &ResolvedPattern,
            names: &mut Vec<String>,
        ) {
            match &pattern.kind {
                ResolvedPatternKind::Binding { local, .. } => {
                    names.push(lowerer.local_name(local));
                }
                ResolvedPatternKind::Constructor { fields, .. } => {
                    for (_, pattern) in fields {
                        collect(lowerer, pattern, names);
                    }
                }
                ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => {
                    for pattern in patterns {
                        collect(lowerer, pattern, names);
                    }
                }
                ResolvedPatternKind::Slice { prefix, rest } => {
                    for pattern in prefix {
                        collect(lowerer, pattern, names);
                    }
                    if let Some(rest) = rest {
                        collect(lowerer, rest, names);
                    }
                }
                ResolvedPatternKind::Wildcard | ResolvedPatternKind::Literal(_) => {}
            }
        }
        let mut names = Vec::new();
        collect(self, pattern, &mut names);
        names
    }

    fn place_spelling(&self, place: &ResolvedPlace) -> String {
        let mut spelling = self.local_name(&place.base);
        for projection in &place.projections {
            match projection {
                ResolvedProjection::Field { field, .. } => {
                    let name = field
                        .0
                        .rsplit_once("::")
                        .map(|(_, name)| name)
                        .unwrap_or(&field.0);
                    spelling.push('.');
                    spelling.push_str(name);
                }
                ResolvedProjection::Tuple { index, .. } => {
                    spelling.push('.');
                    spelling.push_str(&index.to_string());
                }
                ResolvedProjection::Index { index, .. } => match index {
                    ResolvedIndex::Constant(index) => {
                        spelling.push('[');
                        spelling.push_str(&index.to_string());
                        spelling.push(']');
                    }
                    ResolvedIndex::Dynamic(_) => spelling.push_str("[*]"),
                },
                ResolvedProjection::Deref { .. } => spelling = format!("*{spelling}"),
            }
        }
        spelling
    }

    fn finish(
        mut self,
        entry: BasicBlockId,
        current: Option<BasicBlockId>,
    ) -> Result<CallableCfg, Vec<Diagnostic>> {
        if let Some(current) = current {
            if self
                .blocks
                .get(&current)
                .is_some_and(|block| block.terminator.is_none())
            {
                self.terminate(
                    &current,
                    Terminator::Return {
                        value: self
                            .body
                            .root
                            .result
                            .as_ref()
                            .map(|result| result.node_id.clone()),
                        implicit: true,
                    },
                );
            }
        }
        for orphan in self.orphan_roots.clone() {
            if self
                .blocks
                .get(&orphan)
                .is_some_and(|block| block.terminator.is_none())
            {
                self.terminate(&orphan, Terminator::Unreachable);
            }
        }
        for block in self.blocks.values_mut() {
            if block.terminator.is_none() {
                block.terminator = Some(Terminator::Unreachable);
            }
        }
        let blocks = self
            .blocks
            .into_iter()
            .map(|(id, block)| {
                (
                    id,
                    BasicBlock {
                        id: block.id,
                        source: block.source,
                        points: block.points,
                        terminator: block
                            .terminator
                            .expect("CFG finalization installs every terminator"),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut reachable = BTreeSet::new();
        let mut queue = VecDeque::from([entry.clone()]);
        while let Some(block) = queue.pop_front() {
            if !reachable.insert(block.clone()) {
                continue;
            }
            for edge in self.edges.values().filter(|edge| edge.from == block) {
                queue.push_back(edge.to.clone());
            }
        }
        let cfg = CallableCfg {
            owner: self.owner,
            entry,
            blocks,
            edges: self.edges,
            reachable,
        };
        if let Err(mut errors) = cfg.validate() {
            self.errors.append(&mut errors);
        }
        if self.errors.is_empty() {
            Ok(cfg)
        } else {
            Err(self.errors)
        }
    }
}

fn lower_body(body: &ResolvedBody) -> Result<CallableCfg, Vec<Diagnostic>> {
    let mut lowerer = ResolvedCfgLowerer::new(body);
    let entry = lowerer.new_block(&body.root.node_id, &body.root.origin, "entry");
    let current = lowerer.lower_block(&body.root, Some(entry.clone()));
    lowerer.finish(entry, current)
}

pub fn lower_resolved_bodies(
    bodies: &BTreeMap<NodeId, ResolvedBody>,
) -> Result<BTreeMap<NodeId, CallableCfg>, Vec<Diagnostic>> {
    let mut cfgs = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, body) in bodies {
        if owner != &body.owner {
            errors.push(Diagnostic::error(
                format!(
                    "ResolvedBody key '{}' disagrees with owner '{}'",
                    owner.0, body.owner.0
                ),
                body.root.origin.user_span(),
            ));
            continue;
        }
        match lower_body(body) {
            Ok(cfg) => {
                cfgs.insert(owner.clone(), cfg);
            }
            Err(mut body_errors) => errors.append(&mut body_errors),
        }
    }
    if errors.is_empty() {
        Ok(cfgs)
    } else {
        Err(errors)
    }
}
