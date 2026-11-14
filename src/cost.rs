//! Static cost-event summaries and the fail-closed `no_alloc` gate.
//!
//! This module is intentionally independent from the effect checker.  Effects
//! describe what an operation may do; cost events describe implementation
//! obligations such as heap allocation, COW and container growth.  A caller
//! can therefore carry both summaries without one silently standing in for the
//! other.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Versioned schema for cost reports.
pub const COST_SCHEMA_VERSION: u32 = 1;

/// Certainty level used in human and machine reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostCertainty {
    Certain,
    Possible,
    Measured,
    Unknown,
}

impl fmt::Display for CostCertainty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Certain => "certain",
            Self::Possible => "possible",
            Self::Measured => "measured",
            Self::Unknown => "unknown",
        })
    }
}

/// Program-visible cost operations.  `Release` is included in reports but is
/// not itself an allocation violation; deep copy/COW/resize are marked as
/// allocation-capable because they can materialize storage on a reachable path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostEventKind {
    Allocate,
    Release,
    DeepCopy,
    Cow,
    Resize,
    DeviceTransfer,
    Call,
    Unknown,
}

impl fmt::Display for CostEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Allocate => "allocate",
            Self::Release => "release",
            Self::DeepCopy => "deep_copy",
            Self::Cow => "cow",
            Self::Resize => "resize",
            Self::DeviceTransfer => "device_transfer",
            Self::Call => "call",
            Self::Unknown => "unknown",
        })
    }
}

impl CostEventKind {
    fn may_allocate(self) -> bool {
        matches!(
            self,
            Self::Allocate
                | Self::DeepCopy
                | Self::Cow
                | Self::Resize
                | Self::DeviceTransfer
                | Self::Unknown
        )
    }
}

/// One event with its source identity and reason text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEvent {
    pub kind: CostEventKind,
    pub certainty: CostCertainty,
    pub source: String,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl CostEvent {
    pub fn new(kind: CostEventKind, certainty: CostCertainty, source: impl Into<String>) -> Self {
        Self {
            kind,
            certainty,
            source: source.into(),
            bytes: None,
            reason: None,
        }
    }

    pub fn allocation(source: impl Into<String>) -> Self {
        Self::new(CostEventKind::Allocate, CostCertainty::Certain, source)
    }

    pub fn possible_allocation(source: impl Into<String>, reason: impl Into<String>) -> Self {
        let mut event = Self::new(CostEventKind::Allocate, CostCertainty::Possible, source);
        event.reason = Some(reason.into());
        event
    }

    pub fn release(source: impl Into<String>) -> Self {
        Self::new(CostEventKind::Release, CostCertainty::Certain, source)
    }

    pub fn unknown(source: impl Into<String>, reason: impl Into<String>) -> Self {
        let mut event = Self::new(CostEventKind::Unknown, CostCertainty::Unknown, source);
        event.reason = Some(reason.into());
        event
    }
}

/// Static events attributed to one callable/region.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostNode {
    pub events: Vec<CostEvent>,
    #[serde(default)]
    pub calls: Vec<String>,
    #[serde(default)]
    pub callbacks: Vec<String>,
    #[serde(default)]
    pub destructors: Vec<String>,
    #[serde(default)]
    pub cancellation: Vec<String>,
    /// A dynamic call has no finite callee set.  It is unknown for `no_alloc`.
    #[serde(default)]
    pub dynamic_call: bool,
}

impl CostNode {
    pub fn with_event(event: CostEvent) -> Self {
        Self {
            events: vec![event],
            ..Self::default()
        }
    }

    pub fn call(mut self, callee: impl Into<String>) -> Self {
        self.calls.push(callee.into());
        self
    }

    pub fn callback(mut self, callback: impl Into<String>) -> Self {
        self.callbacks.push(callback.into());
        self
    }

    pub fn destructor(mut self, destructor: impl Into<String>) -> Self {
        self.destructors.push(destructor.into());
        self
    }

