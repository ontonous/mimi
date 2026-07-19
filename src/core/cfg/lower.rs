use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::ast::{
    AstNodeMeta, AstOrigin, Block, Expr, File, Item, Lit, Pattern, PatternKind, Stmt,
};
use crate::core::resolved::{impl_method_owner, nested_function_owner, NodeIdBuilder};
use crate::core::NodeId;
use crate::diagnostic::Diagnostic;
use crate::span::SourceRegistry;

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

struct Lowerer<'a> {
    owner: NodeId,
    ids: NodeIdBuilder<'a>,
    fallback: AstNodeMeta,
    blocks: BTreeMap<BasicBlockId, DraftBlock>,
    edges: BTreeMap<EdgeId, CfgEdge>,
    loops: Vec<LoopContext>,
    errors: Vec<Diagnostic>,
    orphan_roots: BTreeSet<BasicBlockId>,
    role_occurrences: HashMap<String, usize>,
}

impl<'a> Lowerer<'a> {
    fn new(owner: NodeId, sources: &'a SourceRegistry, fallback: AstNodeMeta) -> Self {
        Self {
            owner,
            ids: NodeIdBuilder::new(sources),
            fallback,
            blocks: BTreeMap::new(),
            edges: BTreeMap::new(),
            loops: Vec::new(),
            errors: Vec::new(),
            orphan_roots: BTreeSet::new(),
            role_occurrences: HashMap::new(),
        }
    }

    fn meta_or_fallback(&self, meta: Option<AstNodeMeta>) -> AstNodeMeta {
        match meta {
            Some(meta) if meta.span.start_line > 0 && meta.span.start_col > 0 => meta,
            Some(meta) if meta.origin != AstOrigin::User => meta,
            _ => self.fallback,
        }
    }

    fn source(&mut self, meta: Option<AstNodeMeta>, kind: &str, role: &str) -> CfgSource {
        let meta = self.meta_or_fallback(meta);
        let occurrence_key = format!("{kind}:{role}:{}", meta.origin.kind());
        let occurrence = self.role_occurrences.entry(occurrence_key).or_default();
        let stable_role = if *occurrence == 0 {
            role.to_string()
        } else {
            // This discriminator is used only for metadata-indistinguishable
            // generated siblings. Source-positioned user nodes never depend on it.
            format!("{role}.same.{occurrence}")
        };
        *occurrence += 1;
        let node = self.ids.anonymous(
            &self.owner,
            kind,
            &stable_role,
            Some(meta.span),
            meta.origin,
            &mut self.errors,
        );
        CfgSource {
            node,
            span: meta.span,
            origin: meta.origin,
        }
    }

    fn new_block(&mut self, meta: Option<AstNodeMeta>, role: &str) -> BasicBlockId {
        let source = self.source(meta, &format!("cfg.block.{role}"), role);
        let id = BasicBlockId(source.node.clone());
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
        meta: Option<AstNodeMeta>,
        kind: CfgPointKind,
        role: &str,
    ) -> NodeId {
        self.point_with_data(block, meta, kind, role, Vec::new(), Vec::new())
    }

    fn point_with_data(
        &mut self,
        block: &BasicBlockId,
        meta: Option<AstNodeMeta>,
        kind: CfgPointKind,
        role: &str,
        uses: Vec<String>,
        defs: Vec<String>,
    ) -> NodeId {
        let reads = uses.clone();
        let writes = defs.clone();
        self.point_with_accesses(block, meta, kind, role, uses, defs, reads, writes)
    }

    #[allow(clippy::too_many_arguments)]
    fn point_with_accesses(
        &mut self,
        block: &BasicBlockId,
        meta: Option<AstNodeMeta>,
        kind: CfgPointKind,
        role: &str,
        mut uses: Vec<String>,
        mut defs: Vec<String>,
        mut reads: Vec<String>,
        mut writes: Vec<String>,
    ) -> NodeId {
        uses.sort();
        uses.dedup();
        defs.sort();
        defs.dedup();
        reads.sort();
        reads.dedup();
        writes.sort();
        writes.dedup();
        let source = self.source(meta, &format!("cfg.point.{role}"), role);
        let node = source.node.clone();
        if let Some(block) = self.blocks.get_mut(block) {
            block.points.push(CfgPoint {
                source,
                kind,
                uses,
                defs,
                reads,
                writes,
            });
        }
        node
    }

