use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::core::{
    Availability, CanonicalActionKind, CanonicalResourceAction, CfgLocation, IndexProjection, Loan,
    LoanId, LoanKind, LocalId, OwnershipLedger, Place, PlaceProjection, ResourceActionKind,
    ResourceAnalysis, ResourceFact, ResourceId,
};
use crate::diagnostic::Diagnostic;

use super::{BasicBlockId, CallableCfg, EdgeKind};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FlowState {
    resources: BTreeMap<ResourceId, ResourceFact>,
    active_loans: BTreeSet<LoanId>,
}

pub fn analyze_cfgs(
    cfgs: &BTreeMap<crate::core::NodeId, CallableCfg>,
    ledgers: &std::collections::HashMap<crate::core::NodeId, OwnershipLedger>,
) -> Result<BTreeMap<crate::core::NodeId, ResourceAnalysis>, Vec<Diagnostic>> {
    let mut analyses = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, cfg) in cfgs {
        let Some(ledger) = ledgers.get(owner) else {
            analyses.insert(
                owner.clone(),
                ResourceAnalysis {
                    owner: owner.clone(),
                    actions: Vec::new(),
                    loans: Vec::new(),
                    in_states: BTreeMap::new(),
                    out_states: BTreeMap::new(),
                },
            );
            continue;
        };
        match analyze_one(cfg, ledger) {
            Ok(analysis) => {
                analyses.insert(owner.clone(), analysis);
            }
            Err(mut analysis_errors) => errors.append(&mut analysis_errors),
        }
    }
    if errors.is_empty() {
        Ok(analyses)
    } else {
        Err(errors)
    }
}

