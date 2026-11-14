//! Machine-readable technical audit for the V0.2 semantic decisions.
//!
//! The V0.2 planning files intentionally remain design material.  This
//! module is the source-side evidence ledger: it records which part of each
//! D-01..D-14 candidate is executable in the current compiler/runtime and
//! which boundary is still rejected.  It is deliberately independent of the
//! plan status so a plan item cannot become green merely because this catalog
//! exists.
//!
//! Consumers should treat [`DecisionAdmission::NotInTrustedSubset`] and
//! [`DecisionAdmission::FailClosed`] as negative results.  In particular,
//! `Partial` never means that an unsupported source shape is accepted.

use serde::Serialize;

/// Schema identifier for the source-level decision ledger.
pub const V02_DECISION_SCHEMA: &str = "mimi-v02-semantic-decisions-1";

/// Technical state of a decision candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    /// The candidate has executable evidence at its declared boundary.
    Verified,
    /// A bounded implementation is executable, while one or more candidate
    /// surfaces remain outside the trusted language contract.
    Partial,
    /// No trusted implementation exists for the candidate surface.
    Blocked,
    /// The item only narrows product scope; it is not a compiler claim.
    ScopeOnly,
}

/// What a compiler/tooling consumer must do when it reaches the open edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAdmission {
    /// The evidence is sufficient for this decision's declared boundary.
    Accepted,
    /// The source shape is rejected with a stable diagnostic rather than
    /// being silently interpreted by a legacy path.
    FailClosed,
    /// The shape is outside the trusted subset and must not be reported as a
    /// positive semantic fact.
    NotInTrustedSubset,
    /// Product scope metadata only; no implementation admission is implied.
    ScopeOnly,
}

impl DecisionStatus {
    pub const fn is_fully_verified(self) -> bool {
        matches!(self, Self::Verified)
    }
}

impl DecisionAdmission {
    pub const fn is_positive(self) -> bool {
        matches!(self, Self::Accepted)
    }
}

/// One decision record.  Paths are source evidence labels, not claims that a
/// plan or version has been accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DecisionRecord {
    pub id: &'static str,
    pub title: &'static str,
    pub status: DecisionStatus,
    pub admission: DecisionAdmission,
    /// `true` means an unsupported shape has an explicit negative outcome.
    pub fail_closed: bool,
    /// Positive executable evidence already present in the tree.
    pub evidence: &'static [&'static str],
    /// Source tests or probes that can be rerun by a reviewer.
    pub tests: &'static [&'static str],
    /// The current trusted boundary in one sentence.
    pub current_rule: &'static str,
    /// Why the full V0.2 candidate is not yet green, when applicable.
    pub open_edge: &'static str,
    /// Source locations implementing or proving the current rule.
    pub source_refs: &'static [&'static str],
}