    fn edge(
        &mut self,
        from: &BasicBlockId,
        to: &BasicBlockId,
        kind: EdgeKind,
        meta: Option<AstNodeMeta>,
        role: &str,
    ) -> EdgeId {
        let source = self.source(meta, &format!("cfg.edge.{role}"), role);
        let id = EdgeId(source.node.clone());
        if self.edges.contains_key(&id) {
            self.errors.push(Diagnostic::error(
                format!("duplicate CFG edge identity '{}'", id.0 .0),
                source.span,
            ));
        }
        self.edges.insert(
            id.clone(),
            CfgEdge {
                id: id.clone(),
                from: from.clone(),
                to: to.clone(),
                kind,
                source,
            },
        );
        id
    }

    fn terminate(&mut self, block: &BasicBlockId, terminator: Terminator) {
        let Some(block) = self.blocks.get_mut(block) else {
            self.errors.push(Diagnostic::error(
                "attempted to terminate a missing CFG block".to_string(),
                self.fallback.span,
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
        meta: Option<AstNodeMeta>,
        role: &str,
    ) {
        let edge = self.edge(from, to, kind, meta, role);
        self.terminate(from, Terminator::Goto { edge });
    }

    fn ensure_current(
        &mut self,
        current: Option<BasicBlockId>,
        meta: Option<AstNodeMeta>,
        role: &str,
    ) -> BasicBlockId {
        current.unwrap_or_else(|| {
            let block = self.new_block(meta, &format!("unreachable.{role}"));
            self.orphan_roots.insert(block.clone());
            block
        })
    }

    fn lower_block(
        &mut self,
        block: &Block,
        mut current: Option<BasicBlockId>,
        role: &str,
    ) -> Option<BasicBlockId> {
        for stmt in block {
            let block_id = self.ensure_current(current, stmt.meta(), role);
            current = self.lower_stmt(stmt, block_id, role);
        }
        current
    }

    fn lower_stmt(
        &mut self,
        stmt: &Stmt,
        current: BasicBlockId,
        role: &str,
    ) -> Option<BasicBlockId> {
        let meta = stmt.meta();
        match stmt.unlocated() {
            Stmt::Let { pat, init, .. } => {
                let current = init
                    .as_ref()
                    .and_then(|expr| {
                        self.lower_expr(expr, current.clone(), &format!("{role}.let.init"))
                    })
                    .unwrap_or(current);
                self.point_with_data(
                    &current,
                    meta,
                    CfgPointKind::Binding,
                    "stmt.let",
                    Vec::new(),
                    pattern_names(pat),
                );
                Some(current)
            }
            Stmt::Return(value) => {
                let current = value
                    .as_ref()
                    .and_then(|expr| {
                        self.lower_expr(expr, current.clone(), &format!("{role}.return"))
                    })
                    .unwrap_or(current);
                let value_node = value.as_ref().map(|_| {
                    self.point(&current, meta, CfgPointKind::ResourceAction, "stmt.return")
                });
                self.terminate(
                    &current,
                    Terminator::Return {
                        value: value_node,
                        implicit: false,
                    },
                );
                None
            }
            Stmt::Break(value) => {
                let current = value
                    .as_ref()
                    .and_then(|expr| {
                        self.lower_expr(expr, current.clone(), &format!("{role}.break"))
                    })
                    .unwrap_or(current);
                self.point(&current, meta, CfgPointKind::Statement, "stmt.break");
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
                    meta,
                    "break",
                );
                self.terminate(&current, Terminator::Break { edge });
                None
            }
            Stmt::Continue => {
                self.point(&current, meta, CfgPointKind::Statement, "stmt.continue");
                let Some(loop_) = self.loops.last().cloned() else {
                    self.terminate(&current, Terminator::Diverge);
                    return None;
                };
                let edge = self.edge(
                    &current,
                    &loop_.continue_target,
                    EdgeKind::Continue,
                    meta,
                    "continue",
                );
                self.terminate(&current, Terminator::Continue { edge });
                None
            }
            Stmt::Expr(expr) => self.lower_expr(expr, current, &format!("{role}.expr")),
            Stmt::If { cond, then_, else_ } => self.lower_if(
                cond,
                then_,
                else_.as_ref(),
                current,
                meta,
                &format!("{role}.if"),
            ),
            Stmt::While { cond, body } => {
                self.lower_conditional_loop(cond, body, current, meta, &format!("{role}.while"))
            }
            Stmt::WhileLet { init, body, .. } => {
                self.lower_conditional_loop(init, body, current, meta, &format!("{role}.while_let"))
            }
            Stmt::Loop(body) => {
                self.lower_infinite_loop(body, current, meta, &format!("{role}.loop"))
            }
            Stmt::For { iterable, body, .. } => {
                self.lower_conditional_loop(iterable, body, current, meta, &format!("{role}.for"))
            }
            Stmt::Assign { target, value } => {
                let current = self
                    .lower_expr(value, current, &format!("{role}.assign.value"))
                    .unwrap_or_else(|| self.new_block(meta, "unreachable.assign"));
                let current = self
                    .lower_expr(target, current, &format!("{role}.assign.target"))
                    .unwrap_or_else(|| self.new_block(meta, "unreachable.assign.target"));
                self.point_with_accesses(
                    &current,
                    meta,
                    CfgPointKind::Assignment,
                    "stmt.assign",
                    Vec::new(),
                    assigned_binding_name(target).into_iter().collect(),
                    Vec::new(),
                    place_spelling(target).into_iter().collect(),
                );
                Some(current)
            }
            Stmt::Drop(expr) => {
                let current = self
                    .lower_expr(expr, current, &format!("{role}.drop"))
                    .unwrap_or_else(|| self.new_block(meta, "unreachable.drop"));
                self.point(&current, meta, CfgPointKind::ResourceAction, "stmt.drop");
                Some(current)
            }
            Stmt::SharedLet { name, init, .. } => {
                let current = self
                    .lower_expr(init, current, &format!("{role}.shared.init"))
                    .unwrap_or_else(|| self.new_block(meta, "unreachable.shared"));
                self.point_with_data(
                    &current,
                    meta,
                    CfgPointKind::Binding,
                    "stmt.shared_let",
                    Vec::new(),
                    vec![name.clone()],
                );
                Some(current)
            }
            Stmt::Delegate { expr, .. } => {
                let current = self
                    .lower_expr(expr, current, &format!("{role}.delegate"))
                    .unwrap_or_else(|| self.new_block(meta, "unreachable.delegate"));
                self.point(
                    &current,
                    meta,
                    CfgPointKind::ResourceAction,
                    "stmt.delegate",
                );
                Some(current)
            }
            Stmt::Pinned {
                expr,
                timeout,
                body,
                ..
            } => {
                let current = self
                    .lower_expr(expr, current, &format!("{role}.pinned.expr"))
                    .unwrap_or_else(|| self.new_block(meta, "unreachable.pinned"));
                let current = timeout
                    .as_ref()
                    .and_then(|expr| {
                        self.lower_expr(expr, current.clone(), &format!("{role}.pinned.timeout"))
                    })
                    .unwrap_or(current);
                self.lower_block(body, Some(current), &format!("{role}.pinned.body"))
            }
            Stmt::Block(block)
            | Stmt::Arena(block)
            | Stmt::Unsafe(block)
            | Stmt::OnFailure(block)
            | Stmt::Do(block)
            | Stmt::Parasteps(block) => {
                self.lower_block(block, Some(current), &format!("{role}.block"))
            }
            Stmt::Alloc { body, .. } => {
                self.lower_block(body, Some(current), &format!("{role}.alloc"))
            }
            Stmt::Requires(expr, _) | Stmt::Ensures(expr, _) | Stmt::Invariant(expr, _) => {
                self.lower_expr(expr, current, &format!("{role}.contract"))
            }
            Stmt::Math(exprs) => {
                let mut current = Some(current);
                for expr in exprs {
                    let block = self.ensure_current(current, expr.meta(), "math");
                    current = self.lower_expr(expr, block, &format!("{role}.math"));
                }
                current
            }
            Stmt::Func(_) => {
                self.point(&current, meta, CfgPointKind::Statement, "stmt.nested_func");
                Some(current)
            }
            Stmt::Desc(..)
            | Stmt::Rule(..)
            | Stmt::MmsBlock { .. }
            | Stmt::Ellipsis
            | Stmt::Located { .. } => {
                self.point(&current, meta, CfgPointKind::Statement, "stmt.noop");
                Some(current)
            }
        }
    }

    fn lower_expr(
        &mut self,
        expr: &Expr,
        current: BasicBlockId,
        role: &str,
    ) -> Option<BasicBlockId> {
        match expr.unlocated() {
            Expr::If { cond, then_, else_ } => self.lower_if(
                cond,
                then_,
                else_.as_ref(),
                current,
                expr.meta(),
                &format!("{role}.if_expr"),
            ),
            Expr::Match(scrutinee, arms) => self.lower_match(
                scrutinee,
                arms,
                current,
                expr.meta(),
                &format!("{role}.match"),
            ),
            Expr::Block(block)
            | Expr::Arena(block)
            | Expr::Comptime(block)
            | Expr::Quote(block) => {
                self.lower_block(block, Some(current), &format!("{role}.block_expr"))
            }
            _ => {
                let mut uses = Vec::new();
                crate::core::Checker::collect_uses_in_expr(expr, &mut uses);
                let mut reads = Vec::new();
                collect_read_places(expr, &mut reads);
                self.point_with_accesses(
                    &current,
                    expr.meta(),
                    CfgPointKind::Expression,
                    expr_kind(expr),
                    uses,
                    Vec::new(),
                    reads,
                    Vec::new(),
                );
                Some(current)
            }
        }
    }

    fn lower_if(
        &mut self,
        cond: &Expr,
        then_: &Block,
        else_: Option<&Block>,
        current: BasicBlockId,
        meta: Option<AstNodeMeta>,
        role: &str,
    ) -> Option<BasicBlockId> {
        let cond_block = self
            .lower_expr(cond, current, &format!("{role}.condition"))
            .unwrap_or_else(|| self.new_block(meta, "unreachable.if.condition"));
        let condition = self.point(
            &cond_block,
            cond.meta(),
            CfgPointKind::Condition,
            &format!("{role}.condition"),
        );
        let then_block = self.new_block(
            then_.first().and_then(Stmt::meta).or(meta),
            &format!("{role}.then"),
        );
        let else_block = self.new_block(
            else_
                .and_then(|block| block.first())
                .and_then(Stmt::meta)
                .or(meta),
            &format!("{role}.else"),
        );
        let join = self.new_block(meta, &format!("{role}.join"));
        let then_edge = self.edge(
            &cond_block,
            &then_block,
            EdgeKind::Then,
            meta,
            &format!("{role}.then"),
        );
        let else_edge = self.edge(
            &cond_block,
            &else_block,
            EdgeKind::Else,
            meta,
            &format!("{role}.else"),
        );
        self.terminate(
            &cond_block,
            Terminator::Branch {
                condition,
                then_edge,
                else_edge,
            },
        );

        let then_end = self.lower_block(then_, Some(then_block), &format!("{role}.then"));
        let else_end = match else_ {
            Some(block) => self.lower_block(block, Some(else_block), &format!("{role}.else")),
            None => Some(else_block),
        };
        let mut falls_through = false;
        if let Some(end) = then_end {
            self.goto(
                &end,
                &join,
                EdgeKind::Fallthrough,
                meta,
                &format!("{role}.then.join"),
            );
            falls_through = true;
        }
        if let Some(end) = else_end {
            self.goto(
                &end,
                &join,
                EdgeKind::Fallthrough,
                meta,
                &format!("{role}.else.join"),
            );
            falls_through = true;
        }
        falls_through.then_some(join)
    }

    fn lower_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[crate::ast::MatchArm],
        current: BasicBlockId,
        meta: Option<AstNodeMeta>,
        role: &str,
    ) -> Option<BasicBlockId> {
        let dispatch = self
            .lower_expr(scrutinee, current, &format!("{role}.scrutinee"))
            .unwrap_or_else(|| self.new_block(meta, "unreachable.match.scrutinee"));
        let scrutinee_node = self.point(
            &dispatch,
            scrutinee.meta(),
            CfgPointKind::Condition,
            &format!("{role}.scrutinee"),
        );
        let join = self.new_block(meta, &format!("{role}.join"));
        let mut edges = Vec::new();
        let mut falls_through = false;
        for arm in arms {
            let arm_block = self.new_block(Some(arm.meta), &format!("{role}.arm"));
            edges.push(self.edge(
                &dispatch,
                &arm_block,
                EdgeKind::MatchArm,
                Some(arm.meta),
                &format!("{role}.arm"),
            ));
            let end = self.lower_expr(&arm.body, arm_block, &format!("{role}.arm"));
            if let Some(end) = end {
                self.goto(
                    &end,
                    &join,
                    EdgeKind::Fallthrough,
                    Some(arm.meta),
                    &format!("{role}.arm.join"),
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
                scrutinee: scrutinee_node,
                arms: edges,
            },
        );
        falls_through.then_some(join)
    }

    fn lower_conditional_loop(
        &mut self,
        cond: &Expr,
        body: &Block,
        current: BasicBlockId,
        meta: Option<AstNodeMeta>,
        role: &str,
    ) -> Option<BasicBlockId> {
        let header = self.new_block(meta, &format!("{role}.header"));
        let body_entry = self.new_block(
            body.first().and_then(Stmt::meta).or(meta),
            &format!("{role}.body"),
        );
        let exit = self.new_block(meta, &format!("{role}.exit"));
        self.goto(
            &current,
            &header,
            EdgeKind::Fallthrough,
            meta,
            &format!("{role}.enter"),
        );
        let cond_end = self
            .lower_expr(cond, header.clone(), &format!("{role}.condition"))
            .unwrap_or(header.clone());
        let condition = self.point(
            &cond_end,
            cond.meta(),
            CfgPointKind::Condition,
            &format!("{role}.condition"),
        );
        let body_edge = self.edge(
            &cond_end,
            &body_entry,
            EdgeKind::LoopBody,
            meta,
            &format!("{role}.body"),
        );
        let exit_edge = self.edge(
            &cond_end,
            &exit,
            EdgeKind::LoopExit,
            meta,
            &format!("{role}.exit"),
        );
        self.terminate(
            &cond_end,
            Terminator::Branch {
                condition,
                then_edge: body_edge,
                else_edge: exit_edge,
            },
        );
        self.loops.push(LoopContext {
            break_target: exit.clone(),
            continue_target: header.clone(),
            break_seen: false,
        });
        let body_end = self.lower_block(body, Some(body_entry), &format!("{role}.body"));
        self.loops.pop();
        if let Some(end) = body_end {
            self.goto(
                &end,
                &header,
                EdgeKind::Backedge,
                meta,
                &format!("{role}.backedge"),
            );
        }
        Some(exit)
    }

    fn lower_infinite_loop(
        &mut self,
        body: &Block,
        current: BasicBlockId,
        meta: Option<AstNodeMeta>,
        role: &str,
    ) -> Option<BasicBlockId> {
        let header = self.new_block(
            body.first().and_then(Stmt::meta).or(meta),
            &format!("{role}.header"),
        );
        let exit = self.new_block(meta, &format!("{role}.exit"));
        self.goto(
            &current,
            &header,
            EdgeKind::Fallthrough,
            meta,
            &format!("{role}.enter"),
        );
        self.loops.push(LoopContext {
            break_target: exit.clone(),
            continue_target: header.clone(),
            break_seen: false,
        });
        let body_end = self.lower_block(body, Some(header.clone()), &format!("{role}.body"));
        let break_seen = self.loops.pop().is_some_and(|loop_| loop_.break_seen);
        if let Some(end) = body_end {
            self.goto(
                &end,
                &header,
                EdgeKind::Backedge,
                meta,
                &format!("{role}.backedge"),
            );
        }
        break_seen.then_some(exit)
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
                let value = self
                    .blocks
                    .get(&current)
                    .and_then(|block| block.points.last())
                    .map(|point| point.source.node.clone());
                self.terminate(
                    &current,
                    Terminator::Return {
                        value,
                        implicit: true,
                    },
                );
            }
        }
        for orphan in &self.orphan_roots.clone() {
            if self
                .blocks
                .get(orphan)
                .is_some_and(|block| block.terminator.is_none())
            {
                self.terminate(orphan, Terminator::Unreachable);
            }
        }
        for block in self.blocks.values_mut() {
            if block.terminator.is_none() {
                block.terminator = Some(Terminator::Unreachable);
            }
        }
        let blocks: BTreeMap<_, _> = self
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
                            .expect("CFG finalization installs terminator"),
                    },
                )
            })
            .collect();
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