fn analyze_one(
    cfg: &CallableCfg,
    ledger: &OwnershipLedger,
) -> Result<ResourceAnalysis, Vec<Diagnostic>> {
    let mut actions = Vec::new();
    let mut loans = Vec::<Loan>::new();
    let mut legacy_ends: BTreeMap<String, Vec<CfgLocation>> = BTreeMap::new();

    for (ordinal, legacy) in ledger.actions.iter().enumerate() {
        let mut place = parse_place(&cfg.owner, &legacy.resource);
        let parent = if place.projections.first() == Some(&PlaceProjection::Deref) {
            loans
                .iter()
                .rev()
                .find(|loan| loan.reference_name.as_deref() == Some(place.base_name.as_str()))
                .map(|loan| {
                    place = loan.place.clone();
                    loan.id.clone()
                })
        } else {
            None
        };
        let resource = ResourceId(place.base.0.clone());
        let location = locate(cfg, legacy.span);
        let kind = canonical_kind(legacy.kind);
        if kind == CanonicalActionKind::BorrowEnd {
            legacy_ends
                .entry(place.display())
                .or_default()
                .push(location);
            continue;
        }
        let loan_kind = match kind {
            CanonicalActionKind::BorrowShared => Some(LoanKind::Shared),
            CanonicalActionKind::BorrowMut => Some(LoanKind::Mutable),
            _ => None,
        };
        let loan_id = loan_kind.map(|_| {
            LoanId(crate::core::NodeId(format!(
                "{}/loan:{}:{ordinal}",
                cfg.owner.0,
                stable_place_fragment(&place.display())
            )))
        });
        if let (Some(loan_kind), Some(loan_id)) = (loan_kind, loan_id.clone()) {
            let reference_name = infer_reference_name(cfg, &location);
            let reference = reference_name.as_ref().map(|name| {
                LocalId(crate::core::NodeId(format!(
                    "{}/local:{}",
                    cfg.owner.0,
                    stable_place_fragment(name)
                )))
            });
            loans.push(Loan {
                id: loan_id,
                parent,
                kind: loan_kind,
                place: place.clone(),
                reference,
                reference_name,
                start: location.clone(),
                end_edges: Vec::new(),
                span: legacy.span,
            });
        }
        actions.push(CanonicalResourceAction {
            kind,
            resource,
            source: Some(place.clone()),
            target: (kind == CanonicalActionKind::Introduce).then_some(place),
            loan: loan_id,
            location,
            span: legacy.span,
            origin: cfg
                .blocks
                .values()
                .find(|block| block.source.span == legacy.span)
                .map(|block| block.source.origin)
                .unwrap_or(crate::ast::AstOrigin::User),
        });
    }

    let (live_in, live_out) = compute_liveness(cfg);
    for loan in &mut loans {
        let locations = loan
            .reference_name
            .as_ref()
            .map(|reference| {
                liveness_end_locations(cfg, reference, &loan.start, &live_in, &live_out)
            })
            .filter(|locations| !locations.is_empty())
            .unwrap_or_else(|| {
                legacy_ends
                    .get(&loan.place.display())
                    .cloned()
                    .unwrap_or_default()
            });
        loan.end_edges = locations
            .iter()
            .filter_map(|location| location.edge.clone())
            .collect();
        for location in locations {
            let (span, origin) = location_source(cfg, &location);
            actions.push(CanonicalResourceAction {
                kind: CanonicalActionKind::BorrowEnd,
                resource: ResourceId(loan.place.base.0.clone()),
                source: Some(loan.place.clone()),
                target: None,
                loan: Some(loan.id.clone()),
                location,
                span,
                origin,
            });
        }
    }
    let borrowed_roots: BTreeMap<_, _> = loans
        .iter()
        .map(|loan| (loan.place.base_name.clone(), loan.place.base.clone()))
        .collect();
    for (block_id, block) in &cfg.blocks {
        if !cfg.reachable.contains(block_id) {
            continue;
        }
        for point in &block.points {
            for spelling in &point.reads {
                let place = parse_place(&cfg.owner, spelling);
                let Some(local) = borrowed_roots.get(&place.base_name) else {
                    continue;
                };
                let mut place = place;
                place.base = local.clone();
                actions.push(CanonicalResourceAction {
                    kind: CanonicalActionKind::Read,
                    resource: ResourceId(local.0.clone()),
                    source: Some(place),
                    target: None,
                    loan: None,
                    location: CfgLocation {
                        block: block_id.clone(),
                        point: point.source.node.clone(),
                        edge: None,
                    },
                    span: point.source.span,
                    origin: point.source.origin,
                });
            }
            for spelling in &point.writes {
                let place = parse_place(&cfg.owner, spelling);
                let Some(local) = borrowed_roots.get(&place.base_name) else {
                    continue;
                };
                let mut place = place;
                place.base = local.clone();
                actions.push(CanonicalResourceAction {
                    kind: CanonicalActionKind::Write,
                    resource: ResourceId(local.0.clone()),
                    source: Some(place),
                    target: None,
                    loan: None,
                    location: CfgLocation {
                        block: block_id.clone(),
                        point: point.source.node.clone(),
                        edge: None,
                    },
                    span: point.source.span,
                    origin: point.source.origin,
                });
            }
        }
    }

    let loan_catalog: BTreeMap<_, _> = loans.iter().map(|loan| (loan.id.clone(), loan)).collect();
    let mut in_flow = BTreeMap::<BasicBlockId, FlowState>::new();
    let mut out_flow = BTreeMap::<BasicBlockId, FlowState>::new();
    let mut queue: VecDeque<_> = cfg.reachable.iter().cloned().collect();
    let mut queued: BTreeSet<_> = queue.iter().cloned().collect();
    let mut errors = Vec::new();

    while let Some(block) = queue.pop_front() {
        queued.remove(&block);
        let incoming = if block == cfg.entry {
            FlowState::default()
        } else {
            join_predecessors(cfg, &block, &out_flow, &actions, &mut errors)
        };
        let changed_in = in_flow.get(&block) != Some(&incoming);
        if changed_in {
            in_flow.insert(block.clone(), incoming.clone());
        }
        let mut outgoing = incoming;
        let mut block_actions: Vec<_> = actions
            .iter()
            .filter(|action| action.location.block == block && action.location.edge.is_none())
            .collect();
        block_actions.sort_by_key(|action| {
            (
                point_order(cfg, &block, &action.location.point),
                action_rank(action.kind),
            )
        });
        for action in block_actions {
            transfer(action, &mut outgoing, &loan_catalog, &mut errors);
        }
        for edge in cfg.successors(&block) {
            if edge.kind == EdgeKind::Backedge && !outgoing.active_loans.is_empty() {
                errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0415,
                        "borrow remains live across a loop back-edge".to_string(),
                        edge.source.span,
                    )
                    .with_help("end the borrow before the next loop iteration"),
                );
            }
        }
        if out_flow.get(&block) != Some(&outgoing) {
            out_flow.insert(block.clone(), outgoing);
            for edge in cfg.successors(&block) {
                if cfg.reachable.contains(&edge.to) && queued.insert(edge.to.clone()) {
                    queue.push_back(edge.to.clone());
                }
            }
        }
    }

    dedup_errors(&mut errors);
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ResourceAnalysis {
        owner: cfg.owner.clone(),
        actions,
        loans,
        in_states: in_flow
            .into_iter()
            .map(|(block, state)| (block, state.resources))
            .collect(),
        out_states: out_flow
            .into_iter()
            .map(|(block, state)| (block, state.resources))
            .collect(),
    })
}

