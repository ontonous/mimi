//! Stable per-callable control-flow graphs used by ownership and permission analysis.
//!
//! CFG identities are semantic `NodeId`s. They never expose insertion-order or
//! vector indexes, so declaration reordering cannot silently retarget a fact.

mod dataflow;
#[cfg(test)]
mod lower;
mod resolved_lower;
mod resource_lower;
mod validate;

use std::collections::{BTreeMap, BTreeSet};

use crate::diagnostic::Diagnostic;
use crate::span::Span;

use super::{NodeId, Origin, Place};

#[cfg(test)]
pub use dataflow::analyze_cfgs;
#[cfg(test)]
pub use lower::lower_file;
pub use resolved_lower::lower_resolved_bodies;
pub use resource_lower::analyze_resolved_bodies;
pub use validate::validate_cfg;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BasicBlockId(pub NodeId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(pub NodeId);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgSource {
    pub node: NodeId,
    pub span: Span,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfgPointKind {
    Statement,
    Expression,
    Condition,
    Binding,
    Assignment,
    ResourceAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgPoint {
    pub source: CfgSource,
    pub kind: CfgPointKind,
    /// Root binding names used by backwards liveness.
    pub uses: Vec<String>,
    /// Root binding names defined by this point.
    pub defs: Vec<String>,
    /// Stable structured place spellings read at this point.
    pub reads: Vec<String>,
    /// Stable structured place spellings written at this point.
    pub writes: Vec<String>,
    /// Canonical typed places read at this point. Production resource analysis
    /// consumes these identities; `reads` remains a compatibility display.
    pub read_places: Vec<Place>,
    /// Canonical typed places written at this point.
    pub write_places: Vec<Place>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EdgeKind {
    Fallthrough,
    Then,
    Else,
    MatchArm,
    LoopBody,
    LoopExit,
    Backedge,
    Break,
    Continue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgEdge {
    pub id: EdgeId,
    pub from: BasicBlockId,
    pub to: BasicBlockId,
    pub kind: EdgeKind,
    pub source: CfgSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Goto {
        edge: EdgeId,
    },
    Branch {
        condition: NodeId,
        then_edge: EdgeId,
        else_edge: EdgeId,
    },
    Match {
        scrutinee: NodeId,
        arms: Vec<EdgeId>,
    },
    Return {
        value: Option<NodeId>,
        implicit: bool,
    },
    Break {
        edge: EdgeId,
    },
    Continue {
        edge: EdgeId,
    },
    Diverge,
    Unreachable,
}

impl Terminator {
    pub fn outgoing_edges(&self) -> Vec<&EdgeId> {
        match self {
            Self::Goto { edge } | Self::Break { edge } | Self::Continue { edge } => vec![edge],
            Self::Branch {
                then_edge,
                else_edge,
                ..
            } => vec![then_edge, else_edge],
            Self::Match { arms, .. } => arms.iter().collect(),
            Self::Return { .. } | Self::Diverge | Self::Unreachable => Vec::new(),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Return { .. } | Self::Diverge | Self::Unreachable
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    pub id: BasicBlockId,
    pub source: CfgSource,
    pub points: Vec<CfgPoint>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableCfg {
    pub owner: NodeId,
    pub entry: BasicBlockId,
    pub blocks: BTreeMap<BasicBlockId, BasicBlock>,
    pub edges: BTreeMap<EdgeId, CfgEdge>,
    pub reachable: BTreeSet<BasicBlockId>,
}

impl CallableCfg {
    pub fn block(&self, id: &BasicBlockId) -> Option<&BasicBlock> {
        self.blocks.get(id)
    }

    pub fn edge(&self, id: &EdgeId) -> Option<&CfgEdge> {
        self.edges.get(id)
    }

    pub fn predecessors(&self, block: &BasicBlockId) -> Vec<&CfgEdge> {
        self.edges
            .values()
            .filter(|edge| &edge.to == block)
            .collect()
    }

    pub fn successors(&self, block: &BasicBlockId) -> Vec<&CfgEdge> {
        self.edges
            .values()
            .filter(|edge| &edge.from == block)
            .collect()
    }

    pub fn validate(&self) -> Result<(), Vec<Diagnostic>> {
        validate_cfg(self)
    }
}

#[cfg(test)]
mod tests;