fn lower_callable(
    owner: NodeId,
    body: &Block,
    meta: AstNodeMeta,
    sources: &SourceRegistry,
) -> Result<CallableCfg, Vec<Diagnostic>> {
    let mut lowerer = Lowerer::new(owner, sources, meta);
    let entry = lowerer.new_block(Some(meta), "entry");
    let current = lowerer.lower_block(body, Some(entry.clone()), "body");
    lowerer.finish(entry, current)
}

fn qualify(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}::{name}")
    }
}

fn collect_nested(
    body: &Block,
    owner: &NodeId,
    sources: &SourceRegistry,
    out: &mut BTreeMap<NodeId, CallableCfg>,
    errors: &mut Vec<Diagnostic>,
) {
    for stmt in body {
        match stmt.unlocated() {
            Stmt::Func(function) => {
                let nested_owner = nested_function_owner(owner, function);
                match lower_callable(nested_owner.clone(), &function.body, function.meta, sources) {
                    Ok(cfg) => {
                        out.insert(nested_owner.clone(), cfg);
                    }
                    Err(mut nested_errors) => errors.append(&mut nested_errors),
                }
                collect_nested(&function.body, &nested_owner, sources, out, errors);
            }
            Stmt::If { then_, else_, .. } => {
                collect_nested(then_, owner, sources, out, errors);
                if let Some(else_) = else_ {
                    collect_nested(else_, owner, sources, out, errors);
                }
            }
            Stmt::While { body, .. }
            | Stmt::WhileLet { body, .. }
            | Stmt::For { body, .. }
            | Stmt::Pinned { body, .. }
            | Stmt::Alloc { body, .. }
            | Stmt::Block(body)
            | Stmt::Arena(body)
            | Stmt::Unsafe(body)
            | Stmt::OnFailure(body)
            | Stmt::Do(body)
            | Stmt::Parasteps(body)
            | Stmt::Loop(body) => collect_nested(body, owner, sources, out, errors),
            _ => {}
        }
    }
}