fn transfer(
    action: &CanonicalResourceAction,
    state: &mut FlowState,
    loans: &BTreeMap<LoanId, &Loan>,
    errors: &mut Vec<Diagnostic>,
) {
    match action.kind {
        CanonicalActionKind::Read => {
            reject_read_conflicts(action, state, loans, errors);
        }
        CanonicalActionKind::Write => {
            reject_conflicting_loans(action, state, loans, errors);
        }
        CanonicalActionKind::Introduce => {
            state.resources.insert(
                action.resource.clone(),
                ResourceFact {
                    availability: Availability::Available,
                    owner: action.target.clone().or_else(|| action.source.clone()),
                },
            );
        }
        CanonicalActionKind::Move
        | CanonicalActionKind::Drop
        | CanonicalActionKind::Return
        | CanonicalActionKind::TransferChild
        | CanonicalActionKind::DelegateConsume => {
            reject_conflicting_loans(action, state, loans, errors);
            let fact = state
                .resources
                .entry(action.resource.clone())
                .or_insert(ResourceFact {
                    availability: Availability::Available,
                    owner: action.source.clone(),
                });
            if fact.availability != Availability::Available {
                errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0304,
                    format!(
                        "resource '{}' is consumed more than once on this CFG path",
                        action
                            .source
                            .as_ref()
                            .map(Place::display)
                            .unwrap_or_else(|| action.resource.0 .0.clone())
                    ),
                    action.span,
                ));
            }
            fact.availability = Availability::Consumed;
            fact.owner = None;
        }
        CanonicalActionKind::TransferSession => {
            reject_conflicting_loans(action, state, loans, errors);
        }
        CanonicalActionKind::BorrowShared | CanonicalActionKind::BorrowMut => {
            let Some(new_id) = &action.loan else {
                return;
            };
            let Some(new_loan) = loans.get(new_id) else {
                return;
            };
            for active_id in &state.active_loans {
                let Some(active) = loans.get(active_id) else {
                    continue;
                };
                if new_loan.place.conflicts_with(&active.place)
                    && new_loan.parent.as_ref() != Some(active_id)
                    && (new_loan.kind == LoanKind::Mutable || active.kind == LoanKind::Mutable)
                {
                    errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0415,
                        format!(
                            "{} borrow of '{}' conflicts with an active {} borrow",
                            loan_kind_name(new_loan.kind),
                            new_loan.place.display(),
                            loan_kind_name(active.kind)
                        ),
                        action.span,
                    ));
                }
            }
            state.active_loans.insert(new_id.clone());
        }
        CanonicalActionKind::BorrowEnd => {
            if let Some(loan) = &action.loan {
                state.active_loans.remove(loan);
                return;
            }
            let Some(place) = &action.source else {
                return;
            };
            state.active_loans.retain(|loan_id| {
                loans
                    .get(loan_id)
                    .map_or(true, |loan| !loan.place.conflicts_with(place))
            });
        }
    }
}

fn reject_read_conflicts(
    action: &CanonicalResourceAction,
    state: &FlowState,
    loans: &BTreeMap<LoanId, &Loan>,
    errors: &mut Vec<Diagnostic>,
) {
    let Some(place) = &action.source else {
        return;
    };
    for active in &state.active_loans {
        if loans
            .get(active)
            .is_some_and(|loan| loan.kind == LoanKind::Mutable && loan.place.conflicts_with(place))
        {
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0415,
                format!(
                    "cannot read '{}' while it is mutably borrowed",
                    place.display()
                ),
                action.span,
            ));
        }
    }
}