/// The complete D-01..D-14 catalog.  Keep this order stable: tools and audit
/// reports use the array order as the canonical decision order.
pub static DECISIONS: &[DecisionRecord] = &[
    DecisionRecord {
        id: "D-01",
        title: "ordinary value copy and hidden cost",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["VM List/String assignment preserves logical isolation"],
        tests: &["src/interp/value.rs clone/share tests", "tests/v02_decision_semantics.rs::d01_value_cow_isolation"],
        current_rule: "ordinary List/String copies isolate logical updates through the VM COW representation",
        open_edge: "no language-level cost receipt or no_alloc admission and native parity proof",
        source_refs: &["src/interp/value.rs", "src/interp/bytecode/builtins/list.rs"],
    },
    DecisionRecord {
        id: "D-02",
        title: "scope cleanup versus explicit strict obligations",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["MutexGuard exactly-once unlock diagnostics"],
        tests: &["src/tests/phase_e.rs::phase_e_mutex_guard_leak_rejected", "src/tests/phase_e.rs::phase_e_mutex_guard_double_unlock_rejected"],
        current_rule: "MutexGuard and protocol resources require an explicit terminal operation",
        open_edge: "automatic scope cleanup for guard/file/buffer resources is not implemented",
        source_refs: &["src/core/cfg/dataflow.rs", "src/tests/phase_e.rs"],
    },
    DecisionRecord {
        id: "D-03",
        title: "Free/linear attributes and generic composition",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["linear T generic boundary and recursive linear payload checks"],
        tests: &["src/tests/linear_kind.rs", "src/core/checker/linear_kind.rs"],
        current_rule: "linear parameters are explicit and unsupported generic resource shapes are rejected",
        open_edge: "Map/Any and the complete copyable/cleanup/cross-task attribute matrix are not closed",
        source_refs: &["src/core/checker.rs", "src/core/checker/linear_kind.rs", "src/core/cfg/resource_lower.rs"],
    },
    DecisionRecord {
        id: "D-04",
        title: "independent Flow owner and lease composition",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::NotInTrustedSubset,
        fail_closed: true,
        evidence: &["runtime FlowDomain routes two/four/eight independent instances", "lease transfer and stale generation checks"],
        tests: &["src/runtime/flow_domain.rs::independent_flow_routes_cover_two_four_and_eight_instances", "src/runtime/flow_domain.rs::cancel_request_ack_transfer_and_settle_has_one_terminal_event"],
        current_rule: "independent owner/lease identities are available through the runtime reference API",
        open_edge: "Mimi source syntax and canonical MIR admission for independent Flow handles are absent",
        source_refs: &["src/runtime/flow_domain.rs", "devdocs/v0.31/architecture-amendment-1.0.md"],
    },
    DecisionRecord {
        id: "D-05",
        title: "module identity, aliases, and qualified access",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["stable module/symbol IDs and interface diff facts", "qualified calls reject with E0830 in the legacy merge surface"],
        tests: &["src/module_system.rs::module_and_symbol_ids_include_package_version_and_path", "tests/real_world_cli.rs::canonical_mir_alias_qualified_import_call_fails_closed_before_consumers"],
        current_rule: "file-level merge remains the production lookup; identity/interface analysis is an explicit side API",
        open_edge: "qualified source lookup and C-export/incremental routing are not installed in the checker",
        source_refs: &["src/module_system.rs", "src/loader/flow.rs", "src/core/infer/call/simple.rs"],
    },
    DecisionRecord {
        id: "D-06",
        title: "finite effect set and public upper bounds",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["finite EffectKind vocabulary", "interprocedural propagation marks dynamic/FFI calls unknown"],
        tests: &["src/core/effects.rs::propagation_reaches_file_and_network_through_recursive_helpers", "src/core/effects.rs::dynamic_and_extern_calls_are_unknown"],
        current_rule: "effect analysis can produce a finite summary and never treats an unknown call as pure",
        open_edge: "parser/checker public effect declarations and all backend routing are not wired end to end",
        source_refs: &["src/core/effects.rs", "src/parser/top_level.rs", "src/core/checker/items.rs"],
    },
    DecisionRecord {
        id: "D-07",
        title: "host capability scope and dynamic sink checks",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["host-owned path/endpoint grants reject traversal and out-of-scope access", "token identity is registry-backed"],
        tests: &["src/runtime/capability_scope.rs::scoped_path_rejects_traversal_and_symlink_escape", "src/runtime/capability_scope.rs::endpoint_scope_matches_host_and_port"],
        current_rule: "the host registry can issue/revoke scoped tokens and check concrete paths/endpoints",
        open_edge: "guarded sink coverage and host-policy injection still need an end-to-end native/VM receipt; token identity alone is not a range proof",
        source_refs: &["src/runtime/capability_scope.rs", "src/interp/bytecode/builtins/fs.rs", "src/codegen/builtins/io.rs"],
    },
    DecisionRecord {
        id: "D-08",
        title: "cancellation, terminal settlement, and cleanup transfer",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::NotInTrustedSubset,
        fail_closed: true,
        evidence: &["FlowDomain separates request/ack/transfer/settle", "unresponsive cancellation remains pending"],
        tests: &["src/runtime/flow_domain.rs::cancel_request_ack_transfer_and_settle_has_one_terminal_event", "src/runtime/flow_domain.rs::unresponsive_cancel_remains_pending_and_fault_is_single_terminal"],
        current_rule: "runtime FlowDomain tickets cannot synthesize a terminal state for an unresponsive participant",
        open_edge: "request_cancel/join/ack/transfer_cleanup are not language-level typed operations",
        source_refs: &["src/runtime/flow_domain.rs", "src/runtime/actor.rs", "src/interp/bytecode/actor.rs"],
    },
    DecisionRecord {
        id: "D-09",
        title: "multi-Flow scope versus Grouped Flow and Zig",
        status: DecisionStatus::ScopeOnly,
        admission: DecisionAdmission::ScopeOnly,
        fail_closed: false,
        evidence: &["product scope is recorded as an independent multi-Flow requirement"],
        tests: &["src/runtime/flow_domain.rs::independent_flow_routes_cover_two_four_and_eight_instances"],
        current_rule: "Grouped Flow and Zig promotion are separate product-scope questions",
        open_edge: "no language implementation is claimed by this scope record",
        source_refs: &["devdocs/V0.2/05-待裁决问题.md"],
    },
    DecisionRecord {
        id: "D-10",
        title: "static and measured no_alloc cost constraints",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["closed cost graph rejects reachable allocation/callback/destructor paths", "runtime cost region counts allocation and no_alloc violations"],
        tests: &["src/cost.rs::no_alloc_rejects_callback_and_destructor_allocation_with_reason_chain", "src/runtime/cost.rs::no_alloc_guard_counts_allocations_and_unknown_events"],
        current_rule: "standalone cost APIs classify certain/possible/measured/unknown events and reject unknown paths",
        open_edge: "no_alloc is not a source declaration and the checker/codegen does not yet install the gate for every program path",
        source_refs: &["src/cost.rs", "src/runtime/cost.rs", "src/runtime/mod.rs", "src/query.rs"],
    },
    DecisionRecord {
        id: "D-11",
        title: "ordinary persistent data and Actor fields",
        status: DecisionStatus::Blocked,
        admission: DecisionAdmission::NotInTrustedSubset,
        fail_closed: true,
        evidence: &["mutable Actor fields are rejected with E0402 under the current Flow-only rule"],
        tests: &["src/core/checker/items.rs actor mutable-field diagnostic", "docs/language-spec.md persistent-state rule"],
        current_rule: "business state that changes over time must live in a Flow; mutable Actor fields are rejected",
        open_edge: "the candidate ordinary-owned-data rule and native Actor runs-Flow implementation are not complete",
        source_refs: &["src/core/checker/items.rs", "docs/language-spec.md"],
    },
    DecisionRecord {
        id: "D-12",
        title: "executable versus statically provable contracts",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["eight VerifStatus outcomes are explicit", "ProofArtifact carries source/engine/cache identity"],
        tests: &["src/verifier/tests.rs status/proof artifact tests", "src/verifier/ctx.rs::VerifStatus"],
        current_rule: "runtime, trusted-subset, solver-unknown, timeout, infrastructure, and no-obligation outcomes remain distinct",
        open_edge: "full FFI/simple-bool and all call-site routes do not yet have one proven VM/native result contract",
        source_refs: &["src/verifier/ctx.rs", "src/main/verify.rs", "src/verifier/tests.rs"],
    },
    DecisionRecord {
        id: "D-13",
        title: "mutate places, projections, and overlap",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["ownership Place models field/tuple/index/deref conflicts", "public mutate grammar rejects nested/index shapes with E0434"],
        tests: &["src/core/ownership.rs::conflicts_with projection tests", "src/tests/narrow.rs sync mutate tests"],
        current_rule: "only a variable or one-level field is admitted at the public mutate boundary",
        open_edge: "nested and index write-back/evaluation-once semantics are not installed in the checker/codegen",
        source_refs: &["src/core/ownership.rs", "src/core/infer/call/simple.rs", "src/tests/narrow.rs"],
    },
    DecisionRecord {
        id: "D-14",
        title: "cross-task ownership, visibility, and thread affinity",
        status: DecisionStatus::Partial,
        admission: DecisionAdmission::FailClosed,
        fail_closed: true,
        evidence: &["view/mutate/ref task-boundary checks reject unsupported transfers", "runtime atomics use SeqCst implementation ordering"],
        tests: &["src/tests/narrow.rs cross-task view/mutate/ref rejection tests", "src/runtime/concurrency.rs thread-affinity checks"],
        current_rule: "only owned values cross the existing narrow task boundary; runtime ordering is not a language proof",
        open_edge: "MutexGuard can currently cross spawn unchecked and explicit sync/deadlock contracts are incomplete",
        source_refs: &["src/core/checker/narrow.rs", "src/runtime/concurrency.rs", "src/runtime/future.rs"],
    },
];

