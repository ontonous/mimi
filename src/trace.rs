//! v0.31.15: Canonical Semantic Trace — `mimi-semantic-trace-1` profile.
//!
//! Records execution events (Flow transitions, Fault entries, session
//! operations, actor spawns) in a canonical form suitable for cross-backend
//! equivalence comparison. Addresses, thread IDs, wall-clock time, allocator
//! addresses and random hashes are excluded from the comparison key.

use std::fmt;

/// Canonical trace event kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TraceEventKind {
    /// Flow transition executed: (flow, event, from_state) → to_state.
    Transition,
    /// Flow entered Fault state.
    Fault,
    /// Flow recovered from Fault.
    Recover,
    /// Session send operation.
    SessionSend,
    /// Session recv operation.
    SessionRecv,
    /// Session close operation.
    SessionClose,
    /// Actor spawned.
    ActorSpawn,
    /// Actor message dispatched.
    ActorMessage,
    /// Resource introduced.
    ResourceIntroduce,
    /// Resource moved.
    ResourceMove,
    /// Resource dropped.
    ResourceDrop,
    /// Resource returned.
    ResourceReturn,
    /// 0.31.15 追加 A: Flow state ownership transferred (variable → transition
    /// or variable → alias). Records the exact moment a generation is invalidated.
    OwnershipTransfer,
    /// 0.31.15 追加 A: linear violation detected at runtime (use-after-move
    /// safety net). Records the diagnostic path even though the operation is
    /// rejected.
    LinearViolation,
}

/// A single canonical trace event.
///
/// Fields follow `docs/spec/semantic-trace.md` §1. Comparison-excluded
/// fields (addresses, thread IDs, wall-clock) are not stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEvent {
    /// Monotonically increasing event ID within the trace.
    pub event_id: u64,
    /// Event that caused this one (e.g., spawn → child's first event).
    pub parent_event_id: Option<u64>,
    /// Canonical logical actor identity (not OS thread).
    pub logical_actor: String,
    /// Lamport-style logical clock.
    pub logical_clock: u64,
    /// What happened.
    pub kind: TraceEventKind,
    /// Flow instance name (e.g., "Counter").
    pub flow_instance: Option<String>,
    /// Generation counter before the event.
    pub generation_before: Option<u64>,
    /// Generation counter after the event.
    pub generation_after: Option<u64>,
    /// State name before the event (e.g., "Zero").
    pub state_before: Option<String>,
    /// Event/transition name (e.g., "inc").
    pub event_name: Option<String>,
    /// State name after the event (e.g., "Positive").
    pub state_after: Option<String>,
    /// Result or fault description.
    pub result_or_fault: Option<String>,
    /// Session residual before (e.g., "!i32 . end").
    pub session_before: Option<String>,
    /// Session residual after (e.g., "end").
    pub session_after: Option<String>,
    /// Source span (relative to package root).
    pub source_span: Option<String>,
}

/// Collects trace events during interpreter execution.
#[derive(Debug, Default)]
pub struct TraceCollector {
    events: Vec<TraceEvent>,
    next_event_id: u64,
    logical_clock: u64,
    enabled: bool,
}