fn reject_conflicting_loans(
    action: &CanonicalResourceAction,
    state: &FlowState,
    loans: &BTreeMap<LoanId, &Loan>,
    errors: &mut Vec<Diagnostic>,
) {
    let Some(place) = &action.source else {
        return;
    };
    for active in &state.active_loans {
        if loans
            .get(active)
            .is_some_and(|loan| loan.place.conflicts_with(place))
        {
            let operation = if action.kind == CanonicalActionKind::Write {
                "write"
            } else {
                "move or drop"
            };
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0415,
                format!(
                    "cannot {operation} '{}' while it is borrowed",
                    place.display()
                ),
                action.span,
            ));
        }
    }
}

fn join_predecessors(
    cfg: &CallableCfg,
    block: &BasicBlockId,
    out: &BTreeMap<BasicBlockId, FlowState>,
    actions: &[CanonicalResourceAction],
    errors: &mut Vec<Diagnostic>,
) -> FlowState {
    let predecessors: Vec<_> = cfg
        .predecessors(block)
        .into_iter()
        .filter(|edge| cfg.reachable.contains(&edge.from))
        .filter_map(|edge| {
            let mut state = out.get(&edge.from)?.clone();
            for action in actions.iter().filter(|action| {
                action.kind == CanonicalActionKind::BorrowEnd
                    && action.location.edge.as_ref() == Some(&edge.id)
            }) {
                if let Some(loan) = &action.loan {
                    state.active_loans.remove(loan);
                }
            }
            Some(state)
        })
        .collect();
    let Some(first) = predecessors.first() else {
        return FlowState::default();
    };
    let mut joined = first.clone();
    for state in predecessors.into_iter().skip(1) {
        let ids: BTreeSet<_> = joined
            .resources
            .keys()
            .chain(state.resources.keys())
            .cloned()
            .collect();
        for id in ids {
            let left = joined.resources.get(&id).cloned();
            let right = state.resources.get(&id).cloned();
            let fact = match (left, right) {
                (Some(left), Some(right)) if left == right => left,
                (Some(left), Some(right)) => {
                    emit_incompatible_join(cfg, block, &id, &left, &right, errors);
                    ResourceFact {
                        availability: Availability::MaybeConsumed,
                        owner: (left.owner == right.owner).then_some(left.owner).flatten(),
                    }
                }
                (Some(fact), None) | (None, Some(fact)) => {
                    emit_incompatible_join(
                        cfg,
                        block,
                        &id,
                        &fact,
                        &ResourceFact {
                            availability: Availability::MaybeConsumed,
                            owner: None,
                        },
                        errors,
                    );
                    ResourceFact {
                        availability: Availability::MaybeConsumed,
                        owner: fact.owner,
                    }
                }
                (None, None) => continue,
            };
            joined.resources.insert(id, fact);
        }
        joined
            .active_loans
            .extend(state.active_loans.iter().cloned());
    }
    joined
}

fn emit_incompatible_join(
    cfg: &CallableCfg,
    block: &BasicBlockId,
    resource: &ResourceId,
    left: &ResourceFact,
    right: &ResourceFact,
    errors: &mut Vec<Diagnostic>,
) {
    let name = left
        .owner
        .as_ref()
        .or(right.owner.as_ref())
        .map(Place::display)
        .unwrap_or_else(|| resource.0 .0.clone());
    let message = if left.owner != right.owner
        && left.availability == Availability::Available
        && right.availability == Availability::Available
    {
        format!(
            "resource '{}' is moved to incompatible places at a CFG join",
            name
        )
    } else {
        format!(
            "resource '{}' is consumed on only some reachable CFG paths",
            name
        )
    };
    errors.push(
        Diagnostic::error_code(
            crate::diagnostic::codes::E0304,
            message,
            cfg.block(block)
                .map(|block| block.source.span)
                .unwrap_or(crate::span::Span::UNKNOWN),
        )
        .with_help(
            "move, return, transfer, or drop the resource on every reachable path, or on none",
        ),
    );
}

fn infer_reference_name(cfg: &CallableCfg, start: &CfgLocation) -> Option<String> {
    let block = cfg.block(&start.block)?;
    let start_index = block
        .points
        .iter()
        .position(|point| point.source.node == start.point)
        .unwrap_or(0);
    block
        .points
        .iter()
        .skip(start_index + 1)
        .find_map(|point| point.defs.first().cloned())
}

