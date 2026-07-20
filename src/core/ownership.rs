use std::collections::BTreeMap;

use crate::span::Span;

use super::cfg::{BasicBlockId, EdgeId};
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
    Field(String),
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
                (PlaceProjection::Field(left), PlaceProjection::Field(right)) => left == right,
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
                PlaceProjection::Field(field) => {
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
}