impl TraceCollector {
    /// Create a disabled collector (zero overhead when not tracing).
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            next_event_id: 0,
            logical_clock: 0,
            enabled: false,
        }
    }

    /// Enable trace collection.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Whether collection is active.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record a Flow transition event.
    pub fn record_transition(
        &mut self,
        flow: &str,
        event: &str,
        from_state: &str,
        to_state: &str,
        generation: u64,
    ) {
        if !self.enabled {
            return;
        }
        self.logical_clock += 1;
        let id = self.next_event_id;
        self.next_event_id += 1;
        self.events.push(TraceEvent {
            event_id: id,
            parent_event_id: None,
            logical_actor: "main".to_string(),
            logical_clock: self.logical_clock,
            kind: TraceEventKind::Transition,
            flow_instance: Some(flow.to_string()),
            generation_before: Some(generation),
            generation_after: Some(generation + 1),
            state_before: Some(from_state.to_string()),
            event_name: Some(event.to_string()),
            state_after: Some(to_state.to_string()),
            result_or_fault: None,
            session_before: None,
            session_after: None,
            source_span: None,
        });
    }

    /// Record a Fault entry event.
    pub fn record_fault(&mut self, flow: &str, from_state: &str, reason: &str) {
        if !self.enabled {
            return;
        }
        self.logical_clock += 1;
        let id = self.next_event_id;
        self.next_event_id += 1;
        self.events.push(TraceEvent {
            event_id: id,
            parent_event_id: None,
            logical_actor: "main".to_string(),
            logical_clock: self.logical_clock,
            kind: TraceEventKind::Fault,
            flow_instance: Some(flow.to_string()),
            generation_before: None,
            generation_after: None,
            state_before: Some(from_state.to_string()),
            event_name: None,
            state_after: Some("Fault".to_string()),
            result_or_fault: Some(reason.to_string()),
            session_before: None,
            session_after: None,
            source_span: None,
        });
    }

    /// Record a session operation event.
    pub fn record_session(
        &mut self,
        kind: TraceEventKind,
        endpoint: &str,
        residual_before: &str,
        residual_after: &str,
    ) {
        if !self.enabled {
            return;
        }
        self.logical_clock += 1;
        let id = self.next_event_id;
        self.next_event_id += 1;
        self.events.push(TraceEvent {
            event_id: id,
            parent_event_id: None,
            logical_actor: "main".to_string(),
            logical_clock: self.logical_clock,
            kind,
            flow_instance: None,
            generation_before: None,
            generation_after: None,
            state_before: None,
            event_name: Some(endpoint.to_string()),
            state_after: None,
            result_or_fault: None,
            session_before: Some(residual_before.to_string()),
            session_after: Some(residual_after.to_string()),
            source_span: None,
        });
    }

    /// 0.31.15 追加 A: record a flow state ownership transfer.
    /// Captures the exact moment a generation is invalidated: the source
    /// variable is consumed and ownership moves to the transition result
    /// or alias target.
    pub fn record_ownership_transfer(
        &mut self,
        flow: &str,
        from_var: &str,
        to_var: &str,
        generation: u64,
        state: &str,
    ) {
        if !self.enabled {
            return;
        }
        self.logical_clock += 1;
        let id = self.next_event_id;
        self.next_event_id += 1;
        self.events.push(TraceEvent {
            event_id: id,
            parent_event_id: None,
            logical_actor: "main".to_string(),
            logical_clock: self.logical_clock,
            kind: TraceEventKind::OwnershipTransfer,
            flow_instance: Some(flow.to_string()),
            generation_before: Some(generation),
            generation_after: Some(generation + 1),
            state_before: Some(state.to_string()),
            event_name: Some(format!("{} -> {}", from_var, to_var)),
            state_after: Some(state.to_string()),
            result_or_fault: None,
            session_before: None,
            session_after: None,
            source_span: None,
        });
    }

    /// 0.31.15 追加 A: record a linear violation detected at runtime.
    /// The use-after-move safety net in the interpreter triggers this
    /// event, making the violation visible in the trace even though the
    /// operation is rejected.
    pub fn record_linear_violation(
        &mut self,
        flow: &str,
        var: &str,
        state: &str,
        reason: &str,
    ) {
        if !self.enabled {
            return;
        }
        self.logical_clock += 1;
        let id = self.next_event_id;
        self.next_event_id += 1;
        self.events.push(TraceEvent {
            event_id: id,
            parent_event_id: None,
            logical_actor: "main".to_string(),
            logical_clock: self.logical_clock,
            kind: TraceEventKind::LinearViolation,
            flow_instance: Some(flow.to_string()),
            generation_before: None,
            generation_after: None,
            state_before: Some(state.to_string()),
            event_name: Some(var.to_string()),
            state_after: None,
            result_or_fault: Some(reason.to_string()),
            session_before: None,
            session_after: None,
            source_span: None,
        });
    }

    /// Get all collected events.
    pub fn events(&self) -> &[TraceEvent] {
        &self.events
    }

    /// Take all collected events, leaving the collector empty.
    pub fn take_events(&mut self) -> Vec<TraceEvent> {
        std::mem::take(&mut self.events)
    }

    /// Number of collected events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether no events have been collected.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Compare two traces for canonical equivalence.
///
/// Deterministic programs: event sequences must be element-wise equal
/// (ignoring event_id and logical_clock, which are ordering artifacts).
///
/// Returns `Ok(())` if equivalent, or a description of the first difference.
pub fn compare_traces(a: &[TraceEvent], b: &[TraceEvent]) -> Result<(), String> {
    if a.len() != b.len() {
        return Err(format!(
            "trace length mismatch: {} vs {} events",
            a.len(),
            b.len()
        ));
    }
    for (i, (ea, eb)) in a.iter().zip(b.iter()).enumerate() {
        if ea.kind != eb.kind {
            return Err(format!(
                "event {} kind mismatch: {:?} vs {:?}",
                i, ea.kind, eb.kind
            ));
        }
        if ea.flow_instance != eb.flow_instance {
            return Err(format!(
                "event {} flow_instance mismatch: {:?} vs {:?}",
                i, ea.flow_instance, eb.flow_instance
            ));
        }
        if ea.state_before != eb.state_before {
            return Err(format!(
                "event {} state_before mismatch: {:?} vs {:?}",
                i, ea.state_before, eb.state_before
            ));
        }
        if ea.state_after != eb.state_after {
            return Err(format!(
                "event {} state_after mismatch: {:?} vs {:?}",
                i, ea.state_after, eb.state_after
            ));
        }
        if ea.event_name != eb.event_name {
            return Err(format!(
                "event {} event_name mismatch: {:?} vs {:?}",
                i, ea.event_name, eb.event_name
            ));
        }
        if ea.result_or_fault != eb.result_or_fault {
            return Err(format!(
                "event {} result_or_fault mismatch: {:?} vs {:?}",
                i, ea.result_or_fault, eb.result_or_fault
            ));
        }
        // 0.31.15 追加 A: generation counters must match for ownership
        // transfer and transition events. This ensures the happens-before
        // DAG includes consistent generation edges.
        if ea.generation_before != eb.generation_before {
            return Err(format!(
                "event {} generation_before mismatch: {:?} vs {:?}",
                i, ea.generation_before, eb.generation_before
            ));
        }
        if ea.generation_after != eb.generation_after {
            return Err(format!(
                "event {} generation_after mismatch: {:?} vs {:?}",
                i, ea.generation_after, eb.generation_after
            ));
        }
    }
    Ok(())
}