fn compute_liveness(
    cfg: &CallableCfg,
) -> (
    BTreeMap<BasicBlockId, BTreeSet<String>>,
    BTreeMap<BasicBlockId, BTreeSet<String>>,
) {
    let mut live_in = BTreeMap::<BasicBlockId, BTreeSet<String>>::new();
    let mut live_out = BTreeMap::<BasicBlockId, BTreeSet<String>>::new();
    let mut changed = true;
    while changed {
        changed = false;
        for block_id in cfg.reachable.iter().rev() {
            let out: BTreeSet<String> = cfg
                .successors(block_id)
                .into_iter()
                .filter(|edge| cfg.reachable.contains(&edge.to))
                .flat_map(|edge| live_in.get(&edge.to).into_iter().flatten().cloned())
                .collect();
            let mut input = out.clone();
            if let Some(block) = cfg.block(block_id) {
                for point in block.points.iter().rev() {
                    for def in &point.defs {
                        input.remove(def);
                    }
                    input.extend(point.uses.iter().cloned());
                }
            }
            if live_out.get(block_id) != Some(&out) {
                live_out.insert(block_id.clone(), out);
                changed = true;
            }
            if live_in.get(block_id) != Some(&input) {
                live_in.insert(block_id.clone(), input);
                changed = true;
            }
        }
    }
    (live_in, live_out)
}

fn liveness_end_locations(
    cfg: &CallableCfg,
    reference: &str,
    start: &CfgLocation,
    live_in: &BTreeMap<BasicBlockId, BTreeSet<String>>,
    live_out: &BTreeMap<BasicBlockId, BTreeSet<String>>,
) -> Vec<CfgLocation> {
    let reachable_from_start = reachable_from(cfg, &start.block);
    let mut locations = Vec::new();
    let mut seen = BTreeSet::new();

    for block_id in &reachable_from_start {
        let Some(block) = cfg.block(block_id) else {
            continue;
        };
        let last_use = block
            .points
            .iter()
            .enumerate()
            .filter(|(_, point)| point.uses.iter().any(|name| name == reference))
            .map(|(index, point)| (index, point))
            .last();
        if let Some((index, point)) = last_use {
            let after_last_use = block.points.iter().skip(index + 1).any(|later| {
                later.uses.iter().any(|name| name == reference)
                    || later.defs.iter().any(|name| name == reference)
            });
            if !after_last_use
                && !live_out
                    .get(block_id)
                    .is_some_and(|live| live.contains(reference))
            {
                let key = (block_id.clone(), point.source.node.clone(), None);
                if seen.insert(key) {
                    locations.push(CfgLocation {
                        block: block_id.clone(),
                        point: point.source.node.clone(),
                        edge: None,
                    });
                }
            }
        }
    }

    for edge in cfg.edges.values() {
        if !reachable_from_start.contains(&edge.from) || !cfg.reachable.contains(&edge.to) {
            continue;
        }
        let source_live = live_out
            .get(&edge.from)
            .is_some_and(|live| live.contains(reference));
        let target_live = live_in
            .get(&edge.to)
            .is_some_and(|live| live.contains(reference));
        if source_live && !target_live {
            let point = cfg
                .block(&edge.from)
                .and_then(|block| block.points.last())
                .map(|point| point.source.node.clone())
                .unwrap_or_else(|| edge.source.node.clone());
            let key = (edge.from.clone(), point.clone(), Some(edge.id.clone()));
            if seen.insert(key) {
                locations.push(CfgLocation {
                    block: edge.from.clone(),
                    point,
                    edge: Some(edge.id.clone()),
                });
            }
        }
    }

    if locations.is_empty() {
        if let Some(block) = cfg.block(&start.block) {
            let binding = block
                .points
                .iter()
                .skip_while(|point| point.source.node != start.point)
                .find(|point| point.defs.iter().any(|name| name == reference));
            if let Some(point) = binding {
                locations.push(CfgLocation {
                    block: start.block.clone(),
                    point: point.source.node.clone(),
                    edge: None,
                });
            }
        }
    }
    locations
}

