use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{
    MatchArm, Permission, ResolvedBlock, ResolvedCall, ResolvedExpr, ResolvedExprKind,
    ResolvedFStringPart, ResolvedIndex, ResolvedLocal, ResolvedPattern, ResolvedPatternKind,
    ResolvedPlace, ResolvedProjection, ResolvedSignature, ResolvedStmt, ResolvedStmtKind,
    ResolvedUnaryOp, ResolvedValueProjection,
};
use crate::core::{
    CanonicalActionKind, CanonicalResourceAction, CfgLocation, IndexProjection, Loan, LoanId,
    LoanKind, LocalId, NodeId, Place, PlaceProjection, ResolvedBody, ResolvedLocalId, ResolvedType,
    ResolvedTypeId, ResolvedTypeTable, ResourceAnalysis, ResourceId,
};
use crate::diagnostic::Diagnostic;

use super::{dataflow::analyze_canonical, CallableCfg};

struct ActionDraft {
    kind: CanonicalActionKind,
    resource: ResourceId,
    source: Option<Place>,
    target: Option<Place>,
    loan: Option<LoanId>,
}

struct ActionEmitter<'a> {
    cfg: &'a CallableCfg,
    body: &'a ResolvedBody,
    signature: &'a ResolvedSignature,
    types: &'a ResolvedTypeTable,
    locations: BTreeMap<NodeId, CfgLocation>,
    /// Resource catalog: linear local → the resource identities it currently
    /// owns. A single binding may own SEVERAL resources after an aggregate
    /// merge (`let x = (a, b)` moves both atoms into `x`), so the value is a
    /// vector in construction order. Wave-2 (audit G-1): single-valued
    /// identity stranded every source past the first and could never be
    /// re-keyed by reassignment.
    resources: BTreeMap<ResolvedLocalId, Vec<ResourceId>>,
    actions: Vec<CanonicalResourceAction>,
    loans: Vec<Loan>,
    errors: Vec<Diagnostic>,
    /// Audit 2026-08-05 (wave-2, H-6): anonymous temporary borrows
    /// (`inc(&mut x)`) have no named reference, so loan liveness cannot end
    /// them. They are parked here and receive a synthesized BorrowEnd at the
    /// enclosing statement's terminating CFG point (mirroring named-borrow
    /// NLL). Entries are (loan id, borrowed resource).
    pending_anonymous_loans: Vec<(LoanId, ResourceId)>,
    /// 0.31.22 Drop/Transition IR 防漏网断言：跟踪已消费的资源
    /// 用于 debug_assert 检测二次消费 bug
    /// P2/P1-6: Double-consumption debug assertion infrastructure.
    /// Currently disabled (false positives on alias/branch scenarios).
    /// Re-enabling requires CFG path analysis to distinguish:
    /// - Same resource consumed twice in one basic block (real bug)
    /// - Same resource consumed in different branches (legal)
    /// - Alias-induced duplicate tracking (legal)
    /// Linear-resource double-drop tracker (RESOURCE-LINEAR-001 debug signal).
    /// Referenced by the always-on mimi_assert! in emit_drop — must exist in
    /// ALL build profiles (a previous #[cfg(debug_assertions)] gate broke
    /// `cargo build --release`: mimi_assert! is not compiled out in release).
    consumed_resources: BTreeSet<ResourceId>,
    /// 0.36.43: E0304-rejected element extractions (`v[0]` / `v[1..]` /
    /// `(a, b).0` on non-droppable linear containers). The rejection is
    /// diagnostic-only by design, but the surrounding lowering kept pairing
    /// the extracted place into binds/calls/drops — fabricating a transfer
    /// for a value that never moved, so a later legitimate `drop(v)` hit the
    /// RESOURCE-LINEAR-001 double-drop debug signal. Consumers skip places
    /// in this set (error-path hygiene; the function is already invalid).
    rejected_extraction_places: BTreeSet<Place>,
    /// 0.36.43: set by reject_index_read_extraction whenever THIS visited
    /// expression was rejected; the Bind arm uses it to skip pairing an
    /// initializer whose extraction already failed (the E0304 stands alone).
    last_visit_rejected: bool,
    /// 0.36.46: 已做定向头提取（`xs[0]`）的容器基——元素认领后容器保留余部
    /// 义务，每容器至多一次索引提取（二次认领同一位置 = 超认领 → E0304）。
    extracted_containers: BTreeSet<ResolvedLocalId>,
    /// 0.36.46: 绑定初始化器访问上下文中放行的定向提取基——Bind 臂在
    /// visit_expr 后据此做专门的"元素 Introduce + 容器余部保留"记账。
    directional_extraction_base: Option<ResolvedLocalId>,
    /// 0.36.46: 当前是否位于绑定初始化器访问中——定向提取仅对 let-绑定面开
    /// （调用实参 `sink(xs[0])` / 其他位置保持 fail-closed E0304）。
    in_bind_initializer: bool,
}

impl<'a> ActionEmitter<'a> {
    fn new(
        cfg: &'a CallableCfg,
        body: &'a ResolvedBody,
        signature: &'a ResolvedSignature,
        types: &'a ResolvedTypeTable,
    ) -> Self {
        let locations = cfg
            .blocks
            .iter()
            .flat_map(|(block, value)| {
                value.points.iter().map(move |point| {
                    (
                        point.source.node.clone(),
                        CfgLocation {
                            block: block.clone(),
                            point: point.source.node.clone(),
                            edge: None,
                        },
                    )
                })
            })
            .collect();
        Self {
            cfg,
            body,
            signature,
            types,
            locations,
            resources: BTreeMap::new(),
            actions: Vec::new(),
            loans: Vec::new(),
            errors: Vec::new(),
            pending_anonymous_loans: Vec::new(),
            consumed_resources: BTreeSet::new(),
            rejected_extraction_places: BTreeSet::new(),
            last_visit_rejected: false,
            extracted_containers: BTreeSet::new(),
            directional_extraction_base: None,
            in_bind_initializer: false,
        }
    }

    fn emit(mut self) -> Result<ResourceAnalysis, Vec<Diagnostic>> {
        self.build_resource_catalog();
        self.reject_linear_callable_captures();
        self.introduce_parameters();
        self.visit_block(&self.body.root, true);
        if self.errors.is_empty() {
            // 0.31.16: collect flow state resources as auto-droppable.
            // Flow states represent data that can be safely discarded at
            // scope exit, unlike Cap/SessionChan which require explicit
            // consumption.
            // P0-5: containers of flow states (Result/Option/Tuple/Array/Slice)
            // are also droppable iff all linear elements inside are flow states.
            let droppable: BTreeSet<ResourceId> = self
                .resources
                .iter()
                .filter(|(local, _)| {
                    self.body
                        .locals
                        .get(local)
                        .is_some_and(|l| self.is_linear(&l.ty) && self.is_droppable_type(&l.ty))
                })
                .flat_map(|(_, resources)| resources.iter().cloned())
                .collect();
            analyze_canonical(
                self.cfg,
                self.actions,
                self.loans,
                &BTreeMap::new(),
                &droppable,
            )
        } else {
            Err(self.errors)
        }
    }

    /// v0.34.8 (SD-1 tail): check whether a resolved type is a flow state.
    /// Delegates to `NominalTypeId::nominal_is_flow_state` (types.rs — the
    /// single source of truth) instead of repeating the "state:" prefix
    /// string match. Note: intentionally NOT `is_linear` — SessionChan is
    /// linear but is not a flow state and is not auto-droppable.
    fn is_flow_state_resolved(&self, ty: &ResolvedType) -> bool {
        match ty {
            ResolvedType::FlowStateSet { .. } => true,
            ResolvedType::Nominal { item, .. } => item.nominal_is_flow_state(),
            _ => false,
        }
    }

    /// P0-5: check whether a linear type is auto-droppable at scope exit.
    /// Flow states are droppable; containers (Option/Result/Tuple/Array/Slice)
    /// are droppable iff every linear element inside them is also droppable.
    /// Cap/SessionChan are NOT droppable — they require explicit consumption.
    fn is_droppable_type(&self, ty: &ResolvedTypeId) -> bool {
        match self.types.get(ty) {
            Some(ResolvedType::FlowStateSet { .. }) => true,
            // H2 (audit-type 2026-08-03): builtin container nominals follow
            // the same rule as structural containers — droppable iff every
            // linear element is droppable (e.g. List<flow state> yes,
            // List<cap> no). Keep before the generic Nominal arm.
            Some(ResolvedType::Nominal {
                item, arguments, ..
            }) if matches!(
                item.as_str(),
                "builtin:type:List" | "builtin:type:Map" | "builtin:type:Set"
            ) =>
            {
                arguments
                    .iter()
                    .all(|arg| !self.is_linear(arg) || self.is_droppable_type(arg))
            }
            Some(resolved @ ResolvedType::Nominal { .. }) => self.is_flow_state_resolved(resolved),
            Some(ResolvedType::Option(inner)) => self.is_droppable_type(inner),
            Some(ResolvedType::Result { ok, error }) => {
                // Both branches must be droppable for the whole Result to be.
                (!self.is_linear(ok) || self.is_droppable_type(ok))
                    && (!self.is_linear(error) || self.is_droppable_type(error))
            }
            Some(ResolvedType::Tuple(elements)) => elements
                .iter()
                .all(|e| !self.is_linear(e) || self.is_droppable_type(e)),
            Some(ResolvedType::Array { element, .. }) => self.is_droppable_type(element),
            Some(ResolvedType::Slice(inner)) => self.is_droppable_type(inner),
            Some(ResolvedType::Newtype { inner, .. }) => self.is_droppable_type(inner),
            // Cap, SessionChan, and other non-flow-state linear types are NOT droppable.
            _ => false,
        }
    }