impl fmt::Display for TraceEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:?}]", self.kind)?;
        if let Some(ref flow) = self.flow_instance {
            write!(f, " {}", flow)?;
        }
        if let Some(ref before) = self.state_before {
            write!(f, " {}", before)?;
        }
        if let Some(ref event) = self.event_name {
            write!(f, "::{}", event)?;
        }
        if let Some(ref after) = self.state_after {
            write!(f, " -> {}", after)?;
        }
        if let Some(ref result) = self.result_or_fault {
            write!(f, " ({})", result)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_collector_records_transitions() {
        let mut collector = TraceCollector::new();
        collector.enable();
        collector.record_transition("Counter", "inc", "Zero", "Positive", 0);
        collector.record_transition("Counter", "bump", "Positive", "Positive", 1);
        assert_eq!(collector.len(), 2);
        let events = collector.events();
        assert_eq!(events[0].kind, TraceEventKind::Transition);
        assert_eq!(events[0].state_before.as_deref(), Some("Zero"));
        assert_eq!(events[0].state_after.as_deref(), Some("Positive"));
        assert_eq!(events[1].state_before.as_deref(), Some("Positive"));
    }

    #[test]
    fn trace_collector_disabled_is_noop() {
        let mut collector = TraceCollector::new();
        // Not enabled — should be no-op.
        collector.record_transition("Counter", "inc", "Zero", "Positive", 0);
        assert!(collector.is_empty());
    }

    #[test]
    fn compare_identical_traces_ok() {
        let mut a = TraceCollector::new();
        a.enable();
        a.record_transition("F", "e", "A", "B", 0);
        let mut b = TraceCollector::new();
        b.enable();
        b.record_transition("F", "e", "A", "B", 0);
        assert!(compare_traces(a.events(), b.events()).is_ok());
    }

    #[test]
    fn compare_divergent_traces_err() {
        let mut a = TraceCollector::new();
        a.enable();
        a.record_transition("F", "e", "A", "B", 0);
        let mut b = TraceCollector::new();
        b.enable();
        b.record_transition("F", "e", "A", "C", 0);
        let result = compare_traces(a.events(), b.events());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("state_after"));
    }

    #[test]
    fn compare_length_mismatch() {
        let mut a = TraceCollector::new();
        a.enable();
        a.record_transition("F", "e", "A", "B", 0);
        let b = TraceCollector::new();
        let result = compare_traces(a.events(), b.events());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("length"));
    }

    #[test]
    fn ownership_transfer_recorded() {
        let mut c = TraceCollector::new();
        c.enable();
        c.record_ownership_transfer("Counter", "s0", "s1", 0, "Zero");
        assert_eq!(c.len(), 1);
        let e = &c.events()[0];
        assert_eq!(e.kind, TraceEventKind::OwnershipTransfer);
        assert_eq!(e.generation_before, Some(0));
        assert_eq!(e.generation_after, Some(1));
        assert_eq!(e.event_name.as_deref(), Some("s0 -> s1"));
    }

    #[test]
    fn linear_violation_recorded() {
        let mut c = TraceCollector::new();
        c.enable();
        c.record_linear_violation("Counter", "s0", "Zero", "use-after-move");
        assert_eq!(c.len(), 1);
        let e = &c.events()[0];
        assert_eq!(e.kind, TraceEventKind::LinearViolation);
        assert_eq!(e.result_or_fault.as_deref(), Some("use-after-move"));
    }

    #[test]
    fn compare_generation_mismatch() {
        let mut a = TraceCollector::new();
        a.enable();
        a.record_transition("F", "e", "A", "B", 0);
        let mut b = TraceCollector::new();
        b.enable();
        b.record_transition("F", "e", "A", "B", 5);
        let result = compare_traces(a.events(), b.events());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("generation_before"));
    }
}