fn reachable_from(cfg: &CallableCfg, start: &BasicBlockId) -> BTreeSet<BasicBlockId> {
    let mut reachable = BTreeSet::new();
    let mut queue = VecDeque::from([start.clone()]);
    while let Some(block) = queue.pop_front() {
        if !reachable.insert(block.clone()) {
            continue;
        }
        for edge in cfg.successors(&block) {
            queue.push_back(edge.to.clone());
        }
    }
    reachable
}

fn location_source(
    cfg: &CallableCfg,
    location: &CfgLocation,
) -> (crate::span::Span, crate::ast::AstOrigin) {
    if let Some(edge) = location.edge.as_ref().and_then(|edge| cfg.edge(edge)) {
        return (edge.source.span, edge.source.origin);
    }
    cfg.block(&location.block)
        .and_then(|block| {
            block
                .points
                .iter()
                .find(|point| point.source.node == location.point)
                .map(|point| (point.source.span, point.source.origin))
        })
        .unwrap_or((crate::span::Span::UNKNOWN, crate::ast::AstOrigin::User))
}

fn locate(cfg: &CallableCfg, span: crate::span::Span) -> CfgLocation {
    for (block_id, block) in &cfg.blocks {
        if !cfg.reachable.contains(block_id) {
            continue;
        }
        if let Some(point) = block.points.iter().find(|point| point.source.span == span) {
            return CfgLocation {
                block: block_id.clone(),
                point: point.source.node.clone(),
                edge: None,
            };
        }
    }
    let block = cfg
        .block(&cfg.entry)
        .expect("validated CFG always has an entry block");
    CfgLocation {
        block: cfg.entry.clone(),
        point: block.source.node.clone(),
        edge: None,
    }
}

fn point_order(cfg: &CallableCfg, block: &BasicBlockId, point: &crate::core::NodeId) -> usize {
    cfg.block(block)
        .and_then(|block| {
            block
                .points
                .iter()
                .position(|candidate| &candidate.source.node == point)
        })
        .unwrap_or(usize::MAX)
}

fn canonical_kind(kind: ResourceActionKind) -> CanonicalActionKind {
    match kind {
        ResourceActionKind::Introduce => CanonicalActionKind::Introduce,
        ResourceActionKind::Move => CanonicalActionKind::Move,
        ResourceActionKind::Drop => CanonicalActionKind::Drop,
        ResourceActionKind::Return => CanonicalActionKind::Return,
        ResourceActionKind::BorrowShared => CanonicalActionKind::BorrowShared,
        ResourceActionKind::BorrowMut => CanonicalActionKind::BorrowMut,
        ResourceActionKind::BorrowEnd => CanonicalActionKind::BorrowEnd,
        ResourceActionKind::TransferSession => CanonicalActionKind::TransferSession,
        ResourceActionKind::TransferChild => CanonicalActionKind::TransferChild,
        ResourceActionKind::DelegateConsume => CanonicalActionKind::DelegateConsume,
    }
}

fn action_rank(kind: CanonicalActionKind) -> u8 {
    match kind {
        CanonicalActionKind::Read => 0,
        CanonicalActionKind::Write => 1,
        CanonicalActionKind::BorrowEnd => 2,
        CanonicalActionKind::Introduce => 3,
        CanonicalActionKind::BorrowShared | CanonicalActionKind::BorrowMut => 4,
        CanonicalActionKind::Move
        | CanonicalActionKind::Drop
        | CanonicalActionKind::Return
        | CanonicalActionKind::TransferSession
        | CanonicalActionKind::TransferChild
        | CanonicalActionKind::DelegateConsume => 5,
    }
}