    pub fn cancellation(mut self, path: impl Into<String>) -> Self {
        self.cancellation.push(path.into());
        self
    }
}

/// A static callable graph.  IDs should be canonical MIR callable IDs when
/// available; source names are accepted by standalone tools and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostGraph {
    pub nodes: BTreeMap<String, CostNode>,
}

impl CostGraph {
    pub fn insert(&mut self, id: impl Into<String>, node: CostNode) {
        self.nodes.insert(id.into(), node);
    }

    pub fn node(&self, id: &str) -> Option<&CostNode> {
        self.nodes.get(id)
    }
}

/// A source edge in a no-alloc reason chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostReason {
    pub from: String,
    pub to: Option<String>,
    pub kind: CostEventKind,
    pub source: String,
    pub message: String,
}

impl fmt::Display for CostReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.to {
            Some(to) => write!(
                f,
                "{} -> {}: {} ({})",
                self.from, to, self.message, self.source
            ),
            None => write!(f, "{}: {} ({})", self.from, self.message, self.source),
        }
    }
}

/// Failure from a `no_alloc` check.  Every rejection carries the complete
/// caller/callback/destructor/cancellation chain that made the allocation
/// reachable, or an explicit unknown boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoAllocViolation {
    pub root: String,
    pub reasons: Vec<CostReason>,
}

impl fmt::Display for NoAllocViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no_alloc rejected for {}", self.root)?;
        for reason in &self.reasons {
            write!(f, "\n  {reason}")?;
        }
        Ok(())
    }
}

/// Static report returned for both accepted and rejected regions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostReport {
    pub schema_version: u32,
    pub root: String,
    pub certain: Vec<CostEvent>,
    pub possible: Vec<CostEvent>,
    pub measured: Vec<CostEvent>,
    pub unknown: Vec<CostEvent>,
    pub reasons: Vec<CostReason>,
    pub no_alloc: bool,
}

impl CostReport {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn human(&self) -> String {
        let mut out = format!("cost report {} (schema {})", self.root, self.schema_version);
        for (label, events) in [
            ("certain", &self.certain),
            ("possible", &self.possible),
            ("measured", &self.measured),
            ("unknown", &self.unknown),
        ] {
            if !events.is_empty() {
                out.push_str(&format!("\n{label}:"));
                for event in events {
                    out.push_str(&format!("\n  {} @ {}", event.kind, event.source));
                }
            }
        }
        out.push_str(&format!("\nno_alloc: {}", self.no_alloc));
        for reason in &self.reasons {
            out.push_str(&format!("\nreason: {reason}"));
        }
        out
    }
}

/// Analyze a callable graph and enforce no-alloc over all reachable paths.
pub fn analyze_no_alloc(graph: &CostGraph, root: &str) -> Result<CostReport, NoAllocViolation> {
    let mut report = CostReport {
        schema_version: COST_SCHEMA_VERSION,
        root: root.to_owned(),
        certain: Vec::new(),
        possible: Vec::new(),
        measured: Vec::new(),
        unknown: Vec::new(),
        reasons: Vec::new(),
        no_alloc: true,
    };
    let mut visited = BTreeSet::new();
    let mut reasons = Vec::new();
    visit_node(graph, root, root, &mut visited, &mut report, &mut reasons);
    report.reasons = reasons.clone();
    if report.no_alloc {
        Ok(report)
    } else {
        Err(NoAllocViolation {
            root: root.to_owned(),
            reasons,
        })
    }
}