fn collect_items(
    items: &[Item],
    module: &str,
    sources: &SourceRegistry,
    out: &mut BTreeMap<NodeId, CallableCfg>,
    errors: &mut Vec<Diagnostic>,
) {
    for item in items {
        match item {
            Item::Module(module_def) => {
                let nested = qualify(module, &module_def.name);
                collect_items(&module_def.items, &nested, sources, out, errors);
            }
            Item::Func(function) => {
                let owner = NodeId(format!("function:{}", qualify(module, &function.name)));
                match lower_callable(owner.clone(), &function.body, function.meta, sources) {
                    Ok(cfg) => {
                        out.insert(owner.clone(), cfg);
                    }
                    Err(mut cfg_errors) => errors.append(&mut cfg_errors),
                }
                collect_nested(&function.body, &owner, sources, out, errors);
            }
            Item::Actor(actor) => {
                let actor_name = qualify(module, &actor.name);
                for method in &actor.methods {
                    let owner = NodeId(format!("function:{actor_name}::{}", method.name));
                    match lower_callable(owner.clone(), &method.body, method.meta, sources) {
                        Ok(cfg) => {
                            out.insert(owner.clone(), cfg);
                        }
                        Err(mut cfg_errors) => errors.append(&mut cfg_errors),
                    }
                    collect_nested(&method.body, &owner, sources, out, errors);
                }
            }
            Item::Impl(impl_def) => {
                let qualified = qualify(
                    module,
                    &format!("{}:for:{}", impl_def.trait_name, impl_def.type_name),
                );
                for method in &impl_def.methods {
                    let owner = impl_method_owner(&qualified, method);
                    match lower_callable(owner.clone(), &method.body, method.meta, sources) {
                        Ok(cfg) => {
                            out.insert(owner.clone(), cfg);
                        }
                        Err(mut cfg_errors) => errors.append(&mut cfg_errors),
                    }
                    collect_nested(&method.body, &owner, sources, out, errors);
                }
            }
            Item::Flow(flow) => {
                let flow_name = qualify(module, &flow.name);
                for transition in &flow.transitions {
                    let Some(body) = &transition.body else {
                        continue;
                    };
                    let owner = NodeId(format!(
                        "transition:{flow_name}::{}::{}",
                        transition.name, transition.from_state
                    ));
                    match lower_callable(owner.clone(), body, transition.meta, sources) {
                        Ok(cfg) => {
                            out.insert(owner.clone(), cfg);
                        }
                        Err(mut cfg_errors) => errors.append(&mut cfg_errors),
                    }
                    collect_nested(body, &owner, sources, out, errors);
                }
            }
            Item::Type(_)
            | Item::Cap(_)
            | Item::Trait(_)
            | Item::ExternBlock(_)
            | Item::Const { .. }
            | Item::Protocol(_)
            | Item::Session(_) => {}
        }
    }
}

