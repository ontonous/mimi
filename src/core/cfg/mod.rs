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
    /// 0.39.136 perf: adjacency indexes built once at construction.
    /// The `successors`/`predecessors` accessors previously scanned ALL
    /// edges per call (O(E)), which made liveness dataflow effectively
    /// cubic on large CFGs (20k-arm matches took minutes).
    pub successor_index: BTreeMap<BasicBlockId, Vec<EdgeId>>,
    pub predecessor_index: BTreeMap<BasicBlockId, Vec<EdgeId>>,
}

impl CallableCfg {
    pub fn block(&self, id: &BasicBlockId) -> Option<&BasicBlock> {
        self.blocks.get(id)
    }

    pub fn edge(&self, id: &EdgeId) -> Option<&CfgEdge> {
        self.edges.get(id)
    }

    pub fn predecessors(&self, block: &BasicBlockId) -> Vec<&CfgEdge> {
        self.adjacent(block, &self.predecessor_index)
    }

    pub fn successors(&self, block: &BasicBlockId) -> Vec<&CfgEdge> {
        self.adjacent(block, &self.successor_index)
    }

    /// O(out-degree) edge lookup through the prebuilt index.
    fn adjacent<'a>(
        &'a self,
        block: &BasicBlockId,
        index: &'a BTreeMap<BasicBlockId, Vec<EdgeId>>,
    ) -> Vec<&'a CfgEdge> {
        index
            .get(block)
            .map(|ids| ids.iter().filter_map(|id| self.edges.get(id)).collect())
            .unwrap_or_default()
    }

    /// Build adjacency indexes from the edge map. Call once after all edges
    /// are inserted (both CFG constructors route through this).
    pub fn with_adjacency_indexes(mut self) -> Self {
        let mut successor_index: BTreeMap<BasicBlockId, Vec<EdgeId>> = BTreeMap::new();
        let mut predecessor_index: BTreeMap<BasicBlockId, Vec<EdgeId>> = BTreeMap::new();
        for (id, edge) in &self.edges {
            successor_index
                .entry(edge.from.clone())
                .or_default()
                .push(id.clone());
            predecessor_index
                .entry(edge.to.clone())
                .or_default()
                .push(id.clone());
        }
        self.successor_index = successor_index;
        self.predecessor_index = predecessor_index;
        self
    }

    pub fn validate(&self) -> Result<(), Vec<Diagnostic>> {
        validate_cfg(self)
    }
}

#[cfg(test)]
mod tests;
