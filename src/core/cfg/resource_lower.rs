use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{
    DelegateTarget, MatchArm, Permission, ResolvedBlock, ResolvedExpr, ResolvedExprKind,
    ResolvedFStringPart, ResolvedIndex, ResolvedPattern, ResolvedPatternKind, ResolvedPlace,
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
    resources: BTreeMap<ResolvedLocalId, ResourceId>,
    actions: Vec<CanonicalResourceAction>,
    loans: Vec<Loan>,
    errors: Vec<Diagnostic>,
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
            let droppable: BTreeSet<ResourceId> = self
                .resources
                .iter()
                .filter(|(local, _)| {
                    self.body
                        .locals
                        .get(local)
                        .is_some_and(|l| self.is_linear(&l.ty))
                        && self
                            .body
                            .locals
                            .get(local)
                            .map(|l| &l.ty)
                            .and_then(|ty| self.types.get(ty))
                            .is_some_and(|ty| {
                                matches!(
                                    ty,
                                    ResolvedType::FlowStateSet { .. }
                                        | ResolvedType::Nominal { .. }
                                ) && self.is_flow_state_resolved(ty)
                            })
                })
                .map(|(_, resource)| resource.clone())
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

    /// 0.31.16: check whether a resolved type is a flow state (FlowStateSet
    /// or Nominal with "state:" prefix).
    fn is_flow_state_resolved(&self, ty: &ResolvedType) -> bool {
        match ty {
            ResolvedType::FlowStateSet { .. } => true,
            ResolvedType::Nominal { item, .. } => item.as_str().starts_with("state:"),
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
                    .insert(local.clone(), ResourceId(local.0.clone()));
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
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    self.catalog_block(body);
                }
                ResolvedStmtKind::Pinned {
                    value,
                    timeout,
                    binding,
                    body,
                } => {
                    self.catalog_expr(value);
                    if let Some(timeout) = timeout {
                        self.catalog_expr(timeout);
                    }
                    if let Some(binding) = binding {
                        if self
                            .body
                            .locals
                            .get(binding)
                            .is_some_and(|local| self.is_linear(&local.ty))
                        {
                            self.resources
                                .entry(binding.clone())
                                .or_insert_with(|| ResourceId(binding.0.clone()));
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
                | ResolvedStmtKind::Delegate { .. }
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
        let mut bindings = Vec::new();
        self.linear_bindings(pattern, &mut bindings);
        for (index, local) in bindings.into_iter().enumerate() {
            let resource = sources
                .get(index)
                .map(|source| self.resource_for_place(source))
                .unwrap_or_else(|| ResourceId(local.0.clone()));
            self.resources.insert(local, resource);
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
        for statement in &block.statements {
            self.visit_stmt(statement);
        }
        if let Some(result) = &block.result {
            self.visit_expr(result, None);
            if return_result {
                self.emit_consumes(
                    CanonicalActionKind::Return,
                    self.capability_places(result),
                    &result.node_id,
                    &result.origin,
                );
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
                    let sources = self.capability_places(initializer);
                    let mut bindings = Vec::new();
                    self.linear_bindings(pattern, &mut bindings);
                    for (index, binding) in bindings.into_iter().enumerate() {
                        let target = self.place_from_local(&binding);
                        if let Some(source) = sources.get(index) {
                            self.push_action(
                                &statement.node_id,
                                &statement.origin,
                                ActionDraft {
                                    kind: CanonicalActionKind::Move,
                                    resource: self.resource_for_place(source),
                                    source: Some(source.clone()),
                                    target: Some(target),
                                    loan: None,
                                },
                            );
                        } else {
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
                    if let Some(source) = self.capability_places(value).first() {
                        self.push_action(
                            &statement.node_id,
                            &statement.origin,
                            ActionDraft {
                                kind: CanonicalActionKind::Move,
                                resource: self.resource_for_place(source),
                                source: Some(source.clone()),
                                target: Some(self.canonical_place(target)),
                                loan: None,
                            },
                        );
                    }
                }
            }
            ResolvedStmtKind::Return { value, .. } => {
                if let Some(value) = value {
                    self.visit_expr(value, None);
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
                }
            }
            ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {}
            ResolvedStmtKind::Expr(expression) => self.visit_expr(expression, None),
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
                self.visit_block(body, false);
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
                    self.push_action(
                        &statement.node_id,
                        &statement.origin,
                        ActionDraft {
                            kind: CanonicalActionKind::Drop,
                            resource: self.resource_for_place(&place),
                            source: Some(place),
                            target: None,
                            loan: None,
                        },
                    );
                }
            }
            ResolvedStmtKind::Contract { condition, .. } => self.visit_expr(condition, None),
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    self.visit_expr(expression, None);
                }
            }
            ResolvedStmtKind::Delegate {
                permission,
                source,
                target,
            } => {
                if let DelegateTarget::Local(local) = target {
                    let _ = self.resource_for_local(local);
                }
                if *permission == Permission::Consume {
                    let place = self.canonical_place(source);
                    self.push_action(
                        &statement.node_id,
                        &statement.origin,
                        ActionDraft {
                            kind: CanonicalActionKind::DelegateConsume,
                            resource: self.resource_for_place(&place),
                            source: Some(place),
                            target: None,
                            loan: None,
                        },
                    );
                }
            }
            ResolvedStmtKind::Pinned {
                value,
                timeout,
                body,
                ..
            } => {
                self.visit_expr(value, None);
                if let Some(timeout) = timeout {
                    self.visit_expr(timeout, None);
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
                self.loans.push(Loan {
                    id: loan_id.clone(),
                    parent: parent_id,
                    kind,
                    place: source.clone(),
                    reference,
                    reference_name,
                    start: location.clone(),
                    end_edges: Vec::new(),
                    span: expression.origin.user_span(),
                });
                self.actions.push(CanonicalResourceAction {
                    kind: match kind {
                        LoanKind::Shared => CanonicalActionKind::BorrowShared,
                        LoanKind::Mutable => CanonicalActionKind::BorrowMut,
                    },
                    resource: self.resource_for_place(&source),
                    source: Some(source),
                    target: None,
                    loan: Some(loan_id),
                    location,
                    span: expression.origin.user_span(),
                    origin: expression.origin.clone(),
                });
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
            let resource = self.resource_for_place(&place);
            if !seen.insert(resource.clone()) {
                continue;
            }
            self.push_action(
                node,
                origin,
                ActionDraft {
                    kind,
                    resource,
                    source: Some(place),
                    target: None,
                    loan: None,
                },
            );
        }
    }

    fn push_action(&mut self, node: &NodeId, origin: &crate::core::Origin, draft: ActionDraft) {
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
            Some(ResolvedType::Nominal { item, .. }) => {
                item.as_str().ends_with("SessionChan")
                    || item.as_str().ends_with("session_chan")
                    // 0.31.16: individual flow state types use the
                    // "state:<flow>::<state>" identity prefix.
                    || item.as_str().starts_with("state:")
            }
            Some(ResolvedType::Newtype { inner, .. }) => self.is_linear(inner),
            Some(ResolvedType::Tuple(elements)) => {
                elements.iter().any(|element| self.is_linear(element))
            }
            _ => false,
        }
    }

    fn place_is_linear(&self, place: &ResolvedPlace) -> bool {
        self.place_type(place).is_some_and(|ty| self.is_linear(&ty))
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
            _ => {}
        }
    }

    fn resource_for_local(&self, local: &ResolvedLocalId) -> ResourceId {
        self.resources
            .get(local)
            .cloned()
            .unwrap_or_else(|| ResourceId(local.0.clone()))
    }

    fn resource_for_place(&self, place: &Place) -> ResourceId {
        self.resources
            .get(&ResolvedLocalId(place.base.0.clone()))
            .cloned()
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
        // RESOURCE-LINEAR-001: closure construction is an explicit ownership
        // transfer from the enclosing callable's resource state.
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
        let program = crate::core::check_program(&file).expect("closure capture transfers token");
        let analysis = program
            .resource_analysis(&NodeId("function:capture".into()))
            .expect("capture resource analysis");
        assert!(analysis.actions.iter().any(|action| {
            action.kind == CanonicalActionKind::TransferChild
                && action
                    .source
                    .as_ref()
                    .is_some_and(|place| place.display() == "token")
        }));
    }
}