/// Return the immutable canonical catalog.
pub const fn records() -> &'static [DecisionRecord] {
    DECISIONS
}

/// Find one decision without parsing user text or consulting plan state.
pub fn by_id(id: &str) -> Option<&'static DecisionRecord> {
    DECISIONS.iter().find(|record| record.id == id)
}

#[derive(Debug, Serialize)]
struct DecisionReport {
    schema: &'static str,
    decision_count: usize,
    decisions: &'static [DecisionRecord],
}

/// Serialize a stable machine-readable report for audit tooling.
pub fn report_json() -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&DecisionReport {
        schema: V02_DECISION_SCHEMA,
        decision_count: DECISIONS.len(),
        decisions: DECISIONS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_exactly_the_fourteen_v02_decisions() {
        let ids = records().iter().map(|record| record.id).collect::<Vec<_>>();
        assert_eq!(
            ids,
            (1..=14)
                .map(|index| format!("D-{index:02}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(records().len(), 14);
    }

    #[test]
    fn every_non_positive_record_has_an_explicit_negative_boundary() {
        for record in records() {
            if !record.status.is_fully_verified() {
                assert!(
                    !record.admission.is_positive(),
                    "{} admits an unverified candidate",
                    record.id
                );
                assert!(
                    record.fail_closed || matches!(record.admission, DecisionAdmission::ScopeOnly),
                    "{} lacks a fail-closed or scope-only boundary",
                    record.id
                );
            }
            assert!(!record.current_rule.is_empty());
            assert!(!record.open_edge.is_empty());
            assert!(!record.source_refs.is_empty());
            assert!(!record.tests.is_empty());
        }
    }

    #[test]
    fn report_is_machine_readable_and_does_not_include_plan_state() {
        let json = report_json().expect("decision report serializes");
        assert!(json.contains(V02_DECISION_SCHEMA));
        assert!(json.contains("\"decision_count\": 14"));
        assert!(json.contains("\"D-01\""));
        assert!(json.contains("\"D-14\""));
        assert!(!json.contains("plan_status"));
    }

    #[test]
    fn lookup_is_stable_and_unknown_ids_are_not_guessed() {
        assert_eq!(
            by_id("D-04").map(|record| record.title),
            Some("independent Flow owner and lease composition")
        );
        assert!(by_id("D-00").is_none());
        assert!(by_id("D-15").is_none());
    }
}