fn parse_place(owner: &crate::core::NodeId, spelling: &str) -> Place {
    let mut rest = spelling;
    let mut deref = false;
    while let Some(stripped) = rest.strip_prefix('*') {
        deref = true;
        rest = stripped;
    }
    let base_end = rest.find(['.', '[']).unwrap_or(rest.len());
    let base_name = &rest[..base_end];
    let local = LocalId(crate::core::NodeId(format!(
        "{}/local:{}",
        owner.0,
        stable_place_fragment(base_name)
    )));
    let mut place = Place::root(local, base_name);
    if deref {
        place.projections.push(PlaceProjection::Deref);
    }
    rest = &rest[base_end..];
    while !rest.is_empty() {
        if let Some(field) = rest.strip_prefix('.') {
            let end = field.find(['.', '[']).unwrap_or(field.len());
            let segment = &field[..end];
            if let Ok(index) = segment.parse::<usize>() {
                place.projections.push(PlaceProjection::Tuple(index));
            } else {
                place
                    .projections
                    .push(PlaceProjection::Field(segment.to_string()));
            }
            rest = &field[end..];
        } else if let Some(index) = rest.strip_prefix('[') {
            let end = index.find(']').unwrap_or(index.len());
            let segment = &index[..end];
            place.projections.push(PlaceProjection::Index(
                segment
                    .parse::<i64>()
                    .map(IndexProjection::Constant)
                    .unwrap_or(IndexProjection::Dynamic),
            ));
            rest = index.get(end + 1..).unwrap_or_default();
        } else {
            break;
        }
    }
    place
}

fn stable_place_fragment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02x}")
            }
        })
        .collect()
}

fn loan_kind_name(kind: LoanKind) -> &'static str {
    match kind {
        LoanKind::Shared => "shared",
        LoanKind::Mutable => "mutable",
    }
}