fn visit_node(
    graph: &CostGraph,
    root: &str,
    id: &str,
    visited: &mut BTreeSet<String>,
    report: &mut CostReport,
    reasons: &mut Vec<CostReason>,
) {
    if !visited.insert(id.to_owned()) {
        return;
    }
    let Some(node) = graph.node(id) else {
        report.no_alloc = false;
        reasons.push(CostReason {
            from: root.to_owned(),
            to: Some(id.to_owned()),
            kind: CostEventKind::Unknown,
            source: id.to_owned(),
            message: "unknown callable boundary".into(),
        });
        return;
    };

    for event in &node.events {
        match event.certainty {
            CostCertainty::Certain => report.certain.push(event.clone()),
            CostCertainty::Possible => report.possible.push(event.clone()),
            CostCertainty::Measured => report.measured.push(event.clone()),
            CostCertainty::Unknown => report.unknown.push(event.clone()),
        }
        if event.kind.may_allocate() {
            report.no_alloc = false;
            reasons.push(CostReason {
                from: id.to_owned(),
                to: None,
                kind: event.kind,
                source: event.source.clone(),
                message: event
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("{} event is reachable", event.kind)),
            });
        }
    }
    if node.dynamic_call {
        report.no_alloc = false;
        reasons.push(CostReason {
            from: id.to_owned(),
            to: None,
            kind: CostEventKind::Unknown,
            source: id.to_owned(),
            message: "dynamic dispatch has no closed cost summary".into(),
        });
    }

    let mut edges = Vec::new();
    edges.extend(node.calls.iter().cloned());
    edges.extend(node.callbacks.iter().cloned());
    edges.extend(node.destructors.iter().cloned());
    edges.extend(node.cancellation.iter().cloned());
    for callee in edges {
        if !graph.nodes.contains_key(&callee) {
            report.no_alloc = false;
            reasons.push(CostReason {
                from: id.to_owned(),
                to: Some(callee.clone()),
                kind: CostEventKind::Unknown,
                source: callee.clone(),
                message: "reachable call/callback path has no cost summary".into(),
            });
        } else {
            visit_node(graph, root, &callee, visited, report, reasons);
        }
    }
}

/// Shorthand for a node with no events and no outgoing calls.
pub fn no_alloc_node() -> CostNode {
    CostNode::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_alloc_accepts_closed_release_only_graph() {
        let mut graph = CostGraph::default();
        graph.insert(
            "main",
            CostNode {
                events: vec![CostEvent::release("main:cleanup")],
                ..CostNode::default()
            },
        );
        let report = analyze_no_alloc(&graph, "main").expect("release does not allocate");
        assert!(report.no_alloc);
        assert_eq!(report.certain.len(), 1);
    }

    #[test]
    fn no_alloc_rejects_callback_and_destructor_allocation_with_reason_chain() {
        let mut graph = CostGraph::default();
        graph.insert(
            "main",
            CostNode::default()
                .callback("callback")
                .destructor("drop_buffer"),
        );
        graph.insert(
            "callback",
            CostNode::with_event(CostEvent::possible_allocation(
                "callback:format",
                "formatting may grow its output",
            )),
        );
        graph.insert(
            "drop_buffer",
            CostNode::with_event(CostEvent::allocation("drop_buffer:free_list")),
        );
        let error = analyze_no_alloc(&graph, "main").expect_err("allocation path must reject");
        assert!(error.reasons.iter().any(|reason| reason.from == "callback"));
        assert!(error
            .reasons
            .iter()
            .any(|reason| reason.from == "drop_buffer"));
        assert!(error.to_string().contains("no_alloc rejected"));
    }

    #[test]
    fn unknown_calls_fail_closed() {
        let mut graph = CostGraph::default();
        graph.insert("main", CostNode::default().call("missing"));
        let error = analyze_no_alloc(&graph, "main").expect_err("unknown callee");
        assert!(error
            .reasons
            .iter()
            .any(|reason| reason.kind == CostEventKind::Unknown));
    }

    #[test]
    fn no_alloc_rejects_device_transfer_as_allocation_capable() {
        let mut graph = CostGraph::default();
        graph.insert(
            "main",
            CostNode::with_event(CostEvent::new(
                CostEventKind::DeviceTransfer,
                CostCertainty::Certain,
                "main:upload",
            )),
        );
        let error = analyze_no_alloc(&graph, "main").expect_err("device transfer must be gated");
        assert!(error
            .reasons
            .iter()
            .any(|reason| reason.kind == CostEventKind::DeviceTransfer));
    }
}
