use std::collections::BTreeMap;

use crate::span::Span;

use super::cfg::{BasicBlockId, CallableCfg, EdgeId};
use super::{NodeId, Origin};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub NodeId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceId(pub NodeId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LoanId(pub NodeId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IndexProjection {
    Constant(i64),
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlaceProjection {
    Field { field: NodeId, name: String },
    Tuple(usize),
    Index(IndexProjection),
    Deref,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Place {
    pub base: LocalId,
    pub base_name: String,
    pub projections: Vec<PlaceProjection>,
}

impl Place {
    pub fn root(base: LocalId, base_name: impl Into<String>) -> Self {
        Self {
            base,
            base_name: base_name.into(),
            projections: Vec::new(),
        }
    }

    pub fn conflicts_with(&self, other: &Self) -> bool {
        if self.projections.contains(&PlaceProjection::Deref)
            || other.projections.contains(&PlaceProjection::Deref)
        {
            return true;
        }
        if self.base != other.base {
            return false;
        }
        for (left, right) in self.projections.iter().zip(other.projections.iter()) {
            if left == right {
                continue;
            }
            return match (left, right) {
                (
                    PlaceProjection::Field { field: left, .. },
                    PlaceProjection::Field { field: right, .. },
                ) => left == right,
                (PlaceProjection::Tuple(left), PlaceProjection::Tuple(right)) => left == right,
                (
                    PlaceProjection::Index(IndexProjection::Constant(left)),
                    PlaceProjection::Index(IndexProjection::Constant(right)),
                ) => left == right,
                (PlaceProjection::Index(_), PlaceProjection::Index(_)) => true,
                _ => true,
            };
        }
        // Equal paths and root/prefix relationships overlap.
        true
    }

    pub fn display(&self) -> String {
        let mut value = self.base_name.clone();
        for projection in &self.projections {
            match projection {
                PlaceProjection::Field { name: field, .. } => {
                    value.push('.');
                    value.push_str(field);
                }
                PlaceProjection::Tuple(index) => {
                    value.push('.');
                    value.push_str(&index.to_string());
                }
                PlaceProjection::Index(IndexProjection::Constant(index)) => {
                    value.push('[');
                    value.push_str(&index.to_string());
                    value.push(']');
                }
                PlaceProjection::Index(IndexProjection::Dynamic) => value.push_str("[*]"),
                PlaceProjection::Deref => value.insert(0, '*'),
            }
        }
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoanKind {
    Shared,
    Mutable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgLocation {
    pub block: BasicBlockId,
    pub point: NodeId,
    pub edge: Option<EdgeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonicalActionKind {
    Read,
    Write,
    Introduce,
    Move,
    Drop,
    Return,
    TransferSession,
    TransferChild,
    DelegateConsume,
    BorrowShared,
    BorrowMut,
    BorrowEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalResourceAction {
    pub kind: CanonicalActionKind,
    pub resource: ResourceId,
    pub source: Option<Place>,
    pub target: Option<Place>,
    pub loan: Option<LoanId>,
    pub location: CfgLocation,
    pub span: Span,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    Consumed,
    MaybeConsumed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceFact {
    pub availability: Availability,
    pub owner: Option<Place>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loan {
    pub id: LoanId,
    pub parent: Option<LoanId>,
    pub kind: LoanKind,
    pub place: Place,
    pub reference: Option<LocalId>,
    pub reference_name: Option<String>,
    pub start: CfgLocation,
    pub end_edges: Vec<EdgeId>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceAnalysis {
    pub owner: NodeId,
    pub actions: Vec<CanonicalResourceAction>,
    pub loans: Vec<Loan>,
    pub in_states: BTreeMap<BasicBlockId, BTreeMap<ResourceId, ResourceFact>>,
    pub out_states: BTreeMap<BasicBlockId, BTreeMap<ResourceId, ResourceFact>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceState {
    Available,
    Consumed,
    MaybeConsumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceActionKind {
    Introduce,
    Move,
    Drop,
    Return,
    BorrowShared,
    BorrowMut,
    BorrowEnd,
    TransferSession,
    TransferChild,
    DelegateConsume,
}

impl ResourceActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Introduce => "introduce",
            Self::Move => "move",
            Self::Drop => "drop",
            Self::Return => "return",
            Self::BorrowShared => "borrow_shared",
            Self::BorrowMut => "borrow_mut",
            Self::BorrowEnd => "borrow_end",
            Self::TransferSession => "transfer_session",
            Self::TransferChild => "transfer_child",
            Self::DelegateConsume => "delegate_consume",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceAction {
    pub kind: ResourceActionKind,
    pub resource: String,
    pub control_path: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMerge {
    pub resource: String,
    pub then_state: ResourceState,
    pub else_state: ResourceState,
    pub merged_state: ResourceState,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipLedger {
    pub owner: NodeId,
    pub actions: Vec<ResourceAction>,
    pub branch_merges: Vec<BranchMerge>,
}

impl OwnershipLedger {
    #[cfg(test)]
    pub(crate) fn new(owner: NodeId) -> Self {
        Self {
            owner,
            actions: Vec::new(),
            branch_merges: Vec::new(),
        }
    }

    pub fn action_count(&self, kind: ResourceActionKind) -> usize {
        self.actions
            .iter()
            .filter(|action| action.kind == kind)
            .count()
    }

    pub fn resources(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .actions
            .iter()
            .map(|action| action.resource.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn has_maybe_consumed_merge(&self) -> bool {
        self.branch_merges
            .iter()
            .any(|merge| merge.merged_state == ResourceState::MaybeConsumed)
    }

    /// Compatibility projection for consumers not yet migrated to canonical
    /// `ResourceAnalysis`. It is never an analysis input.
    pub(crate) fn from_analysis(analysis: &ResourceAnalysis, cfg: &CallableCfg) -> Self {
        let actions = analysis
            .actions
            .iter()
            .filter_map(|action| {
                let kind = match action.kind {
                    CanonicalActionKind::Read | CanonicalActionKind::Write => return None,
                    CanonicalActionKind::Introduce => ResourceActionKind::Introduce,
                    CanonicalActionKind::Move => ResourceActionKind::Move,
                    CanonicalActionKind::Drop => ResourceActionKind::Drop,
                    CanonicalActionKind::Return => ResourceActionKind::Return,
                    CanonicalActionKind::TransferSession => ResourceActionKind::TransferSession,
                    CanonicalActionKind::TransferChild => ResourceActionKind::TransferChild,
                    CanonicalActionKind::DelegateConsume => ResourceActionKind::DelegateConsume,
                    CanonicalActionKind::BorrowShared => ResourceActionKind::BorrowShared,
                    CanonicalActionKind::BorrowMut => ResourceActionKind::BorrowMut,
                    CanonicalActionKind::BorrowEnd => ResourceActionKind::BorrowEnd,
                };
                Some(ResourceAction {
                    kind,
                    resource: action
                        .source
                        .as_ref()
                        .or(action.target.as_ref())
                        .map(Place::display)
                        .unwrap_or_else(|| action.resource.0 .0.clone()),
                    control_path: Vec::new(),
                    span: action.span,
                })
            })
            .collect();

        let mut branch_merges = Vec::new();
        for (block, incoming) in &analysis.in_states {
            let predecessors = cfg
                .predecessors(block)
                .into_iter()
                .filter(|edge| cfg.reachable.contains(&edge.from))
                .filter_map(|edge| analysis.out_states.get(&edge.from))
                .collect::<Vec<_>>();
            if predecessors.len() < 2 {
                continue;
            }
            let resources = predecessors
                .iter()
                .flat_map(|state| state.keys().cloned())
                .collect::<std::collections::BTreeSet<_>>();
            for resource in resources {
                let state = |fact: Option<&ResourceFact>| {
                    fact.map(|fact| match fact.availability {
                        Availability::Available => ResourceState::Available,
                        Availability::Consumed => ResourceState::Consumed,
                        Availability::MaybeConsumed => ResourceState::MaybeConsumed,
                    })
                    .unwrap_or(ResourceState::MaybeConsumed)
                };
                let name = incoming
                    .get(&resource)
                    .and_then(|fact| fact.owner.as_ref())
                    .or_else(|| {
                        predecessors.iter().find_map(|facts| {
                            facts.get(&resource).and_then(|fact| fact.owner.as_ref())
                        })
                    })
                    .map(Place::display)
                    .unwrap_or_else(|| {
                        analysis
                            .actions
                            .iter()
                            .find(|action| action.resource == resource)
                            .and_then(|action| action.source.as_ref())
                            .map(Place::display)
                            .unwrap_or_else(|| resource.0 .0.clone())
                    });
                branch_merges.push(BranchMerge {
                    resource: name,
                    then_state: state(predecessors[0].get(&resource)),
                    else_state: state(predecessors[1].get(&resource)),
                    merged_state: state(incoming.get(&resource)),
                    span: cfg
                        .block(block)
                        .map(|block| block.source.span)
                        .unwrap_or(Span::UNKNOWN),
                });
            }
        }
        branch_merges.sort_by(|left, right| {
            (left.span.start_line, left.span.start_col, &left.resource).cmp(&(
                right.span.start_line,
                right.span.start_col,
                &right.resource,
            ))
        });
        Self {
            owner: analysis.owner.clone(),
            actions,
            branch_merges,
        }
    }
}