fn dedup_errors(errors: &mut Vec<Diagnostic>) {
    let mut seen = BTreeSet::new();
    errors.retain(|error| {
        seen.insert((
            error.code.clone(),
            error.message.clone(),
            error.span.start_line,
            error.span.start_col,
        ))
    });
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

    #[test]
    fn structured_places_follow_conservative_overlap_rules() {
        let owner = crate::core::NodeId("function:test".into());
        let left = parse_place(&owner, "p.left");
        let right = parse_place(&owner, "p.right");
        let root = parse_place(&owner, "p");
        let dynamic = parse_place(&owner, "xs[*]");
        let fixed = parse_place(&owner, "xs[1]");
        assert!(!left.conflicts_with(&right));
        assert!(root.conflicts_with(&left));
        assert!(dynamic.conflicts_with(&fixed));
        assert!(!parse_place(&owner, "xs[0]").conflicts_with(&fixed));
    }

    #[test]
    fn fixed_point_joins_consumed_capabilities_on_reachable_predecessors() {
        let file = parse(
            r#"
cap Token
func close(flag: bool, token: cap Token) -> i32 {
    if flag { drop(token) } else { drop(token) }
    0
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("balanced branch checks");
        let owner = crate::core::NodeId("function:close".into());
        let analysis = program
            .resource_analysis(&owner)
            .expect("resource analysis");
        let token = ResourceId(crate::core::NodeId("function:close/local:token".into()));
        assert!(analysis.out_states.values().any(|state| state
            .get(&token)
            .is_some_and(|fact| { fact.availability == Availability::Consumed })));
        assert!(analysis.actions.iter().all(|action| {
            program
                .callable_cfg(&owner)
                .is_some_and(|cfg| cfg.reachable.contains(&action.location.block))
        }));
    }

    #[test]
    fn multiple_shared_loans_are_distinct_canonical_identities() {
        let file = parse(
            r#"
type Pair { value: i32 }
func read() -> i32 {
    let p = Pair { value: 7 }
    let a = &p.value
    let b = &p.value
    *a + *b
}
func main() -> i32 { read() }
"#,
        );
        let program = crate::core::check_program(&file).expect("shared loans check");
        let analysis = program
            .resource_analysis(&crate::core::NodeId("function:read".into()))
            .expect("read analysis");
        let loans: Vec<_> = analysis
            .loans
            .iter()
            .filter(|loan| loan.kind == LoanKind::Shared && loan.place.display() == "p.value")
            .collect();
        assert_eq!(loans.len(), 2);
        assert_ne!(loans[0].id, loans[1].id);
        assert_eq!(loans[0].reference_name.as_deref(), Some("a"));
        assert_eq!(loans[1].reference_name.as_deref(), Some("b"));
        assert!(analysis.actions.iter().any(|action| {
            action.kind == CanonicalActionKind::BorrowEnd
                && action.loan.as_ref() == Some(&loans[0].id)
        }));
    }

    #[test]
    fn partial_consume_diagnostic_is_emitted_from_reachable_cfg_join() {
        let file = parse(
            r#"
cap Token
func close(flag: bool, token: cap Token) -> i32 {
    if flag { drop(token) }
    0
}
func main() -> i32 { 0 }
"#,
        );
        let errors = crate::core::check_program(&file).expect_err("partial consume must fail");
        assert!(errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::diagnostic::codes::E0304)
                && error.message.contains("reachable CFG paths")
        }));
    }

    #[test]
    fn terminal_predecessors_do_not_pollute_following_join() {
        let file = parse(
            r#"
cap Token
func pass(flag: bool, token: cap Token) -> cap Token {
    if flag { return token }
    return token
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("terminal branch is balanced");
        let owner = crate::core::NodeId("function:pass".into());
        let analysis = program.resource_analysis(&owner).expect("pass analysis");
        assert_eq!(
            analysis
                .actions
                .iter()
                .filter(|action| action.kind == CanonicalActionKind::Return)
                .count(),
            2
        );
    }

    #[test]
    fn mutable_loan_rejects_overlapping_root_read() {
        let file = parse(
            r#"
func read_while_mutably_borrowed() -> i32 {
    let mut value = 1
    let loan = &mut value
    let copied = value
    *loan = 2
    copied
}
func main() -> i32 { 0 }
"#,
        );
        let errors = crate::core::check_program(&file).expect_err("root read must conflict");
        assert!(errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::diagnostic::codes::E0415)
                && error.message == "cannot read 'value' while it is mutably borrowed"
        }));
    }

    #[test]
    fn shared_loan_rejects_overlapping_write() {
        let file = parse(
            r#"
func write_while_shared() -> i32 {
    let mut value = 1
    let loan = &value
    value = 2
    *loan
}
func main() -> i32 { 0 }
"#,
        );
        let errors = crate::core::check_program(&file).expect_err("root write must conflict");
        assert!(errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::diagnostic::codes::E0415)
                && error.message == "cannot write 'value' while it is borrowed"
        }));
    }

    #[test]
    fn disjoint_sibling_accesses_remain_available_during_mutable_loan() {
        let file = parse(
            r#"
type Pair { left: i32, right: i32 }
func update_siblings() -> i32 {
    let mut pair = Pair { left: 1, right: 2 }
    let loan = &mut pair.left
    pair.right = pair.right + 1
    *loan = 4
    pair.right
}
func main() -> i32 { update_siblings() }
"#,
        );
        crate::core::check_program(&file).expect("sibling places do not overlap");
    }

    #[test]
    fn edge_specific_borrow_end_is_stable_and_source_traceable() {
        let file = parse(
            r#"
type Pair { left: i32, right: i32 }
func branch_read(flag: bool) -> i32 {
    let pair = Pair { left: 1, right: 2 }
    let loan = &pair.left
    let selected = if flag { *loan } else { 0 }
    selected + pair.right
}
func main() -> i32 { branch_read(true) }
"#,
        );
        let program = crate::core::check_program(&file).expect("branch loan checks");
        let owner = crate::core::NodeId("function:branch_read".into());
        let analysis = program
            .resource_analysis(&owner)
            .expect("resource analysis");
        let cfg = program.callable_cfg(&owner).expect("callable CFG");
        let edge_end = analysis
            .actions
            .iter()
            .find(|action| {
                action.kind == CanonicalActionKind::BorrowEnd && action.location.edge.is_some()
            })
            .expect("borrow ends on the branch where the reference is dead");
        let edge = edge_end.location.edge.as_ref().expect("edge identity");
        assert!(cfg.edge(edge).is_some());
        assert!(cfg.reachable.contains(&edge_end.location.block));
        assert_ne!(edge_end.span, crate::span::Span::UNKNOWN);
        assert_eq!(edge_end.origin, crate::ast::AstOrigin::User);
    }

    #[test]
    fn loan_live_across_backedge_is_rejected() {
        let file = parse(
            r#"
func loop_read(flag: bool) -> i32 {
    let value = 1
    let loan = &value
    while flag {
        println(*loan)
    }
    value
}
func main() -> i32 { 0 }
"#,
        );
        let errors = crate::core::check_program(&file).expect_err("loop-carried loan must fail");
        assert!(errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::diagnostic::codes::E0415)
                && error.message == "borrow remains live across a loop back-edge"
        }));
    }
}
