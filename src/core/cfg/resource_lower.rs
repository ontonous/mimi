use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{
    MatchArm, Permission, ResolvedBlock, ResolvedExpr, ResolvedExprKind, ResolvedFStringPart,
    ResolvedIndex, ResolvedLocal, ResolvedPattern, ResolvedPatternKind, ResolvedPlace,
    ResolvedProjection, ResolvedSignature, ResolvedStmt, ResolvedStmtKind, ResolvedUnaryOp,
    ResolvedValueProjection,
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
        let sources = initializer
            .map(|value| self.capability_places(value))
            .unwrap_or_default();
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
                    self.visit_expr(initializer, reference.as_ref());
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
                initializer, body, ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                self.visit_expr(initializer, None);
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
                initializer,
                then_block,
                else_block,
                ..
            } => {
                self.visit_expr(initializer, None);
                self.visit_block(then_block, false);
                if let Some(else_block) = else_block {
                    self.visit_block(else_block, false);
                }
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                self.visit_block(body, false);
            }
            ResolvedStmtKind::Drop(places) => {
                let places = places
                    .iter()
                    .filter(|place| self.place_is_linear(place))
                    .map(|place| self.canonical_place(place))
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

    fn visit_arm(&mut self, arm: &MatchArm) {
        if let Some(guard) = &arm.guard {
            self.visit_expr(guard, None);
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

    /// M9 (0.36.22): element extraction by INDEX READ from a linear container
    /// was the fail-open member of the element-consumption gap — the ledger
    /// attributed the whole container as consumed by the read, but only the
    /// extracted handle was released, silently leaking every unextracted
    /// element (inconsistent with match/for extraction, which are fail-closed
    /// E0256/E0304). 0.36.25: the SLICE sibling (`v[1..]`) copies the same
    /// handle values while consuming the container obligation — identical
    /// leak, closed identically. Reject uniformly: a linear container must
    /// be moved or dropped as a whole.
    fn reject_index_read_extraction(&mut self, expression: &ResolvedExpr) {
        // Non-droppable linear element containers (Cap/SessionChan) leak
        // every unextracted element on element-level reads (M9/slice).
        // Flow-state-element containers are auto-droppable at scope exit
        // (0.31.16 P0-5), so element reads there are a sanctioned pattern
        // and stay legal.
        let non_droppable_linear_container = |local: &ResolvedLocal| -> bool {
            self.is_linear(&local.ty) && !self.is_droppable_type(&local.ty)
        };
        match &expression.kind {
            ResolvedExprKind::Load(place) => {
                let has_index = place
                    .projections
                    .iter()
                    .any(|projection| matches!(projection, ResolvedProjection::Index { .. }));
                if has_index {
                    if let Some(local) = self.body.locals.get(&place.base) {
                        if non_droppable_linear_container(local) {
                            self.push_index_read_error(&place.base, expression);
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
                            }
                        }
                    }
                }
            }
            _ => {}
        }
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
                places.push(self.canonical_place(place));
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
