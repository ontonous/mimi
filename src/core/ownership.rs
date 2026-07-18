use crate::span::Span;

use super::NodeId;

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