    fn reject_linear_callable_captures(&mut self) {
        for capture in &self.body.captures {
            let Some(local) = self.body.locals.get(capture) else {
                continue;
            };
            if self.is_linear(&local.ty) {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0304,
                        format!(
                            "linear resource '{}' is not owned by the current callable",
                            local.display_name
                        ),
                        local.origin.user_span(),
                    )
                    .with_help(
                        "pass the resource as an explicit parameter or transfer it into a closure",
                    ),
                );
            }
        }
    }

    fn build_resource_catalog(&mut self) {
        // 0.31.13 追加 A: transition `self` (first parameter) is implicitly
        // consumed by the transition mechanism — the source state is transformed
        // into the target state. Don't track it as a linear resource that must
        // be explicitly consumed in the body.
        let is_transition = self.signature.owner.0.starts_with("transition:");
        for (idx, parameter) in self.signature.parameters.iter().enumerate() {
            if is_transition && idx == 0 {
                continue;
            }
            if !self.is_linear(&parameter.ty) {
                continue;
            }
            let local = ResolvedLocalId(NodeId(format!("{}/local", parameter.id.0 .0)));
            if self.body.locals.contains_key(&local) {
                self.resources
                    .insert(local.clone(), vec![ResourceId(local.0.clone())]);
            }
        }
        self.catalog_block(&self.body.root);
    }

    fn catalog_block(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    self.catalog_pattern(pattern, initializer.as_ref());
                    if let Some(initializer) = initializer {
                        self.catalog_expr(initializer);
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    self.catalog_expr(condition);
                    self.catalog_block(body);
                }
                ResolvedStmtKind::WhileLet {
                    pattern,
                    initializer,
                    body,
                }
                | ResolvedStmtKind::For {
                    pattern,
                    iterable: initializer,
                    body,
                } => {
                    self.catalog_pattern(pattern, None);
                    self.catalog_expr(initializer);
                    self.catalog_block(body);
                }
                ResolvedStmtKind::IfLet {
                    pattern,
                    initializer,
                    then_block,
                    else_block,
                } => {
                    self.catalog_pattern(pattern, None);
                    self.catalog_expr(initializer);
                    self.catalog_block(then_block);
                    if let Some(else_block) = else_block {
                        self.catalog_block(else_block);
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    self.catalog_block(body);
                }
                ResolvedStmtKind::Pinned {
                    value,
                    binding,
                    body,
                } => {
                    self.catalog_expr(value);
                    if let Some(binding) = binding {
                        if self
                            .body
                            .locals
                            .get(binding)
                            .is_some_and(|local| self.is_linear(&local.ty))
                        {
                            // Audit 2026-08-05 (wave-1 fix 6): mirror the
                            // Bind/split catalog rule — when the pinned value
                            // is a single linear place, the binding takes over
                            // that place's resource identity so a later
                            // Drop/Move on the binding resolves the same fact
                            // the Move retargeted (P1-10 alignment).
                            let expanded = self.expand_sources(&self.capability_places(value));
                            if expanded.len() == 1 {
                                let resource = expanded.into_iter().next().expect("len == 1");
                                self.resources
                                    .entry(binding.clone())
                                    .or_insert(vec![resource]);
                            } else {
                                self.resources
                                    .entry(binding.clone())
                                    .or_insert_with(|| vec![ResourceId(binding.0.clone())]);
                            }
                        }
                    }
                    self.catalog_block(body);
                }
                ResolvedStmtKind::Assign { value, .. }
                | ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => self.catalog_expr(value),
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.catalog_expr(value);
                    }
                }
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.catalog_expr(expression);
                    }
                }
                ResolvedStmtKind::Continue
                | ResolvedStmtKind::Drop(_)
                | ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            self.catalog_expr(result);
        }
    }

    fn catalog_pattern(&mut self, pattern: &ResolvedPattern, initializer: Option<&ResolvedExpr>) {
        // 0.36.46 定向头提取：`let c = xs[0]`——绑定认领一个元素（fresh 身份），
        // 容器保留余部义务（自身身份不动）。目录在此也跳过继承，否则 c 会拿到
        // xs 的资源身份（幻影），稍后的 drop(xs) 撞 double-consume。
        let sources = if self.is_directional_head_binding(initializer) {
            Vec::new()
        } else {
            initializer
                .map(|value| self.capability_places(value))
                .unwrap_or_default()
        };
        let expanded = self.expand_sources(&sources);
        let mut bindings = Vec::new();
        self.linear_bindings(pattern, &mut bindings);
        // Wave-2 (audit C-2/G-1/G-2 + review §1.3): the catalog mirrors the
        // Move/Introduce actions visit_stmt emits so later Drop/Move resolve
        // the exact ResourceIds the dataflow facts are keyed by.
        //
        // * split() shape — a single-element Tuple([receiver]) with one
        //   linear source and >=2 bindings: the first binding inherits the
        //   receiver's resource (visit_stmt Moves receiver → binding₀); the
        //   remaining bindings are the split-out atoms and are cataloged as
        //   fresh introductions (P1-10 capability decision).
        // * equal counts — positional pairing, one resource per binding.
        // * one binding, several resources — an aggregate merge such as
        //   `let x = (a, b)`: the single binding inherits EVERY source
        //   resource (visit_stmt Moves each into it), so a later `drop(x)`
        //   expands into one Drop per owned resource.
        // * anything else — first binding takes the first resource, the rest
        //   are fresh; visit_stmt fail-closes the mismatch (E0304) whenever
        //   obligations would be stranded or mispaired.
        let split_shape = sources.len() == 1
            && bindings.len() >= 2
            && matches!(initializer, Some(value)
                if matches!(&value.kind, ResolvedExprKind::Tuple(values) if values.len() == 1));
        if split_shape {
            let mut bindings = bindings.into_iter();
            if let Some(first) = bindings.next() {
                let resource = expanded
                    .first()
                    .cloned()
                    .unwrap_or_else(|| self.resource_for_place(&sources[0]));
                self.resources.insert(first, vec![resource]);
            }
            for local in bindings {
                self.resources
                    .insert(local.clone(), vec![ResourceId(local.0.clone())]);
            }
        } else if !expanded.is_empty() && expanded.len() == bindings.len() {
            for (index, local) in bindings.into_iter().enumerate() {
                self.resources.insert(local, vec![expanded[index].clone()]);
            }
        } else if self.single_binding(pattern).is_some() && expanded.len() > 1 {
            let binding = bindings.into_iter().next().expect("bindings.len() == 1");
            self.resources.insert(binding, expanded);
        } else if let Some(resource) = expanded.first() {
            let mut bindings = bindings.into_iter();
            if let Some(first) = bindings.next() {
                self.resources.insert(first, vec![resource.clone()]);
            }
            for local in bindings {
                self.resources
                    .insert(local.clone(), vec![ResourceId(local.0.clone())]);
            }
        } else {
            for local in bindings {
                self.resources
                    .insert(local.clone(), vec![ResourceId(local.0.clone())]);
            }
        }
    }

    fn catalog_expr(&mut self, expression: &ResolvedExpr) {
        match &expression.kind {
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.catalog_expr(condition);
                self.catalog_block(then_block);
                self.catalog_block(else_block);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.catalog_expr(scrutinee);
                for arm in arms {
                    self.catalog_pattern(&arm.pattern, None);
                    if let Some(guard) = &arm.guard {
                        self.catalog_expr(guard);
                    }
                    self.catalog_expr(&arm.body);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.catalog_block(block),
            ResolvedExprKind::Comprehension {
                pattern,
                value,
                iterable,
                guard,
            } => {
                self.catalog_pattern(pattern, None);
                self.catalog_expr(iterable);
                if let Some(guard) = guard {
                    self.catalog_expr(guard);
                }
                self.catalog_expr(value);
            }
            _ => self.for_each_expr_child(expression, |this, child| this.catalog_expr(child)),
        }
    }

    fn introduce_parameters(&mut self) {
        let entry = self.entry_location();
        // 0.31.13 追加 A: transition `self` (first parameter) is implicitly
        // consumed by the transition mechanism — skip Introduce action.
        let is_transition = self.signature.owner.0.starts_with("transition:");
        for (idx, parameter) in self.signature.parameters.iter().enumerate() {
            if is_transition && idx == 0 {
                continue;
            }
            if !self.is_linear(&parameter.ty) {
                continue;
            }
            let local = ResolvedLocalId(NodeId(format!("{}/local", parameter.id.0 .0)));
            let Some(declaration) = self.body.locals.get(&local) else {
                self.errors.push(Diagnostic::error(
                    format!(
                        "linear parameter '{}' has no ResolvedBody local",
                        parameter.name
                    ),
                    self.body.root.origin.user_span(),
                ));
                continue;
            };
            let place = self.place_from_local(&local);
            self.actions.push(CanonicalResourceAction {
                kind: CanonicalActionKind::Introduce,
                resource: self.resource_for_local(&local),
                source: Some(place.clone()),
                target: Some(place),
                loan: None,
                location: entry.clone(),
                span: declaration.origin.user_span(),
                origin: declaration.origin.clone(),
            });
        }
    }

    /// 0.36.45: then 块内首个线性消费的最内层节点——CFG 点序把内层表达式
    /// 点排在外层语句点之前，Introduce 键到语句节点会在首个消费之后应用
    /// （顺序翻转）；键到消费自身所在的点 + 动作秩排序（Introduce=3 <
    /// Move=5）保证先复位后消费。二元/累加形状从右侧下探（`n = n + f(x)`
    /// 的消费在右），构造/调用形状直接落在调用节点。
    fn linear_action_node(&self, expr: &ResolvedExpr) -> (NodeId, crate::core::Origin) {
        let mut current = expr;
        loop {
            match &current.kind {
                ResolvedExprKind::Call(call) => {
                    // 调用实参链下钻：线性消费发生在最内层实参（
                    // `println(sink(x))` 里 x 的 Move 键在 sink 调用节点，
                    // 不在 println 外层）——点序内层在前，Introduce 必须 ≤
                    // 首个消费点；Nominal 通常只走首实参（接收者/首个值），
                    // 首实参及其嵌套链已覆盖标准形状。
                    if let Some(first) = call.arguments.first().map(|a| &a.value) {
                        current = first;
                        continue;
                    }
                    return (current.node_id.clone(), current.origin.clone());
                }
                ResolvedExprKind::Load(_) => {
                    return (current.node_id.clone(), current.origin.clone());
                }
                ResolvedExprKind::Binary { right, .. } => current = right,
                ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                    // 臂体/块体常被 block 包裹：下钻首语句的表达式
                    //（点序内层在前，块节点点 = 块出口，落在其后会顺序翻转）。
                    let head: Option<&ResolvedExpr> = block
                        .statements
                        .first()
                        .and_then(|s| match &s.kind {
                            ResolvedStmtKind::Expr(e) => Some(e),
                            ResolvedStmtKind::Return { value: Some(e), .. } => Some(e),
                            ResolvedStmtKind::Bind {
                                initializer: Some(e),
                                ..
                            } => Some(e),
                            ResolvedStmtKind::Assign { value, .. } => Some(value),
                            _ => None,
                        })
                        .or_else(|| block.result.as_ref().map(|v| &**v));
                    match head {
                        Some(e) => current = e,
                        None => {
                            // 首语句是 Drop 等非表达式语句：其动作键在语句节点
                            // 本身——以语句节点为 Introduce 锚（同点 + 动作秩
                            // Introduce=3 < Drop=5 保证先复位后消费）。
                            if let Some(st) = block.statements.first() {
                                return (st.node_id.clone(), st.origin.clone());
                            }
                            return (current.node_id.clone(), current.origin.clone());
                        }
                    }
                }
                _ => return (current.node_id.clone(), current.origin.clone()),
            }
        }
    }

    fn visit_block(&mut self, block: &ResolvedBlock, return_result: bool) {
        // v0.34.8 (golden §6.2): reset the double-consume tracker at each
        // block boundary — a resource consumed in one branch is legitimately
        // consumed in a sibling branch. The assertion now catches only a
        // genuine second consumption within one straight-line block.
        self.consumed_resources.clear();
        for statement in &block.statements {
            self.visit_stmt(statement);
            // Audit 2026-08-05 (wave-2, H-6): an anonymous temporary borrow
            // created anywhere in this statement (`inc(&mut x)`, including
            // nested call arguments) ends at the statement boundary — mirror
            // of named-borrow NLL. The flush resolves against the statement's
            // terminating CFG point; statements without their own point keep
            // the loans pending until the next flush site.
            self.flush_pending_loans(&statement.node_id, &statement.origin);
        }
        if let Some(result) = &block.result {
            self.visit_expr(result, None);
            if return_result {
                // Audit 2026-08-05 (wave-2, C-2): a function whose RESULT is
                // a branch expression over distinct linear resources leaks
                // every arm not taken at runtime — reject like every other
                // consumption position.
                if let Some(distinct) = self.xor_branch_violation(result) {
                    self.push_xor_diagnostic(&distinct, &result.origin);
                } else {
                    self.emit_consumes(
                        CanonicalActionKind::Return,
                        self.capability_places(result),
                        &result.node_id,
                        &result.origin,
                    );
                }
            }
        }
    }

    fn visit_stmt(&mut self, statement: &ResolvedStmt) {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                if let Some(initializer) = initializer {
                    let reference = self.single_binding(pattern);
                    self.last_visit_rejected = false;
                    self.in_bind_initializer = true;
                    self.visit_expr(initializer, reference.as_ref());
                    self.in_bind_initializer = false;
                    // 0.36.46: 定向头提取 `let c = xs[0]`——c 认领一个元素义务
                    //（fresh Introduce），容器保留余部义务（既有 fact 不动；
                    // 后续 drop(xs) = 释放余部；不触 xs → 返回门禁 E0256）。
                    // 与普通配对不同：绝不把容器整体 Move 进 c。
                    if let Some(base) = self.directional_extraction_base.take() {
                        let mut bindings = Vec::new();
                        self.linear_bindings(pattern, &mut bindings);
                        if bindings.len() != 1 {
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0304,
                                    format!(
                                        "head extraction `{}[0]` binds {} linear name(s);                                          exactly one element is released",
                                        self.local_name(&base),
                                        bindings.len()
                                    ),
                                    statement.origin.user_span(),
                                )
                                .with_help(&format!(
                                    "bind the extracted element with a single name (`let c = {}[0]`)",
                                    self.local_name(&base)
                                )),
                            );
                            return;
                        }
                        let target = self.place_from_local(&bindings[0]);
                        self.push_action(
                            &statement.node_id,
                            &statement.origin,
                            ActionDraft {
                                kind: CanonicalActionKind::Introduce,
                                resource: self.resource_for_local(&bindings[0]),
                                source: Some(target.clone()),
                                target: Some(target),
                                loan: None,
                            },
                        );
                        return;
                    }
                    // 0.36.43: an E0304-rejected extraction (v[0] — index/slice/
                    // tuple projection of a non-droppable linear container) must
                    // not pair its place into the binding — the extracted value
                    // never moved, and fabricating the Move made x own the
                    // container's resource, so a later legitimate drop(v) hit
                    // the RESOURCE-LINEAR-001 double-drop signal. The E0304 is
                    // the whole story; skip the pairing entirely.
                    if self.last_visit_rejected {
                        // The binding was never established (the E0304 stands
                        // alone); a preceding materialization already attached
                        // the container's resource identity to the binding's
                        // local, so without this a later `drop(x)` would
                        // discharge the container's identity and the next
                        // `drop(v)` would trip RESOURCE-LINEAR-001. Clear the
                        // phantom ownership.
                        let mut bindings = Vec::new();
                        self.linear_bindings(pattern, &mut bindings);
                        for binding in bindings {
                            self.resources.remove(&binding);
                        }
                        return;
                    }
                    // Audit 2026-08-05 (wave-2, C-2/G-2): If/Match are XOR —
                    // exactly one arm's value flows at runtime. Consuming a
                    // branch expression that carries SEVERAL distinct linear
                    // resources (into a binding here, into a call/return/
                    // break below) can discharge at most one obligation; the
                    // rest leak on every path that does not take their arm.
                    // Fail-closed with E0840 at every consumption point so
                    // the rule is position-invariant. Design call: a single
                    // binding is one consumer; XOR of distinct resources
                    // needs one consumer per resource, which straight-line
                    // ownership facts cannot grant (the surviving resource
                    // differs per path). A branch duplicating ONE place
                    // (`if f { t } else { t }`) stays legal — one distinct
                    // resource, one obligation.
                    if let Some(distinct) = self.xor_branch_violation(initializer) {
                        self.push_xor_diagnostic(&distinct, &statement.origin);
                        return;
                    }
                    let sources = self.capability_places(initializer);
                    let pairs = self.expand_source_pairs(&sources);
                    let mut bindings = Vec::new();
                    self.linear_bindings(pattern, &mut bindings);
                    // Audit 2026-08-05 (wave-1 fix 1, extended wave-2 review
                    // §1.3): positional pairing is only sound when every
                    // linear source is matched by a linear binding. Wildcards
                    // and length mismatches mispair or strand sources:
                    // `let (_, y) = (a, b)` would Move a → y while untouched
                    // b stays Available → `drop(b); drop(y)` accepted
                    // (verified use-after-move + silent leak of a).
                    // Fail-closed: reject every mismatch EXCEPT:
                    // * one binding receiving several resources — a legal
                    //   aggregate merge (`let x = (a, b)`); the binding
                    //   inherits every resource and a later drop expands to
                    //   all of them;
                    // * the sanctioned split() lowering — a single-element
                    //   Tuple([receiver]) with one source and >=2 bindings —
                    //   but ONLY when the pattern binds every position: a
                    //   wildcard in a split pattern silently discards a
                    //   capability atom (review §1.3 fifth hole:
                    //   `let (_, w) = c.split()` checked green while the
                    //   read atom leaked), so any split shape with a wildcard
                    //   is rejected;
                    // * sources.len() == 0 — the initializer contributes no
                    //   linear place (e.g. a call result), so every binding
                    //   is a fresh introduction and nothing can be mispaired.
                    // RED LINE (wave1-review §1.3, verified PoC): the split
                    // shape is identified by the pattern's SLOT count, not
                    // the surviving linear-binding count. Counting bindings
                    // lets `_` eat one slot: `(_, w)` compresses to one
                    // binding, balances the single source 1==1 in the generic
                    // pairing arm below, and the discarded read atom escapes
                    // with zero obligation (`mimi check` returned Ok). The
                    // slot count stays 2, so the wildcard rejection below
                    // cannot be vacated by binding compression.
                    let split_shape = sources.len() == 1
                        && matches!(&initializer.kind, ResolvedExprKind::Tuple(values) if values.len() == 1)
                        && self.pattern_slot_count(pattern) >= 2;
                    if split_shape && self.pattern_has_wildcard(pattern) {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0304,
                                "split() capability atoms cannot be discarded with `_`: every atom \
                                     of a split must be bound and consumed explicitly"
                                    .to_string(),
                                statement.origin.user_span(),
                            )
                            .with_help(
                                "bind every split atom (e.g. `let (r, w) = c.split()`) and \
                                     consume each one; `_` silently leaks the unbound atom",
                            ),
                        );
                        return;
                    }
                    if !split_shape
                        && !pairs.is_empty()
                        && pairs.len() != bindings.len()
                        && self.single_binding(pattern).is_none()
                    {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0304,
                                format!(
                                    "destructuring consumes linear values ambiguously: \
                                     {} linear source(s) cannot be paired positionally with {} linear binding(s)",
                                    pairs.len(),
                                    bindings.len()
                                ),
                                statement.origin.user_span(),
                            )
                            .with_help(
                                "bind every linear element explicitly; wildcard `_` positions and \
                                 shortened patterns strand or mispair linear sources \
                                 (only `split()` tuples and single-binding aggregates are exempt)",
                            ),
                        );
                        return;
                    }
                    if split_shape {
                        // Sanctioned split() lowering: binding₀ MOVES the
                        // receiver (consuming the receiver's fact); the rest
                        // are fresh atom introductions.
                        let mut bindings = bindings.into_iter();
                        if let (Some(first), Some(source)) = (bindings.next(), sources.first()) {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource: self.resource_for_place(source),
                                    source: Some(source.clone()),
                                    target: Some(self.place_from_local(&first)),
                                    loan: None,
                                },
                            );
                        }
                        for local in bindings {
                            let target = self.place_from_local(&local);
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Introduce,
                                    resource: self.resource_for_local(&local),
                                    source: Some(target.clone()),
                                    target: Some(target),
                                    loan: None,
                                },
                            );
                        }
                    } else if !pairs.is_empty() && pairs.len() == bindings.len() {
                        for (binding, (resource, source)) in
                            bindings.into_iter().zip(pairs.into_iter())
                        {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource,
                                    source: Some(source),
                                    target: Some(self.place_from_local(&binding)),
                                    loan: None,
                                },
                            );
                        }
                    } else if bindings.len() == 1 && !pairs.is_empty() {
                        // Aggregate merge into one binding (`let x = (a,b)`):
                        // every source resource moves into the binding, which
                        // becomes the owner of all of them.
                        let binding = bindings.into_iter().next().expect("bindings.len() == 1");
                        let target = self.place_from_local(&binding);
                        for (resource, source) in pairs {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource,
                                    source: Some(source),
                                    target: Some(target.clone()),
                                    loan: None,
                                },
                            );
                        }
                    } else {
                        for binding in bindings {
                            let target = self.place_from_local(&binding);
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Introduce,
                                    resource: self.resource_for_local(&binding),
                                    source: Some(target.clone()),
                                    target: Some(target),
                                    loan: None,
                                },
                            );
                        }
                    }
                }
            }
            ResolvedStmtKind::Assign { target, value, .. } => {
                self.visit_expr(value, None);
                if self.place_is_linear(target) {
                    // Audit 2026-08-05 (wave-2, C-2): assigning a branch
                    // expression with distinct linear resources into one
                    // place is the same XOR violation as consuming it.
                    if let Some(distinct) = self.xor_branch_violation(value) {
                        self.push_xor_diagnostic(&distinct, &statement.origin);
                        return;
                    }
                    let pairs = self.expand_source_pairs(&self.capability_places(value));
                    let target_place = self.canonical_place(target);
                    if pairs.is_empty() {
                        // Audit 2026-08-05 (wave-2, review §5.5): a linear
                        // RESULT (typically a call) assigned into a linear
                        // place establishes a fresh obligation owned by the
                        // target — previously no action was emitted at all,
                        // so `x = make_token()` created no fact and the
                        // obligation vanished.
                        if self.is_linear(&value.ty) && !self.is_droppable_type(&value.ty) {
                            let resource =
                                ResourceId(NodeId(format!("{}/assigned", statement.node_id.0)));
                            if target.projections.is_empty() {
                                self.resources
                                    .insert(target.base.clone(), vec![resource.clone()]);
                            }
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Introduce,
                                    resource,
                                    source: Some(target_place.clone()),
                                    target: Some(target_place),
                                    loan: None,
                                },
                            );
                        }
                    } else {
                        // Audit 2026-08-05 (wave-2, G-1): move EVERY linear
                        // source into the target (the old `.first()` stranded
                        // every element past the first: `x = (c, d)` parked
                        // d → false E0256), then re-establish the target's
                        // resource identity so a later `drop(x)` resolves the
                        // facts the assignment just retargeted. Without the
                        // re-key, `drop(x); x = b; drop(x)` reported the
                        // second drop as a double-consume of x's ORIGINAL
                        // (already consumed) identity — a false E0304.
                        // Re-keying only happens for root targets; projected
                        // targets (`x.field = a`) keep the base's catalog.
                        for (resource, source) in &pairs {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource: resource.clone(),
                                    source: Some(source.clone()),
                                    target: Some(target_place.clone()),
                                    loan: None,
                                },
                            );
                        }
                        if target.projections.is_empty() {
                            self.resources.insert(
                                target.base.clone(),
                                pairs.into_iter().map(|(resource, _)| resource).collect(),
                            );
                        }
                    }
                }
            }
            ResolvedStmtKind::Return { value, .. } => {
                if let Some(value) = value {
                    self.visit_expr(value, None);
                    // Audit 2026-08-05 (wave-2, C-2): `return if/match` with
                    // distinct linear arms leaks every arm that is not taken
                    // at runtime while the analysis consumed all of them.
                    if let Some(distinct) = self.xor_branch_violation(value) {
                        self.push_xor_diagnostic(&distinct, &statement.origin);
                        return;
                    }
                    self.emit_consumes(
                        CanonicalActionKind::Return,
                        self.capability_places(value),
                        &statement.node_id,
                        &statement.origin,
                    );
                }
            }
            ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    self.visit_expr(value, None);
                    // P1-4: break value must emit consume actions, symmetric
                    // with Return. `loop { break token }` must track token
                    // as consumed. Audit 2026-08-05 (wave-2, C-2): with the
                    // same XOR guard — `break if f { a } else { b }` leaks
                    // the arm not taken.
                    if let Some(distinct) = self.xor_branch_violation(value) {
                        self.push_xor_diagnostic(&distinct, &statement.origin);
                        return;
                    }
                    self.emit_consumes(
                        CanonicalActionKind::Move,
                        self.capability_places(value),
                        &statement.node_id,
                        &statement.origin,
                    );
                }
            }
            ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {}
            ResolvedStmtKind::Expr(expression) => {
                self.visit_expr(expression, None);
                // Audit 2026-08-05 (wave-2, review §5.5): statement-style
                // discard of a call returning a linear value used to
                // establish NO obligation — the result was never introduced,
                // so `make_token();` leaked silently (no fact, no E0256).
                // Introduce the discarded result as a fresh obligation at the
                // call site; the return gate then reports it like any other
                // unconsumed resource. Droppable results (flow states) are
                // exempt, mirroring the return gate.
                if let Some(call_expr) = self.discarded_linear_call(expression) {
                    let resource = ResourceId(NodeId(format!("{}/discarded", call_expr.node_id.0)));
                    let place =
                        Place::root(LocalId(resource.0.clone()), "<discarded linear result>");
                    self.push_action(
                        &statement.node_id,
                        &statement.origin,
                        ActionDraft {
                            kind: CanonicalActionKind::Introduce,
                            resource,
                            source: Some(place.clone()),
                            target: Some(place),
                            loan: None,
                        },
                    );
                }
            }
            ResolvedStmtKind::While { condition, body } => {
                self.visit_expr(condition, None);
                self.visit_block(body, false);
            }
            ResolvedStmtKind::WhileLet {
                pattern,
                initializer,
                body,
                ..
            }
            | ResolvedStmtKind::For {
                pattern,
                iterable: initializer,
                body,
                ..
            } => {
                self.visit_expr(initializer, None);
                // 0.36.37: a for/while-let loop over a linear container is an
                // exhaustive, element-wise deconstruction — candidate (1)
                // extended from match/if-let (0.36.36). Per-iteration element
                // Introductions at the pattern Binding point (loop-carried, so
                // the backedge fixed-point pass sees the element FRESH — the
                // body's consumption never trips the E0304 "moved after
                // consumed" artifact), and the container obligation dissolves
                // at the loop statement. Fail-closed guards mirror
                // match/if-let: a stranding wildcard (`for _ in v` over a
                // linear element) and any early exit in the body
                // (`break`/`return` — at runtime the not-yet-iterated
                // elements are abandoned) keep the container obligation
                // unsolved → E0256.
                if let Some(container_place) = self.linear_loop_container(initializer, pattern) {
                    // Container dissolve: ONLY builtin sequence containers
                    // (List/Map/Set) in for-loops are exhaustive element-wise
                    // deconstructions — the VM iterates the container and
                    // every element is visited exactly once (when the body
                    // consumes it; the per-iteration obligation enforces
                    // that). A while-let over Option/Result re-evaluates its
                    // initializer every round and NEVER consumes the
                    // container binding (runtime semantics — `while let
                    // Some(x) = o` re-reads o), so dissolving the container
                    // there would falsely accept a runtime-infinite loop:
                    // the container obligation stays unsolved (E0256).
                    // Early exits (`break`/`return`) also block the dissolve:
                    // at runtime the not-yet-iterated elements are abandoned.
                    if self.linear_sequence_container(&initializer.ty)
                        && !self.block_has_early_exit(body, true)
                    {
                        self.emit_consumes(
                            CanonicalActionKind::Drop,
                            vec![container_place],
                            &statement.node_id,
                            &statement.origin,
                        );
                    }
                    // 0.36.37: per-iteration element obligation — the loop
                    // variable binds a FRESH element every iteration, so the
                    // body must consume each one. Keyed on the pattern
                    // Binding point inside the loop body — loop-carried, so
                    // the Introduce resets the fact before the body runs on
                    // every fixed-point pass and a body consumption
                    // (`sink(x)`) never trips the E0304 backedge
                    // double-consume artifact. A body that skips the
                    // consumption (continue/conditionals/early break) leaves
                    // the element Available at the loop-carried diverging
                    // sink or the return path → E0256. Emitted even when the
                    // container dissolve is blocked (early exits): the
                    // container obligation then stays unsolved (E0256 on the
                    // container — the not-yet-iterated elements leak at
                    // runtime), and the element obligation still reports the
                    // current element independently.
                    let mut bindings = Vec::new();
                    self.linear_bindings(pattern, &mut bindings);
                    for local in bindings {
                        let target = self.place_from_local(&local);
                        self.push_action(
                            &pattern.node_id,
                            &pattern.origin,
                            ActionDraft {
                                kind: CanonicalActionKind::Introduce,
                                resource: self.resource_for_local(&local),
                                source: Some(target.clone()),
                                target: Some(target),
                                loan: None,
                            },
                        );
                    }
                }
                // Audit 2026-08-05 (wave-2, G-4): the for/while-let iterable
                // is evaluated EXACTLY ONCE before the loop (hoisted into the
                // CFG pre-header), so its anonymous temporary borrows end at
                // the loop statement's terminating point — BEFORE the body
                // runs. Without this explicit flush, the body's FIRST
                // statement boundary grabs the iterable's pending loan and
                // ends it at ITS point inside the body, co-located with the
                // first write (which ranks before the BorrowEnd) — a false
                // E0415. Keying on the loop statement's node resolves the
                // pre-header location added by resolved_lower's hoist.
                self.flush_pending_loans(&statement.node_id, &statement.origin);
                self.visit_block(body, false);
            }
            ResolvedStmtKind::IfLet {
                pattern,
                initializer,
                then_block,
                else_block,
                ..
            } => {
                self.visit_expr(initializer, None);
                // 0.36.36: same container-obligation dissolve as the Match arm
                // — `if let Some(x) = o` exhaustively handles o (bind or no
                // payload); a stranding wildcard keeps fail-closed behavior.
                if let Some(container_place) = self.linear_match_container(initializer, &[pattern])
                {
                    // resolved_lower hoists the initializer into the CFG
                    // pre-header, so the STATEMENT node has no CFG point —
                    // key the dissolve on the initializer expression itself.
                    self.emit_consumes(
                        CanonicalActionKind::Drop,
                        vec![container_place],
                        &initializer.node_id,
                        &initializer.origin,
                    );
                }
                // 0.36.45: then 块绑定名 Introduce（for 体内 if-let 的循环
                // 载入修正）。绑定是逐迭代临时资源——每轮循环重新 Introduce
                // 复位 availability；否则上一迭代的 Consumed 事实被背边携带，
                // 下一迭代的 Move 撞 "moved after it was consumed"
                // （基线非循环单遍不受影响）。
                let mut bindings = Vec::new();
                self.linear_bindings(pattern, &mut bindings);
                // 0.36.45: 绑定 Introduce 必须落在 then 块入口（仅 then 路径
                // 执行 + 每迭代复位）——若落在头部分支前，fall-through 也
                // 会看到 Available 的 y → 汇合撞 "consumed on only some
                // paths"；若键到语句节点，CFG 点序把内层表达式点排在前面
                //（stmt 点 = 语句出口），Introduce 会应用在首个消费之后。
                // 键到首语句的表达式/初始化器节点 = 块的入口点——与消费
                // 同点，dataflow 按动作秩排序（Introduce=3 < Move=5）保证
                // 先复位后消费。空 then 块（弃置 y）退化为头部键：两路径
                // 均匀看到 Available → 返回门禁 E0256（弃置即拒绝）。
                let (then_head_node, then_head_origin) = match then_block.statements.first() {
                    Some(stmt) => match stmt.kind {
                        ResolvedStmtKind::Expr(ref e)
                        | ResolvedStmtKind::Return {
                            value: Some(ref e), ..
                        } => self.linear_action_node(e),
                        ResolvedStmtKind::Bind {
                            initializer: Some(ref init),
                            ..
                        } => self.linear_action_node(init),
                        ResolvedStmtKind::Assign { ref value, .. } => {
                            self.linear_action_node(value)
                        }
                        _ => (stmt.node_id.clone(), stmt.origin.clone()),
                    },
                    None => (initializer.node_id.clone(), initializer.origin.clone()),
                };
                for binding in bindings {
                    let target = self.place_from_local(&binding);
                    self.push_action(
                        &then_head_node,
                        &then_head_origin,
                        ActionDraft {
                            kind: CanonicalActionKind::Introduce,
                            resource: self.resource_for_local(&binding),
                            source: Some(target.clone()),
                            target: Some(target),
                            loan: None,
                        },
                    );
                }
                self.visit_block(then_block, false);
                if let Some(else_block) = else_block {
                    self.visit_block(else_block, false);
                }
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                self.visit_block(body, false);
            }
            ResolvedStmtKind::Drop(places) => {
                // 0.36.43: `drop(v[0])` — the Drop arm carries resolved PLACES
                // (no expression visit), so the M9 reject never ran for it.
                // Dropping one element releases it and leaks every unextracted
                // sibling — the same element-extraction hole, rejected
                // identically (and neutered, so a later `drop(v)` cannot
                // double-consume the container's identity).
                for place in places.iter() {
                    let has_element_projection = place.projections.iter().any(|projection| {
                        matches!(
                            projection,
                            ResolvedProjection::Index { .. } | ResolvedProjection::Tuple { .. }
                        )
                    });
                    if !has_element_projection {
                        continue;
                    }
                    let Some(local) = self.body.locals.get(&place.base) else {
                        continue;
                    };
                    if self.is_linear(&local.ty) && !self.is_droppable_type(&local.ty) {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0304,
                                format!(
                                    "'{}' cannot be dropped by index or slice: element-level                                      extraction from a linear container is not tracked and                                      leaks every unextracted element",
                                    self.local_name(&place.base)
                                ),
                                statement.origin.user_span(),
                            )
                            .with_help(
                                "move or drop the whole container (e.g. drop(v)) instead of                                  indexing or slicing into it",
                            ),
                        );
                        self.rejected_extraction_places
                            .insert(self.canonical_place(place));
                    }
                }
                let places = places
                    .iter()
                    .filter(|place| self.place_is_linear(place))
                    .map(|place| self.canonical_place(place))
                    .filter(|place| !self.rejected_extraction_places.contains(place))
                    .collect::<Vec<_>>();
                for place in places {
                    // Audit 2026-08-05 (wave-2, G-1): dropping an aggregate
                    // owner (`let x = (a, b); drop(x)`) consumes EVERY
                    // resource the place currently owns — one Drop per
                    // identity. Dropping a single-identity place is unchanged.
                    let mut seen = BTreeSet::new();
                    for resource in self.resources_for_place(&place) {
                        if !seen.insert(resource.clone()) {
                            continue;
                        }
                        self.push_action(
                            &statement.node_id,
                            &statement.origin,
                            ActionDraft {
                                kind: CanonicalActionKind::Drop,
                                resource,
                                source: Some(place.clone()),
                                target: None,
                                loan: None,
                            },
                        );
                    }
                }
            }
            ResolvedStmtKind::Contract { condition, .. } => self.visit_expr(condition, None),
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    self.visit_expr(expression, None);
                }
            }
            ResolvedStmtKind::Pinned {
                value,
                binding,
                body,
            } => {
                self.visit_expr(value, None);
                // Audit 2026-08-05 (wave-1 fix 6): a pinned binding of linear
                // type used to get a catalog entry but NO action — the pinned
                // resource was invisible to dataflow and could escape without
                // consumption. Mirror the Bind arm: a single linear source
                // MOVES into the binding (consuming the source's fact), a
                // source-free value INTRODUCES the binding as a fresh
                // obligation, and multiple sources are ambiguous → fail-closed.
                // Not Introduce-only: introducing without consuming the source
                // would double-count (`pinned(cap_val) |p| drop(p)` leaking
                // cap_val) — the option that cannot regress is Move semantics.
                if let Some(binding) = binding {
                    if self
                        .body
                        .locals
                        .get(binding)
                        .is_some_and(|local| self.is_linear(&local.ty))
                    {
                        let target = self.place_from_local(binding);
                        let pairs = self.expand_source_pairs(&self.capability_places(value));
                        if pairs.len() == 1 {
                            let (resource, source) = pairs.into_iter().next().expect("len == 1");
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource,
                                    source: Some(source),
                                    target: Some(target),
                                    loan: None,
                                },
                            );
                        } else if pairs.is_empty() {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Introduce,
                                    resource: self.resource_for_local(binding),
                                    source: Some(target.clone()),
                                    target: Some(target),
                                    loan: None,
                                },
                            );
                        } else {
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0304,
                                    format!(
                                        "pinned binding consumes linear values ambiguously: \
                                         {} linear sources cannot be paired with one binding",
                                        pairs.len()
                                    ),
                                    statement.origin.user_span(),
                                )
                                .with_help(
                                    "pin a single linear place; multiple linear sources cannot \
                                     be positionally assigned to one pinned binding",
                                ),
                            );
                        }
                    }
                }
                self.visit_block(body, false);
            }
        }
    }

    fn visit_expr(
        &mut self,
        expression: &ResolvedExpr,
        borrow_reference: Option<&ResolvedLocalId>,
    ) {
        self.reject_index_read_extraction(expression);
        match &expression.kind {
            ResolvedExprKind::Unary {
                op: ResolvedUnaryOp::BorrowShared | ResolvedUnaryOp::BorrowMutable,
                operand,
            } => {
                self.visit_expr(operand, None);
                let mut source = match &operand.kind {
                    ResolvedExprKind::Load(source) => self.canonical_place(source),
                    _ => Place::root(
                        LocalId(NodeId(format!("{}/temporary", expression.node_id.0))),
                        "<temporary>",
                    ),
                };
                let kind = match &expression.kind {
                    ResolvedExprKind::Unary {
                        op: ResolvedUnaryOp::BorrowMutable,
                        ..
                    } => LoanKind::Mutable,
                    _ => LoanKind::Shared,
                };
                let loan_id = LoanId(NodeId(format!("{}/loan", expression.node_id.0)));
                let parent = source
                    .projections
                    .first()
                    .filter(|projection| **projection == PlaceProjection::Deref)
                    .and_then(|_| {
                        self.loans
                            .iter()
                            .rev()
                            .find(|loan| loan.reference.as_ref() == Some(&source.base))
                            .map(|loan| (loan.id.clone(), loan.place.clone()))
                    });
                let parent_id = parent.as_ref().map(|(id, _)| id.clone());
                if let Some((_, parent_place)) = parent {
                    source = parent_place;
                }
                let reference = borrow_reference.map(|local| LocalId(local.0.clone()));
                let reference_name = borrow_reference.map(|local| self.local_name(local));
                let location = self.location(&expression.node_id, &expression.origin);
                let resource = self.resource_for_place(&source);
                self.loans.push(Loan {
                    id: loan_id.clone(),
                    parent: parent_id,
                    kind,
                    place: source.clone(),
                    reference,
                    reference_name: reference_name.clone(),
                    start: location.clone(),
                    end_edges: Vec::new(),
                    span: expression.origin.user_span(),
                });
                self.actions.push(CanonicalResourceAction {
                    kind: match kind {
                        LoanKind::Shared => CanonicalActionKind::BorrowShared,
                        LoanKind::Mutable => CanonicalActionKind::BorrowMut,
                    },
                    resource: resource.clone(),
                    source: Some(source),
                    target: None,
                    loan: Some(loan_id.clone()),
                    location,
                    span: expression.origin.user_span(),
                    origin: expression.origin.clone(),
                });
                // Audit 2026-08-05 (wave-2, H-6): anonymous borrows have no
                // reference binding whose liveness could end the loan — park
                // them for statement-boundary termination in visit_block.
                if reference_name.is_none() {
                    self.pending_anonymous_loans.push((loan_id, resource));
                }
            }
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    self.visit_expr(&argument.value, None);
                }
                // 0.36.47 容器方法变换面（Phase C"容器方法余面"）：线性接收者的
                // 变换方法（Mutate 借用标记；结果 = List/Map/Set/Tuple 且携带
                // 线性义务）降为消费语义——接收者容器整体转出（Move），义务
                // 转移到结果（`let ys = xs.reverse()`：xs 移入 reverse，ys 携带
                // 元素义务、drop(ys) 结算——与 for 迭代同构；此前 Mutate 借用
                // 不解体容器 → 用户被迫额外 drop(xs) = 不可达语义）。
                // 读取/提取面（len/is_empty/count/find/first/last/find_map——
                // 结果标量/裸元素/Option）保持借用：接收者仍需整体 drop。
                if matches!(call.permission, Some(Permission::Mutate))
                    && self.method_transform_result(call)
                {
                    // 0.36.48：变换面 ALL 线性参数整体转出（不只 receiver）。
                    // 义务守恒：结果容器的义务 = 每个线性实参的义务并集。
                    //   - receiver（xs.reverse()）：xs 移入方法（4u 已开）；
                    //   - 容器参数（xs.concat(ys)：ys 元素义务并入结果 zs，
                    //     用户只 drop(zs)——此前 ys 义务原处 → E0256 死锁）；
                    //   - 线性值参数（xs.remove(v)/xs.intersperse(sep)：元素
                    //     义务进方法，由方法体结算/并入结果恰一次）。
                    // 读/提取面（len/first/find_map——结果标量/裸元素/Option）
                    // 不在此列：参数保持借用（容器义务原处）。
                    for argument in &call.arguments {
                        if let ResolvedExprKind::Load(place) = &argument.value.kind {
                            if self.place_is_linear(place) {
                                let canonical = self.canonical_place(place);
                                self.push_action(
                                    &expression.node_id,
                                    &expression.origin,
                                    ActionDraft {
                                        kind: CanonicalActionKind::Move,
                                        resource: self.resource_for_place(&canonical),
                                        source: Some(canonical.clone()),
                                        target: None,
                                        loan: None,
                                    },
                                );
                            }
                        }
                    }
                }
                if !matches!(call.permission, Some(Permission::View | Permission::Mutate)) {
                    for argument in &call.arguments {
                        let transferred_endpoint = match &argument.value.kind {
                            ResolvedExprKind::Load(place) if place.projections.is_empty() => call
                                .session
                                .iter()
                                .any(|transition| transition.endpoint == place.base),
                            _ => false,
                        };
                        if transferred_endpoint {
                            continue;
                        }
                        // Audit 2026-08-05 (wave-2, C-2):
                        // `sink(if flag { a } else { b })` consumed BOTH arms
                        // under AND semantics while XOR runtime moves exactly
                        // one — the other arm's resource leaks on every run.
                        // Reject branch arguments carrying distinct resources.
                        if let Some(distinct) = self.xor_branch_violation(&argument.value) {
                            self.push_xor_diagnostic(&distinct, &expression.origin);
                            continue;
                        }
                        self.emit_consumes(
                            CanonicalActionKind::Move,
                            self.capability_places(&argument.value),
                            &expression.node_id,
                            &expression.origin,
                        );
                    }
                }
                for transition in &call.session {
                    let place = self.place_from_local(&transition.endpoint);
                    self.push_action(
                        &expression.node_id,
                        &expression.origin,
                        ActionDraft {
                            kind: if transition.terminal {
                                CanonicalActionKind::Drop
                            } else {
                                CanonicalActionKind::TransferSession
                            },
                            resource: self.resource_for_local(&transition.endpoint),
                            source: Some(place.clone()),
                            target: (!transition.terminal).then_some(place),
                            loan: None,
                        },
                    );
                }
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.visit_expr(condition, None);
                self.visit_block(then_block, false);
                self.visit_block(else_block, false);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee, None);
                // 0.36.36 candidate (1): an EXHAUSTIVE match over a linear
                // aggregate container (Option/Result) dissolves the CONTAINER
                // obligation — every arm either binds the payload (its own
                // resource chain continues) or has none. Fail-closed guards:
                // single linear source, aggregate container type, and no
                // wildcard anywhere in any arm pattern (a wildcard position
                // could strand linear payload atoms).
                let arm_patterns: Vec<&ResolvedPattern> =
                    arms.iter().map(|arm| &arm.pattern).collect();
                if let Some(container_place) = self.linear_match_container(scrutinee, &arm_patterns)
                {
                    self.emit_consumes(
                        CanonicalActionKind::Drop,
                        vec![container_place],
                        &expression.node_id,
                        &expression.origin,
                    );
                }
                for arm in arms {
                    self.visit_arm(arm);
                }
            }
            ResolvedExprKind::Lambda(lambda) => {
                let captures = lambda
                    .captures
                    .iter()
                    .filter(|capture| {
                        self.body
                            .locals
                            .get(capture)
                            .is_some_and(|local| self.is_linear(&local.ty))
                    })
                    .map(|capture| self.place_from_local(capture))
                    .collect();
                self.emit_consumes(
                    CanonicalActionKind::TransferChild,
                    captures,
                    &expression.node_id,
                    &expression.origin,
                );
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.visit_block(block, false),
            _ => self.for_each_expr_child(expression, |this, child| this.visit_expr(child, None)),
        }
    }

    /// 0.36.36 candidate (1): is this match professor an exhaustive
    /// destructure of ONE linear aggregate container (Option/Result) with no
    /// wildcard patterns? When yes, the container's obligation is discharged
    /// at the match (payload bindings keep their own chains).
    fn linear_match_container(
        &self,
        scrutinee: &ResolvedExpr,
        patterns: &[&ResolvedPattern],
    ) -> Option<Place> {
        let ty = &scrutinee.ty;
        if !self.is_linear(ty) || self.is_droppable_type(ty) {
            return None;
        }
        let is_aggregate = matches!(
            self.types.get(ty),
            Some(ResolvedType::Option(_)) | Some(ResolvedType::Result { .. })
        );
        if !is_aggregate {
            return None;
        }
        if patterns.is_empty() || patterns.iter().any(|p| self.pattern_strands_linear(p)) {
            return None;
        }
        let places = self.capability_places(scrutinee);
        if places.len() != 1 {
            return None;
        }
        if self.resources_for_place(&places[0]).len() != 1 {
            return None;
        }
        Some(places[0].clone())
    }

    /// 0.36.36: a wildcard position strands a LINEAR atom when the covered
    /// field/pattern slot is linear (Some(_) over Option<cap>); wildcards
    /// over non-linear slots (Err(_) over a string payload) are harmless.
    fn pattern_strands_linear(&self, pattern: &ResolvedPattern) -> bool {
        match &pattern.kind {
            ResolvedPatternKind::Wildcard => self.is_linear(&pattern.ty),
            ResolvedPatternKind::Constructor { fields, .. } => fields
                .iter()
                .any(|(_, pattern)| self.pattern_strands_linear(pattern)),
            ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => patterns
                .iter()
                .any(|pattern| self.pattern_strands_linear(pattern)),
            ResolvedPatternKind::Slice { prefix, rest } => {
                prefix
                    .iter()
                    .any(|pattern| self.pattern_strands_linear(pattern))
                    || rest
                        .as_deref()
                        .is_some_and(|pattern| self.pattern_strands_linear(pattern))
            }
            ResolvedPatternKind::Binding { .. } | ResolvedPatternKind::Literal(_) => false,
        }
    }

    /// 0.36.37: a for/while-let loop over a linear container is an
    /// exhaustive, element-wise deconstruction — the container obligation can
    /// dissolve at the loop statement (candidate (1) extended from
    /// match/if-let). The guard mirrors `linear_match_container` and adds the
    /// builtin container nominals (List/Map/Set): single linear source,
    /// single owned identity, no linear-stranding wildcard in the pattern.
    fn linear_loop_container(
        &self,
        iterable: &ResolvedExpr,
        pattern: &ResolvedPattern,
    ) -> Option<Place> {
        let ty = &iterable.ty;
        if !self.is_linear(ty) || self.is_droppable_type(ty) {
            return None;
        }
        let is_aggregate = match self.types.get(ty) {
            Some(ResolvedType::Option(_)) | Some(ResolvedType::Result { .. }) => true,
            // builtin container nominals are linear exactly when an argument
            // is (H2 recursion) — kept here for shape affinity with the
            // 0.36.36 match guard.
            Some(ResolvedType::Nominal {
                item, arguments, ..
            }) if matches!(
                item.as_str(),
                "builtin:type:List" | "builtin:type:Map" | "builtin:type:Set"
            ) && arguments.iter().any(|argument| self.is_linear(argument)) =>
            {
                true
            }
            _ => false,
        };
        if !is_aggregate {
            return None;
        }
        if self.pattern_strands_linear(pattern) {
            return None;
        }
        let places = self.capability_places(iterable);
        if places.len() != 1 {
            return None;
        }
        if self.resources_for_place(&places[0]).len() != 1 {
            return None;
        }
        Some(places[0].clone())
    }

    /// 0.36.37: builtin sequence container nominals (List/Map/Set) with linear
    /// arguments — the only shapes whose for-loops exhaustively deconstruct
    /// the container at runtime (VM iterates the container once, taking each
    /// element). Option/Result while-let re-evaluates its initializer without
    /// consuming the binding, so it is NOT dissolvable.
    fn linear_sequence_container(&self, ty: &ResolvedTypeId) -> bool {
        matches!(
            self.types.get(ty),
            Some(ResolvedType::Nominal { item, arguments, .. })
                if matches!(
                    item.as_str(),
                    "builtin:type:List" | "builtin:type:Map" | "builtin:type:Set"
                ) && arguments.iter().any(|argument| self.is_linear(argument))
        )
    }

    /// 0.36.37: does the loop body contain an early exit that would abandon
    /// not-yet-iterated elements? `break` exits the loop mid-iteration (the
    /// remaining elements are never consumed at runtime); `return` exits the
    /// function. `continue` is safe — the loop still visits every element and
    /// a skipped element consumption is caught by the per-iteration element
    /// obligation (E0256). A `break` inside a NESTED loop targets the nested
    /// loop only (`count_breaks = false` there), but a `return` escapes
    /// everything.
    fn block_has_early_exit(&mut self, block: &ResolvedBlock, count_breaks: bool) -> bool {
        block
            .statements
            .iter()
            .any(|statement| self.stmt_has_early_exit(statement, count_breaks))
    }

    fn stmt_has_early_exit(&mut self, statement: &ResolvedStmt, count_breaks: bool) -> bool {
        match &statement.kind {
            ResolvedStmtKind::Break(_) => count_breaks,
            ResolvedStmtKind::Return { .. } => true,
            ResolvedStmtKind::Continue => false,
            // Nested loops own their breaks; only a return inside their
            // bodies escapes THIS loop. Their condition/iterable expressions
            // still evaluate in this loop's context — an expression-embedded
            // break there targets this loop.
            ResolvedStmtKind::While {
                condition, body, ..
            } => {
                self.expr_has_early_exit(condition, count_breaks)
                    || self.block_has_early_exit(body, false)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                self.expr_has_early_exit(initializer, count_breaks)
                    || self.block_has_early_exit(body, false)
            }
            ResolvedStmtKind::Loop(body) => self.block_has_early_exit(body, false),
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                self.expr_has_early_exit(initializer, count_breaks)
                    || self.block_has_early_exit(then_block, count_breaks)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| self.block_has_early_exit(block, count_breaks))
            }
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|value| self.expr_has_early_exit(value, count_breaks)),
            ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => self.expr_has_early_exit(value, count_breaks),
            ResolvedStmtKind::Assign { value, .. } => self.expr_has_early_exit(value, count_breaks),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                self.expr_has_early_exit(value, count_breaks)
                    || self.block_has_early_exit(body, count_breaks)
            }
            ResolvedStmtKind::Scope { body, .. } => self.block_has_early_exit(body, count_breaks),
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Math(_)
            | ResolvedStmtKind::NestedCallable(_) => false,
        }
    }

    fn expr_has_early_exit(&mut self, expression: &ResolvedExpr, count_breaks: bool) -> bool {
        match &expression.kind {
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expr_has_early_exit(condition, count_breaks)
                    || self.block_has_early_exit(then_block, count_breaks)
                    || self.block_has_early_exit(else_block, count_breaks)
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.expr_has_early_exit(scrutinee, count_breaks)
                    || arms.iter().any(|arm| {
                        arm.guard
                            .as_ref()
                            .is_some_and(|guard| self.expr_has_early_exit(guard, count_breaks))
                            || self.expr_has_early_exit(&arm.body, count_breaks)
                    })
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.block_has_early_exit(block, count_breaks),
            ResolvedExprKind::Comprehension {
                iterable,
                guard,
                value,
                ..
            } => {
                self.expr_has_early_exit(iterable, count_breaks)
                    || guard
                        .as_ref()
                        .is_some_and(|guard| self.expr_has_early_exit(guard, count_breaks))
                    || self.expr_has_early_exit(value, count_breaks)
            }
            _ => {
                let mut found = false;
                self.for_each_expr_child(expression, |this, child| {
                    if this.expr_has_early_exit(child, count_breaks) {
                        found = true;
                    }
                });
                found
            }
        }
    }

    fn visit_arm(&mut self, arm: &MatchArm) {
        if let Some(guard) = &arm.guard {
            self.visit_expr(guard, None);
        }
        // 0.36.45: 臂模式绑定 Introduce——与 IfLet 臂同款循环载入修正：臂绑定
        // 是逐迭代临时资源（非循环单遍 or_insert 即够）；循环内需每迭代复位，
        // 否则上一迭代的 Consumed 事实被背边携带 → "moved after consumed"。
        // 键在臂体首动作内层节点（0.36.45 then 头键同理由：点序内层在前，
        // Introduce 秩 3 < Move 秩 5 保证先复位后消费）。
        let mut bindings = Vec::new();
        self.linear_bindings(&arm.pattern, &mut bindings);
        if !bindings.is_empty() {
            let (node, origin) = self.linear_action_node(&arm.body);
            for binding in bindings {
                let target = self.place_from_local(&binding);
                self.push_action(
                    &node,
                    &origin,
                    ActionDraft {
                        kind: CanonicalActionKind::Introduce,
                        resource: self.resource_for_local(&binding),
                        source: Some(target.clone()),
                        target: Some(target),
                        loan: None,
                    },
                );
            }
        }
        self.visit_expr(&arm.body, None);
    }

    fn emit_consumes(
        &mut self,
        kind: CanonicalActionKind,
        places: Vec<Place>,
        node: &NodeId,
        origin: &crate::core::Origin,
    ) {
        let mut seen = BTreeSet::new();
        for place in places {
            // Audit 2026-08-05 (wave-2, G-1): a place may own several
            // resource identities after an aggregate merge (`let x = (a, b);
            // return x`); consuming it discharges every one of them.
            for resource in self.resources_for_place(&place) {
                if !seen.insert(resource.clone()) {
                    continue;
                }
                self.push_action(
                    node,
                    origin,
                    ActionDraft {
                        kind,
                        resource,
                        source: Some(place.clone()),
                        target: None,
                        loan: None,
                    },
                );
            }
        }
    }

    fn push_action(&mut self, node: &NodeId, origin: &crate::core::Origin, draft: ActionDraft) {
        // v0.34.8 (golden §6.2): restore the double-consume debug assertion.
        // Only `Drop` triggers — Move is a transfer (aggregate move-through
        // legitimately emits Move → Move chains for one resource).
        //
        // v0.34.8 偏差：实现为 warn-only 而非 panic。用户错误（如
        // session_double_close_rejected 的 E0304 场景）也会触发第二次
        // Drop——panic 会把合法 L2 负测试炸成 ICE。warn 保留检测信号，
        // checker 诊断（E0304/E0425）正常完成。精确路径分析排 1.x。
        #[cfg(debug_assertions)]
        {
            use crate::core::CanonicalActionKind;
            if draft.kind == CanonicalActionKind::Drop {
                mimi_assert!(
                    !self.consumed_resources.contains(&draft.resource),
                    "RESOURCE-LINEAR-001: resource {:?} dropped twice (debug signal) — \
                     genuine double-drop indicates an ownership-analysis bug; \
                     user-level double-consume is caught by E0304/E0425 diagnostics.",
                    draft.resource
                );
                self.consumed_resources.insert(draft.resource.clone());
            }
        }
        let location = self.location(node, origin);
        self.actions.push(CanonicalResourceAction {
            kind: draft.kind,
            resource: draft.resource,
            source: draft.source,
            target: draft.target,
            loan: draft.loan,
            location,
            span: origin.user_span(),
            origin: origin.clone(),
        });
    }

    fn entry_location(&self) -> CfgLocation {
        let block = self
            .cfg
            .block(&self.cfg.entry)
            .expect("validated CFG has an entry block");
        CfgLocation {
            block: self.cfg.entry.clone(),
            point: block.source.node.clone(),
            edge: None,
        }
    }

    fn location(&mut self, node: &NodeId, origin: &crate::core::Origin) -> CfgLocation {
        self.locations.get(node).cloned().unwrap_or_else(|| {
            self.errors.push(Diagnostic::error(
                format!("resource action '{}' has no CFG point", node.0),
                origin.user_span(),
            ));
            self.entry_location()
        })
    }

    fn is_linear(&self, ty: &ResolvedTypeId) -> bool {
        match self.types.get(ty) {
            Some(ResolvedType::Capability(_)) => true,
            // 0.31.16: Flow state sets (multi-target transition results)
            // are linear — each state value can only be consumed once.
            Some(ResolvedType::FlowStateSet { .. }) => true,
            // SD-1: read structural flag set at interning time.
            // Replaces `starts_with("state:")` / `ends_with("SessionChan")`
            // string matching. Single source of truth: NominalTypeId::nominal_is_linear().
            Some(ResolvedType::Nominal { is_linear, .. }) if *is_linear => true,
            // H2 (audit-type 2026-08-03): builtin container nominals (List/
            // Map/Set intern as Nominal, unlike the structural Option/Result/
            // Array/Slice variants above) are linear when any type argument
            // is linear. Without this, `func sink(v: List<cap>) { }` could
            // discard the container — and every element — without consuming
            // anything, defeating exactly-once in concrete signatures.
            Some(ResolvedType::Nominal {
                item, arguments, ..
            }) => {
                matches!(
                    item.as_str(),
                    "builtin:type:List" | "builtin:type:Map" | "builtin:type:Set"
                ) && arguments.iter().any(|arg| self.is_linear(arg))
            }
            Some(ResolvedType::Newtype { inner, .. }) => self.is_linear(inner),
            Some(ResolvedType::Tuple(elements)) => {
                elements.iter().any(|element| self.is_linear(element))
            }
            // P0-5: Recurse through container types. A linear resource
            // inside Option/Result/Array/Slice is still linear and must
            // be tracked (Introduce, Move, return check). Without this,
            // `Option<cap Token>` is invisible to the analysis.
            Some(ResolvedType::Option(inner)) => self.is_linear(inner),
            Some(ResolvedType::Result { ok, error }) => self.is_linear(ok) || self.is_linear(error),
            Some(ResolvedType::Array { element, .. }) => self.is_linear(element),
            Some(ResolvedType::Slice(inner)) => self.is_linear(inner),
            Some(ResolvedType::CBuffer(inner)) => self.is_linear(inner),
            // P0-6: GenericParameter returns false (conservative). Without
            // monomorphization, we cannot know if T will be instantiated
            // with a linear type. This is a documented analysis limitation:
            // generic functions may miss linear resource tracking.
            _ => false,
        }
    }

    fn place_is_linear(&self, place: &ResolvedPlace) -> bool {
        self.place_type(place).is_some_and(|ty| self.is_linear(&ty))
    }

    /// 0.36.47 容器方法变换面判定：结果 = 名义容器（List/Map/Set）或元组。
    /// 变换方法与 for 迭代同构——容器整体转出、义务移至结果；读取/提取方法
    /// （标量/裸元素/Option/Result 结果）保持借用面。
    fn method_transform_result(&self, call: &ResolvedCall) -> bool {
        match self.types.get(&call.result) {
            Some(ResolvedType::Nominal { item, .. }) => matches!(
                item.as_str(),
                "builtin:type:List" | "builtin:type:Map" | "builtin:type:Set"
            ),
            Some(ResolvedType::Tuple(_)) => true,
            _ => false,
        }
    }

    /// 0.36.46 定向头提取（绑定初始化器形状）：初始化为 `Load(xs[0])` 的
    /// 单投影字面量 0 + 非可弃线性容器基——目录与 Bind 臂共用的放行判定。
    fn is_directional_head_binding(&self, initializer: Option<&ResolvedExpr>) -> bool {
        let Some(value) = initializer else {
            return false;
        };
        let ResolvedExprKind::Load(place) = &value.kind else {
            return false;
        };
        if !self.is_directional_head_index(place) {
            return false;
        }
        self.body
            .locals
            .get(&place.base)
            .is_some_and(|l| self.is_linear(&l.ty) && !self.is_droppable_type(&l.ty))
    }

    /// 0.36.46 定向头提取形状：`xs[0]`——单一投影、字面量常量 0 的 Index。
    /// 定向 = 只开头部位置；`xs[1]` / 动态索引 / 多级投影保持 fail-closed。
    fn is_directional_head_index(&self, place: &ResolvedPlace) -> bool {
        place.projections.len() == 1
            && matches!(
                &place.projections[0],
                ResolvedProjection::Index {
                    index: ResolvedIndex::Constant(0),
                    ..
                }
            )
    }

    /// M9 (0.36.22): element extraction by INDEX READ from a linear container
    /// was the fail-open member of the element-consumption gap — the ledger
    /// attributed the whole container as consumed by the read, but only the
    /// extracted handle was released, silently leaking every unextracted
    /// element (inconsistent with match/for extraction, which are fail-closed
    /// E0256/E0304). 0.36.25: the SLICE sibling (`v[1..]`) copies the same
    /// handle values while consuming the container obligation — identical
    /// leak, closed identically. Reject uniformly: a linear container must
    /// be moved or dropped as a whole. 0.36.46: 定向头提取面（let 绑定 +
    /// 字面量 0）例外——见 is_directional_head_index 与 Bind 臂配对。
    fn reject_index_read_extraction(&mut self, expression: &ResolvedExpr) {
        // Non-droppable linear element containers (Cap/SessionChan) leak
        // every unextracted element on element-level reads (M9/slice).
        // Flow-state-element containers are auto-droppable at scope exit
        // (0.31.16 P0-5), so element reads there are a sanctioned pattern
        // and stay legal.
        let non_droppable_linear_container = |local: &ResolvedLocal| -> bool {
            self.is_linear(&local.ty) && !self.is_droppable_type(&local.ty)
        };
        let mut emitted = false;
        match &expression.kind {
            ResolvedExprKind::Load(place) => {
                let has_index = place
                    .projections
                    .iter()
                    .any(|projection| matches!(projection, ResolvedProjection::Index { .. }));
                // 0.36.26: tuple field access `t.0` — extracting one atom from
                // a linear tuple leaks the sibling atoms (destructure instead).
                let has_tuple = place
                    .projections
                    .iter()
                    .any(|projection| matches!(projection, ResolvedProjection::Tuple { .. }));
                if has_index || has_tuple {
                    if let Some(local) = self.body.locals.get(&place.base) {
                        if non_droppable_linear_container(local) {
                            if has_index {
                                if self.in_bind_initializer && self.is_directional_head_index(place)
                                {
                                    // 0.36.46 定向头提取：`let c = xs[0]`——
                                    // 字面量 0、单一投影、直接局部基、非可弃
                                    // 线性容器。c 认领一个元素义务（Bind 臂做
                                    // fresh Introduce），容器保留余部义务（须
                                    // 整体消费一次：drop = 释放余部）。每容器
                                    // 至多一次索引提取——重复认领 → E0304。
                                    if !self.extracted_containers.insert(place.base.clone()) {
                                        self.errors.push(
                                            Diagnostic::error_code(
                                                crate::diagnostic::codes::E0304,
                                                format!(
                                                    "resource '{}' head element is claimed more than once",
                                                    self.local_name(&place.base)
                                                ),
                                                expression.origin.user_span(),
                                            )
                                            .with_help(&format!(
                                                "extract `{}[0]` at most once per container; consume the remainder with a whole-container drop/move/return",
                                                self.local_name(&place.base)
                                            )),
                                        );
                                        self.last_visit_rejected = true;
                                        emitted = true;
                                    } else {
                                        self.directional_extraction_base = Some(place.base.clone());
                                    }
                                    // 放行（不 reject）：Bind 臂专门配对。
                                } else {
                                    self.push_index_read_error(&place.base, expression);
                                    emitted = true;
                                }
                            } else {
                                self.push_element_leak_error(expression);
                                emitted = true;
                            }
                        }
                    }
                }
            }
            // 0.36.25: `v[1..]` on a linear container — the slice copies the
            // handle values (alias!) and only the slice's copies are dropped;
            // the container's own handles leak. Same fail-closed rule.
            ResolvedExprKind::Slice { target, .. } => {
                if let ResolvedExprKind::Load(place) = &target.kind {
                    if place.projections.is_empty() {
                        if let Some(local) = self.body.locals.get(&place.base) {
                            if non_droppable_linear_container(local) {
                                self.push_index_read_error(&place.base, expression);
                                emitted = true;
                            }
                        }
                    }
                }
            }
            // 0.36.26: non-place element extraction `[a, b][0]` / `(a, b).0` — the
            // collect side selects only the indexed element, so the pairing
            // balances and the unextracted linear elements leak silently.
            // Judged by the container's TYPE (list literals hold cap
            // constants, not places — per-element probing misses them).
            ResolvedExprKind::Project { value, projection } => {
                let is_element_extraction = matches!(
                    projection,
                    ResolvedValueProjection::Index(_) | ResolvedValueProjection::Tuple(_)
                );
                if is_element_extraction
                    && self.is_linear(&value.ty)
                    && !self.is_droppable_type(&value.ty)
                {
                    // Extracting the ONLY literal element is a whole
                    // consumption (no leak) — stay legal.
                    let single_literal = match &value.kind {
                        ResolvedExprKind::List(elements) => elements.len() == 1,
                        ResolvedExprKind::Tuple(elements) => elements.len() == 1,
                        _ => false,
                    };
                    if !single_literal {
                        self.push_element_leak_error(expression);
                        emitted = true;
                    }
                }
            }
            _ => {}
        }
        // 0.36.43: neuter the rejected extraction's sources — later consumers
        // (binds/calls/drops) must not fabricate transfers for a value that
        // never moved (the E0304 already fails the function; the fabricated
        // transfer used to double-drop the container on a later drop(v)).
        if emitted {
            self.last_visit_rejected = true;
            for place in self.capability_places(expression) {
                self.rejected_extraction_places.insert(place);
            }
        }
    }

    fn push_element_leak_error(&mut self, expression: &ResolvedExpr) {
        self.errors.push(
            Diagnostic::error_code(
                crate::diagnostic::codes::E0304,
                "element-level extraction from a linear container is not tracked and \
                     leaks every unextracted element"
                    .to_string(),
                expression.origin.user_span(),
            )
            .with_help(
                "move or drop the whole container, or destructure it to bind every \
                 element explicitly",
            ),
        );
    }

    fn push_index_read_error(&mut self, base: &ResolvedLocalId, expression: &ResolvedExpr) {
        self.errors.push(
            Diagnostic::error_code(
                crate::diagnostic::codes::E0304,
                format!(
                    "'{}' cannot be read by index or slice: element-level extraction \
                     from a linear container is not tracked and leaks every \
                     unextracted element",
                    self.local_name(base)
                ),
                expression.origin.user_span(),
            )
            .with_help(
                "move or drop the whole container (e.g. drop(v)) instead of \
                 indexing or slicing into it",
            ),
        );
    }

    fn place_type(&self, place: &ResolvedPlace) -> Option<ResolvedTypeId> {
        self.body
            .locals
            .get(&place.base)
            .map(|local| place.projected_type(local).clone())
    }

    fn linear_bindings(&self, pattern: &ResolvedPattern, bindings: &mut Vec<ResolvedLocalId>) {
        match &pattern.kind {
            ResolvedPatternKind::Binding { local, .. } if self.is_linear(&pattern.ty) => {
                bindings.push(local.clone());
            }
            ResolvedPatternKind::Constructor { fields, .. } => {
                for (_, pattern) in fields {
                    self.linear_bindings(pattern, bindings);
                }
            }
            ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => {
                for pattern in patterns {
                    self.linear_bindings(pattern, bindings);
                }
            }
            ResolvedPatternKind::Slice { prefix, rest } => {
                for pattern in prefix {
                    self.linear_bindings(pattern, bindings);
                }
                if let Some(rest) = rest {
                    self.linear_bindings(rest, bindings);
                }
            }
            ResolvedPatternKind::Wildcard
            | ResolvedPatternKind::Literal(_)
            | ResolvedPatternKind::Binding { .. } => {}
        }
    }

    fn single_binding(&self, pattern: &ResolvedPattern) -> Option<ResolvedLocalId> {
        match &pattern.kind {
            ResolvedPatternKind::Binding { local, .. } => Some(local.clone()),
            _ => None,
        }
    }

    /// True when the pattern contains a wildcard anywhere (nested included).
    /// Audit 2026-08-05 (wave-2, review §1.3): wildcard positions in a
    /// split() pattern silently discard capability atoms.
    fn pattern_has_wildcard(&self, pattern: &ResolvedPattern) -> bool {
        match &pattern.kind {
            ResolvedPatternKind::Wildcard => true,
            ResolvedPatternKind::Constructor { fields, .. } => fields
                .iter()
                .any(|(_, pattern)| self.pattern_has_wildcard(pattern)),
            ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => patterns
                .iter()
                .any(|pattern| self.pattern_has_wildcard(pattern)),
            ResolvedPatternKind::Slice { prefix, rest } => {
                prefix
                    .iter()
                    .any(|pattern| self.pattern_has_wildcard(pattern))
                    || rest
                        .as_deref()
                        .is_some_and(|pattern| self.pattern_has_wildcard(pattern))
            }
            ResolvedPatternKind::Literal(_) | ResolvedPatternKind::Binding { .. } => false,
        }
    }

    /// Number of top-level pattern positions. A split() destructure reports
    /// its tuple arity even when a wildcard ate one binding — the slot
    /// count, not the surviving linear-binding count, identifies the split
    /// shape so the wildcard rejection cannot be vacated by compression
    /// (`let (_, w) = c.split()` still counts 2 slots, 1 binding).
    fn pattern_slot_count(&self, pattern: &ResolvedPattern) -> usize {
        match &pattern.kind {
            ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => {
                patterns.len()
            }
            ResolvedPatternKind::Constructor { fields, .. } => fields.len(),
            _ => 1,
        }
    }

    /// Audit 2026-08-05 (wave-2, G-1): every resource identity currently
    /// owned by the source places, in construction order, deduplicated.
    /// Aggregate owners (`let x = (a, b)`) expand to all owned identities.
    fn resources_for_place(&self, place: &Place) -> Vec<ResourceId> {
        if place.projections.is_empty() {
            if let Some(resources) = self.resources.get(&ResolvedLocalId(place.base.0.clone())) {
                return resources.clone();
            }
        }
        vec![self.resource_for_place(place)]
    }

    fn expand_sources(&self, sources: &[Place]) -> Vec<ResourceId> {
        self.expand_source_pairs(sources)
            .into_iter()
            .map(|(resource, _)| resource)
            .collect()
    }

    /// (resource, originating place) pairs for the sources, deduplicated by
    /// resource with the first originating place kept. The place is the
    /// action source — the owner-validation target of dataflow (H-5).
    fn expand_source_pairs(&self, sources: &[Place]) -> Vec<(ResourceId, Place)> {
        let mut pairs = Vec::new();
        let mut seen = BTreeSet::new();
        for source in sources {
            for resource in self.resources_for_place(source) {
                if seen.insert(resource.clone()) {
                    pairs.push((resource, source.clone()));
                }
            }
        }
        pairs
    }

    /// Audit 2026-08-05 (wave-2, C-2): If/Match are XOR — exactly one arm's
    /// value flows at runtime. When a consumed value is (or contains, in
    /// value position) a branch expression whose arms carry SEVERAL distinct
    /// linear resources, consuming it can discharge at most one obligation;
    /// the others leak on every path not taking their arm. Returns one
    /// representative place per distinct resource when the value violates.
    /// AND aggregates are NOT violations: `(a, b)` moves every element.
    fn xor_branch_violation(&self, value: &ResolvedExpr) -> Option<Vec<Place>> {
        match &value.kind {
            ResolvedExprKind::If { .. } | ResolvedExprKind::Match { .. } => {
                let mut representatives: Vec<Place> = Vec::new();
                let mut seen = BTreeSet::new();
                for place in self.capability_places(value) {
                    for resource in self.resources_for_place(&place) {
                        if seen.insert(resource) {
                            representatives.push(place.clone());
                        }
                    }
                }
                (representatives.len() > 1).then_some(representatives)
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => block
                .result
                .as_ref()
                .and_then(|result| self.xor_branch_violation(result)),
            ResolvedExprKind::Scope { body, .. } => body
                .result
                .as_ref()
                .and_then(|result| self.xor_branch_violation(result)),
            ResolvedExprKind::Cast { value, .. }
            | ResolvedExprKind::Try { value, .. }
            | ResolvedExprKind::Spawn(value)
            | ResolvedExprKind::Await(value) => self.xor_branch_violation(value),
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => values
                .iter()
                .find_map(|value| self.xor_branch_violation(value)),
            ResolvedExprKind::Record { fields, .. } => fields
                .iter()
                .find_map(|field| self.xor_branch_violation(&field.value)),
            ResolvedExprKind::Map(pairs) => pairs.iter().find_map(|(key, value)| {
                self.xor_branch_violation(key)
                    .or_else(|| self.xor_branch_violation(value))
            }),
            ResolvedExprKind::Project { value, .. } => self.xor_branch_violation(value),
            _ => None,
        }
    }

    fn push_xor_diagnostic(&mut self, representatives: &[Place], origin: &crate::core::Origin) {
        let names = representatives
            .iter()
            .map(Place::display)
            .collect::<Vec<_>>()
            .join("', '");
        self.errors.push(
            Diagnostic::error_code(
                crate::diagnostic::codes::E0840,
                format!(
                    "branch expression carries {} distinct linear resources ('{}') but exactly \
                     one flows at runtime — consuming it leaks every arm that is not taken",
                    representatives.len(),
                    names
                ),
                origin.user_span(),
            )
            .with_help(
                "consume each capability on its own control-flow path, or bind the branches to \
                 distinct places and consume them separately",
            ),
        );
    }

    /// Audit 2026-08-05 (wave-2, review §5.5): the call whose linear result a
    /// statement-style expression discards (through any number of plain
    /// block/scope wrappers). Used to establish the dropped obligation.
    fn discarded_linear_call<'b>(
        &self,
        mut expression: &'b ResolvedExpr,
    ) -> Option<&'b ResolvedExpr> {
        loop {
            match &expression.kind {
                ResolvedExprKind::Call(_) => {
                    return (self.is_linear(&expression.ty)
                        && !self.is_droppable_type(&expression.ty))
                    .then_some(expression);
                }
                ResolvedExprKind::Block(block)
                | ResolvedExprKind::Comptime(block)
                | ResolvedExprKind::Quote(block) => {
                    expression = block.result.as_ref()?;
                }
                ResolvedExprKind::Scope { body, .. } => {
                    expression = body.result.as_ref()?;
                }
                _ => return None,
            }
        }
    }

    /// Audit 2026-08-05 (wave-2, H-6): terminate anonymous loans parked
    /// during statement visits at the statement's terminating CFG point.
    fn flush_pending_loans(&mut self, node: &NodeId, origin: &crate::core::Origin) {
        if self.pending_anonymous_loans.is_empty() {
            return;
        }
        let Some(location) = self.locations.get(node).cloned() else {
            return; // no CFG point here — keep them for the next flush site
        };
        let pending = std::mem::take(&mut self.pending_anonymous_loans);
        for (loan_id, resource) in pending {
            self.actions.push(CanonicalResourceAction {
                kind: CanonicalActionKind::BorrowEnd,
                resource,
                source: None,
                target: None,
                loan: Some(loan_id),
                location: location.clone(),
                span: origin.user_span(),
                origin: origin.clone(),
            });
        }
    }

    fn capability_places(&self, expression: &ResolvedExpr) -> Vec<Place> {
        let mut places = Vec::new();
        self.collect_capability_places(expression, &mut places);
        places
    }

    fn collect_capability_places(&self, expression: &ResolvedExpr, places: &mut Vec<Place>) {
        match &expression.kind {
            ResolvedExprKind::Load(place) if self.place_is_linear(place) => {
                let canonical = self.canonical_place(place);
                if !self.rejected_extraction_places.contains(&canonical) {
                    places.push(canonical);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.collect_capability_places(value, places);
                }
            }
            ResolvedExprKind::Record { fields, .. } => {
                for field in fields {
                    self.collect_capability_places(&field.value, places);
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                let selected = match (projection, &value.kind) {
                    (
                        ResolvedValueProjection::Field(projected),
                        ResolvedExprKind::Record { fields, .. },
                    ) => fields
                        .iter()
                        .find(|field| &field.field == projected)
                        .map(|field| &field.value),
                    (ResolvedValueProjection::Tuple(index), ResolvedExprKind::Tuple(values)) => {
                        values.get(*index)
                    }
                    (ResolvedValueProjection::Index(index), ResolvedExprKind::List(values)) => {
                        match &index.kind {
                            ResolvedExprKind::Literal(crate::core::ResolvedLiteral::Int(index))
                                if *index >= 0 =>
                            {
                                values.get(*index as usize)
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(selected) = selected {
                    self.collect_capability_places(selected, places);
                } else {
                    // The typed projection is closed but not statically
                    // separable. Conservatively consume all candidate linear
                    // inputs instead of inventing a partial-move identity.
                    self.collect_capability_places(value, places);
                }
            }
            ResolvedExprKind::Cast { value, .. } => self.collect_capability_places(value, places),
            ResolvedExprKind::Block(block) => {
                if let Some(value) = &block.result {
                    self.collect_capability_places(value, places);
                }
            }
            ResolvedExprKind::If {
                then_block,
                else_block,
                ..
            } => {
                if let Some(result) = &then_block.result {
                    self.collect_capability_places(result, places);
                }
                if let Some(result) = &else_block.result {
                    self.collect_capability_places(result, places);
                }
            }
            ResolvedExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.collect_capability_places(&arm.body, places);
                }
            }
            // P0-7: Previously silently ignored by `_ => {}`. Linear resources
            // inside these expressions were invisible to the analysis — no
            // Move/TransferChild actions generated, no E0304 double-consume
            // diagnostics.
            ResolvedExprKind::Spawn(inner) | ResolvedExprKind::Await(inner) => {
                self.collect_capability_places(inner, places);
            }
            // NOTE: Call arguments are NOT collected here — they are already
            // handled at the call-site level (visit_expr's Call arm above),
            // which calls capability_places on each argument directly. Adding
            // Call here would double-count and cause false E0304 diagnostics.
            ResolvedExprKind::Map(pairs) => {
                for (key, value) in pairs {
                    self.collect_capability_places(key, places);
                    self.collect_capability_places(value, places);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.collect_capability_places(value, places);
                self.collect_capability_places(iterable, places);
                if let Some(guard) = guard {
                    self.collect_capability_places(guard, places);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => {
                self.collect_capability_places(receiver, places);
            }
            ResolvedExprKind::Range { start, end } => {
                self.collect_capability_places(start, places);
                self.collect_capability_places(end, places);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.collect_capability_places(target, places);
                if let Some(start) = start {
                    self.collect_capability_places(start, places);
                }
                if let Some(end) = end {
                    self.collect_capability_places(end, places);
                }
            }
            ResolvedExprKind::Unary { operand, .. } => {
                self.collect_capability_places(operand, places);
            }
            ResolvedExprKind::Try { value, .. }
            | ResolvedExprKind::TypeOf(value)
            | ResolvedExprKind::Old(value) => {
                self.collect_capability_places(value, places);
            }
            ResolvedExprKind::Scope { body, .. } | ResolvedExprKind::Comptime(body) => {
                if let Some(result) = &body.result {
                    self.collect_capability_places(result, places);
                }
            }
            // Leaf / non-place expressions: no linear resources to track.
            // Call is handled at the call-site level (visit_expr's Call arm),
            // NOT here — recursing into arguments would double-count.
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::FString(_)
            | ResolvedExprKind::Lambda(_)
            | ResolvedExprKind::Quote(_)
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::Binary { .. }
            | ResolvedExprKind::Call(_)
            | ResolvedExprKind::Load(_) => {}
        }
    }

    /// Primary resource identity of a local (the first owned identity).
    /// Introduced obligations and session endpoints always carry a single
    /// identity; aggregate owners are consumed through
    /// `resources_for_place`, which expands every owned identity.
    fn resource_for_local(&self, local: &ResolvedLocalId) -> ResourceId {
        self.resources
            .get(local)
            .and_then(|resources| resources.first().cloned())
            .unwrap_or_else(|| ResourceId(local.0.clone()))
    }

    fn resource_for_place(&self, place: &Place) -> ResourceId {
        self.resources
            .get(&ResolvedLocalId(place.base.0.clone()))
            .and_then(|resources| resources.first().cloned())
            .unwrap_or_else(|| ResourceId(place.base.0.clone()))
    }

    fn place_from_local(&self, local: &ResolvedLocalId) -> Place {
        Place::root(LocalId(local.0.clone()), self.local_name(local))
    }

    fn local_name(&self, local: &ResolvedLocalId) -> String {
        self.body
            .locals
            .get(local)
            .map(|local| local.display_name.clone())
            .unwrap_or_else(|| local.0 .0.clone())
    }

    fn canonical_place(&self, place: &ResolvedPlace) -> Place {
        let projections = place
            .projections
            .iter()
            .map(|projection| match projection {
                ResolvedProjection::Field { field, name, .. } => PlaceProjection::Field {
                    field: field.clone(),
                    name: name.clone(),
                },
                ResolvedProjection::Tuple { index, .. } => PlaceProjection::Tuple(*index),
                ResolvedProjection::Index { index, .. } => PlaceProjection::Index(match index {
                    ResolvedIndex::Constant(index) => IndexProjection::Constant(*index),
                    ResolvedIndex::Dynamic(_) => IndexProjection::Dynamic,
                }),
                ResolvedProjection::Deref { .. } => PlaceProjection::Deref,
            })
            .collect();
        Place {
            base: LocalId(place.base.0.clone()),
            base_name: self.local_name(&place.base),
            projections,
        }
    }

    fn for_each_expr_child(
        &mut self,
        expression: &ResolvedExpr,
        mut visit: impl FnMut(&mut Self, &ResolvedExpr),
    ) {
        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        visit(self, value);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                visit(self, value);
                if let ResolvedValueProjection::Index(index) = projection {
                    visit(self, index);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                visit(self, left);
                visit(self, right);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => visit(self, operand),
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    visit(self, &argument.value);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    visit(self, value);
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    visit(self, key);
                    visit(self, value);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                visit(self, iterable);
                if let Some(guard) = guard {
                    visit(self, guard);
                }
                visit(self, value);
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => visit(self, receiver),
            ResolvedExprKind::Record { fields, .. } => {
                for field in fields {
                    visit(self, &field.value);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                visit(self, start);
                visit(self, end);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                visit(self, target);
                if let Some(start) = start {
                    visit(self, start);
                }
                if let Some(end) = end {
                    visit(self, end);
                }
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::Lambda(_)
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_)
            | ResolvedExprKind::Block(_)
            | ResolvedExprKind::Scope { .. }
            | ResolvedExprKind::Comptime(_)
            | ResolvedExprKind::If { .. }
            | ResolvedExprKind::Match { .. }
            | ResolvedExprKind::Quote(_) => {}
        }
    }
}

pub fn analyze_resolved_bodies(
    cfgs: &BTreeMap<NodeId, CallableCfg>,
    bodies: &BTreeMap<NodeId, ResolvedBody>,
    signatures: &BTreeMap<NodeId, ResolvedSignature>,
    types: &ResolvedTypeTable,
) -> Result<BTreeMap<NodeId, ResourceAnalysis>, Vec<Diagnostic>> {
    let mut analyses = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, cfg) in cfgs {
        let Some(body) = bodies.get(owner) else {
            errors.push(Diagnostic::error(
                format!("CFG '{}' has no ResolvedBody", owner.0),
                cfg.block(&cfg.entry)
                    .map(|block| block.source.span)
                    .unwrap_or(crate::span::Span::UNKNOWN),
            ));
            continue;
        };
        let Some(signature) = signatures.get(owner) else {
            errors.push(Diagnostic::error(
                format!("CFG '{}' has no ResolvedSignature", owner.0),
                body.root.origin.user_span(),
            ));
            continue;
        };
        match ActionEmitter::new(cfg, body, signature, types).emit() {
            Ok(analysis) => {
                analyses.insert(owner.clone(), analysis);
            }
            Err(mut action_errors) => errors.append(&mut action_errors),
        }
    }
    if errors.is_empty() {
        Ok(analyses)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> crate::ast::File {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse")
    }

    fn action_location_exists(cfg: &CallableCfg, action: &CanonicalResourceAction) -> bool {
        let Some(block) = cfg.block(&action.location.block) else {
            return false;
        };
        if let Some(edge_id) = &action.location.edge {
            return cfg
                .edge(edge_id)
                .is_some_and(|edge| edge.from == action.location.block);
        }
        block.source.node == action.location.point
            || block
                .points
                .iter()
                .any(|point| point.source.node == action.location.point)
    }

    #[test]
    fn typed_binding_move_preserves_resource_identity_and_only_root_result_returns() {
        // RESOURCE-LINEAR-001: a binding move changes the owner place, not the
        // logical resource identity; nested block results are not callable returns.
        let file = parse(
            r#"
cap Token
func forward(token: cap Token) -> cap Token {
    let moved = { token }
    moved
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("typed binding move checks");
        let owner = NodeId("function:forward".into());
        let body = program.resolved_body(&owner).expect("forward body");
        let token = body
            .locals
            .values()
            .find(|local| local.display_name == "token")
            .expect("token local");
        let expected = ResourceId(token.id.0.clone());
        let analysis = program
            .resource_analysis(&owner)
            .expect("resource analysis");

        let introduce = analysis
            .actions
            .iter()
            .find(|action| action.kind == CanonicalActionKind::Introduce)
            .expect("parameter introduction");
        let binding_move = analysis
            .actions
            .iter()
            .find(|action| {
                action.kind == CanonicalActionKind::Move
                    && action
                        .target
                        .as_ref()
                        .is_some_and(|place| place.display() == "moved")
            })
            .expect("binding move");
        let returns = analysis
            .actions
            .iter()
            .filter(|action| action.kind == CanonicalActionKind::Return)
            .collect::<Vec<_>>();

        assert_eq!(introduce.resource, expected);
        assert_eq!(binding_move.resource, expected);
        assert_eq!(returns.len(), 1, "nested block result must not return");
        assert_eq!(returns[0].resource, expected);
        assert_eq!(
            returns[0].source.as_ref().map(Place::display).as_deref(),
            Some("moved")
        );
    }

    #[test]
    fn typed_loans_keep_node_identity_place_precision_and_cfg_location() {
        // RESOURCE-LINEAR-001: canonical loans and places come from typed
        // nodes, retaining the distinction between constant and dynamic index.
        let file = parse(
            r#"
func inspect(xs: List<i32>, index: i32) -> i32 {
    let fixed = &xs[0]
    let dynamic = &xs[index]
    *fixed + *dynamic
}
func main() -> i32 { inspect([1, 2], 1) }
"#,
        );
        let program = crate::core::check_program(&file).expect("indexed loans check");
        let owner = NodeId("function:inspect".into());
        let analysis = program
            .resource_analysis(&owner)
            .expect("resource analysis");
        let cfg = program.callable_cfg(&owner).expect("callable CFG");
        let places = analysis
            .loans
            .iter()
            .map(|loan| loan.place.display())
            .collect::<BTreeSet<_>>();

        assert!(places.contains("xs[0]"));
        assert!(places.contains("xs[*]"));
        for loan in &analysis.loans {
            let action = analysis
                .actions
                .iter()
                .find(|action| action.loan.as_ref() == Some(&loan.id))
                .expect("loan action");
            assert_eq!(loan.id.0 .0, format!("{}/loan", action.location.point.0));
        }
        assert!(analysis
            .actions
            .iter()
            .all(|action| action_location_exists(cfg, action)));
    }

    #[test]
    fn canonical_return_gate_rejects_available_linear_resource() {
        // RESOURCE-LINEAR-001: return-path completeness belongs to the CFG
        // fixed point, not the legacy checker scope snapshots.
        let file = parse(
            r#"
cap Token
func leak(token: cap Token) -> i32 { 0 }
func main() -> i32 { 0 }
"#,
        );
        let errors = crate::core::check_program(&file)
            .expect_err("canonical return gate must reject the leak");
        assert!(errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::diagnostic::codes::E0256)
                && error.message
                    == "linear resource 'token' must be consumed before this return path"
        }));
    }

    #[test]
    fn linear_lambda_capture_transfers_resource_to_child() {
        // RESOURCE-LINEAR-001 + T-2 (audit 2026-08-05 wave-2): closure
        // capture of a LINEAR resource (cap/session/flow state) is rejected
        // at the checker with E0427 — a closure can be invoked more than
        // once, so a captured capability would be consumed on EVERY call,
        // escaping exactly-once enforcement. The old (pre-T-2) behavior
        // TransferChild'd the token at construction time and let the child
        // drop it, which double-consumes on a twice-called lambda.
        // The resource analysis still supports TransferChild for non-linear
        // captures; this test now pins the checker-level rejection.
        let file = parse(
            r#"
cap Token
func capture(token: cap Token) -> i32 {
    let child = fn() -> i32 { drop(token); 0 }
    0
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file)
            .expect_err("closure capture of a linear capability must be rejected (E0427)");
        assert!(
            program
                .iter()
                .any(|error| error.code.as_deref() == Some(crate::diagnostic::codes::E0427)),
            "expected E0427 for linear capture, got: {program:?}"
        );
    }

    #[test]
    fn sd1_flow_state_tracked_via_structural_flag() {
        // SD-1: flow state types are linear via the structural is_linear flag
        // on ResolvedType::Nominal, not via starts_with("state:") string matching.
        // Verify that a capability parameter is tracked as a linear resource
        // through the full check → resolve → resource_lower pipeline.
        let file = parse(
            r#"
cap Token
func process(token: cap Token) -> cap Token { token }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("cap program checks");
        let owner = NodeId("function:process".into());

        // Verify the resolved type has is_linear set structurally
        let body = program.resolved_body(&owner).expect("process body");
        let token_local = body
            .locals
            .values()
            .find(|local| local.display_name == "token")
            .expect("token local");
        let token_ty = program.resolved_types().get(&token_local.ty);
        // Capability types are tracked as linear (separate variant)
        assert!(
            matches!(token_ty, Some(crate::core::ResolvedType::Capability(_))),
            "cap Token must resolve to Capability variant"
        );

        // Verify resource analysis tracks it
        let analysis = program
            .resource_analysis(&owner)
            .expect("process resource analysis");
        assert!(
            analysis
                .actions
                .iter()
                .any(|action| action.kind == CanonicalActionKind::Introduce),
            "capability parameter must be tracked as linear resource"
        );
        assert!(
            analysis
                .actions
                .iter()
                .any(|action| action.kind == CanonicalActionKind::Return),
            "capability parameter must be returned"
        );
    }
}