pub fn lower_file(file: &File) -> Result<BTreeMap<NodeId, CallableCfg>, Vec<Diagnostic>> {
    let mut cfgs = BTreeMap::new();
    let mut errors = Vec::new();
    collect_items(&file.items, "", &file.sources, &mut cfgs, &mut errors);
    if errors.is_empty() {
        Ok(cfgs)
    } else {
        Err(errors)
    }
}

fn expr_kind(expr: &Expr) -> &'static str {
    match expr.unlocated() {
        Expr::Literal(Lit::Int(_)) => "expr.int",
        Expr::Literal(Lit::Float(_)) => "expr.float",
        Expr::Literal(Lit::Bool(_)) => "expr.bool",
        Expr::Literal(Lit::String(_)) | Expr::Literal(Lit::FString(_)) => "expr.string",
        Expr::Literal(Lit::Unit) => "expr.unit",
        Expr::Ident(_) => "expr.ident",
        Expr::Binary(..) => "expr.binary",
        Expr::Unary(..) => "expr.unary",
        Expr::Call(..) => "expr.call",
        Expr::Field(..) => "expr.field",
        Expr::Index(..) => "expr.index",
        Expr::Tuple(..) => "expr.tuple",
        Expr::List(..) => "expr.list",
        Expr::Comprehension { .. } => "expr.comprehension",
        Expr::Match(..) => "expr.match",
        Expr::Record { .. } => "expr.record",
        Expr::Block(..) => "expr.block",
        Expr::Try(..) => "expr.try",
        Expr::OptionalChain(..) => "expr.optional_chain",
        Expr::Spawn(..) => "expr.spawn",
        Expr::Await(..) => "expr.await",
        Expr::Quote(..) => "expr.quote",
        Expr::QuoteInterpolate(..) => "expr.quote_interpolate",
        Expr::Comptime(..) => "expr.comptime",
        Expr::TypeOf(..) => "expr.typeof",
        Expr::TypeInfo(..) => "expr.typeinfo",
        Expr::If { .. } => "expr.if",
        Expr::Lambda { .. } => "expr.lambda",
        Expr::Old(..) => "expr.old",
        Expr::SliceExpr { .. } => "expr.slice",
        Expr::Range { .. } => "expr.range",
        Expr::Turbofish(..) => "expr.turbofish",
        Expr::TupleIndex(..) => "expr.tuple_index",
        Expr::Arena(..) => "expr.arena",
        Expr::MapLiteral { .. } => "expr.map",
        Expr::SetLiteral(..) => "expr.set",
        Expr::NamedArg(..) => "expr.named_arg",
        Expr::Cast(..) => "expr.cast",
        Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
    }
}

fn pattern_names(pattern: &Pattern) -> Vec<String> {
    fn collect(pattern: &Pattern, out: &mut Vec<String>) {
        match &pattern.kind {
            PatternKind::Variable(name) => out.push(name.clone()),
            PatternKind::Constructor(_, fields) => {
                for (_, pattern) in fields {
                    collect(pattern, out);
                }
            }
            PatternKind::Tuple(values) | PatternKind::Array(values) => {
                for pattern in values {
                    collect(pattern, out);
                }
            }
            PatternKind::Slice(values, rest) => {
                for pattern in values {
                    collect(pattern, out);
                }
                if let Some(rest) = rest {
                    collect(rest, out);
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) => {}
        }
    }
    let mut names = Vec::new();
    collect(pattern, &mut names);
    names
}

fn assigned_binding_name(expr: &Expr) -> Option<String> {
    match expr.unlocated() {
        Expr::Ident(name) => Some(name.clone()),
        _ => None,
    }
}

fn place_spelling(expr: &Expr) -> Option<String> {
    match expr.unlocated() {
        Expr::Ident(name) => Some(name.clone()),
        Expr::Field(base, field) => Some(format!("{}.{}", place_spelling(base)?, field)),
        Expr::TupleIndex(base, index) => Some(format!("{}.{}", place_spelling(base)?, index)),
        Expr::Index(base, index) => {
            let index = match index.unlocated() {
                Expr::Literal(Lit::Int(value)) => value.to_string(),
                _ => "*".to_string(),
            };
            Some(format!("{}[{index}]", place_spelling(base)?))
        }
        Expr::Unary(crate::ast::UnOp::Deref, base) => Some(format!("*{}", place_spelling(base)?)),
        _ => None,
    }
}

fn collect_read_places(expr: &Expr, places: &mut Vec<String>) {
    if let Some(place) = place_spelling(expr) {
        places.push(place);
        collect_index_operands(expr, places);
        return;
    }
    match expr.unlocated() {
        Expr::Unary(_, inner)
        | Expr::Try(inner)
        | Expr::Spawn(inner)
        | Expr::Await(inner)
        | Expr::Cast(inner, _)
        | Expr::NamedArg(_, inner)
        | Expr::OptionalChain(inner, _) => collect_read_places(inner, places),
        Expr::Binary(_, left, right) => {
            collect_read_places(left, places);
            collect_read_places(right, places);
        }
        Expr::Call(callee, args) => {
            collect_read_places(callee, places);
            for arg in args {
                collect_read_places(arg, places);
            }
        }
        Expr::Tuple(values) | Expr::List(values) | Expr::SetLiteral(values) => {
            for value in values {
                collect_read_places(value, places);
            }
        }
        Expr::Record { fields, .. } => {
            for field in fields {
                collect_read_places(&field.value, places);
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            collect_read_places(expr, places);
            collect_read_places(iter, places);
            if let Some(guard) = guard {
                collect_read_places(guard, places);
            }
        }
        Expr::Range { start, end } => {
            collect_read_places(start, places);
            collect_read_places(end, places);
        }
        Expr::SliceExpr { target, start, end } => {
            collect_read_places(target, places);
            if let Some(start) = start {
                collect_read_places(start, places);
            }
            if let Some(end) = end {
                collect_read_places(end, places);
            }
        }
        Expr::MapLiteral { entries } => {
            for (key, value) in entries {
                collect_read_places(key, places);
                collect_read_places(value, places);
            }
        }
        Expr::Turbofish(_, _, args) => {
            for arg in args {
                collect_read_places(arg, places);
            }
        }
        Expr::Literal(_)
        | Expr::Ident(_)
        | Expr::Field(_, _)
        | Expr::TupleIndex(_, _)
        | Expr::Index(_, _)
        | Expr::Block(_)
        | Expr::If { .. }
        | Expr::Match(_, _)
        | Expr::Lambda { .. }
        | Expr::Arena(_)
        | Expr::Comptime(_)
        | Expr::Quote(_)
        | Expr::QuoteInterpolate(_)
        | Expr::Old(_)
        | Expr::TypeOf(_)
        | Expr::TypeInfo(_)
        | Expr::Located { .. } => {}
    }
}

fn collect_index_operands(expr: &Expr, places: &mut Vec<String>) {
    match expr.unlocated() {
        Expr::Index(base, index) => {
            collect_index_operands(base, places);
            collect_read_places(index, places);
        }
        Expr::Field(base, _) | Expr::TupleIndex(base, _) | Expr::Unary(_, base) => {
            collect_index_operands(base, places);
        }
        _ => {}
    }
}
