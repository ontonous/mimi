//! Executable source-side evidence for the V0.2 D-01..D-14 decision ledger.
//!
//! These tests intentionally exercise the small APIs that are already
//! trusted.  They do not turn a partial runtime API into a language-level
//! acceptance: the catalog assertions keep the open edge and admission mode
//! visible in the same test binary.

use mimi::core::effects::{builtin_effects, EffectKind};
use mimi::cost::{analyze_no_alloc, CostEvent, CostGraph, CostNode};
use mimi::interp::Value;
use mimi::runtime::capability_scope::{check_path, forget, issue_path, PathAccess};
use mimi::runtime::flow_domain::{FlowDomainError, FlowDomainOwner, FlowTerminal};
use mimi::v02_decisions::{by_id, report_json, DecisionAdmission, DecisionStatus};

#[test]
fn d01_value_cow_isolation() {
    let original = Value::List(std::sync::Arc::new(vec![Value::Int(1)]));
    let mut copied = original.clone();
    let Value::List(values) = &mut copied else {
        panic!("list value");
    };
    std::sync::Arc::make_mut(values)[0] = Value::Int(9);
    let Value::List(values) = original else {
        panic!("list value");
    };
    assert_eq!(values[0], Value::Int(1));
    assert_eq!(
        by_id("D-01").map(|record| record.status),
        Some(DecisionStatus::Partial)
    );
}

#[test]
fn catalog_is_machine_readable_and_never_promotes_partial_candidates() {
    let report = report_json().expect("decision report must serialize");
    let value: serde_json::Value = serde_json::from_str(&report).expect("valid JSON report");
    assert_eq!(value["schema"], "mimi-v02-semantic-decisions-1");
    assert_eq!(value["decision_count"], 14);
    let records = value["decisions"].as_array().expect("decision array");
    assert_eq!(records.len(), 14);
    for record in records {
        if record["status"] != "verified" {
            assert_ne!(record["admission"], "accepted");
        }
    }
    assert_eq!(
        by_id("D-11").map(|record| record.status),
        Some(DecisionStatus::Blocked)
    );
    assert_eq!(
        by_id("D-09").map(|record| record.admission),
        Some(DecisionAdmission::ScopeOnly)
    );
}

#[test]
fn d04_independent_flow_owner_lease_and_terminal_settlement_are_executable() {
    let mut owner = FlowDomainOwner::new().expect("owner");
    let participant = owner.participant();
    let first = participant.open_flow(11).expect("first flow");
    let second = participant.open_flow(22).expect("second flow");
    assert_ne!(first, second);
    let first_lease = participant.acquire_lease(first).expect("lease");
    assert_eq!(first_lease.complete(12), Ok(FlowTerminal::Completed(12)));
    let second_lease = participant.acquire_lease(second).expect("lease");
    assert_eq!(second_lease.complete(23), Ok(FlowTerminal::Completed(23)));
    owner.close().expect("all independent flows settled");
    assert_eq!(
        by_id("D-04").map(|record| record.status),
        Some(DecisionStatus::Partial)
    );
}

#[test]
fn d06_effect_builtins_have_finite_facets_and_unknown_names_do_not_become_pure() {
    let fetch = builtin_effects("fetch");
    assert!(fetch.contains(EffectKind::Network));
    assert!(fetch.contains(EffectKind::Blocking));
    assert!(builtin_effects("not_a_builtin").is_empty());
    assert_eq!(
        by_id("D-06").map(|record| record.admission),
        Some(DecisionAdmission::FailClosed)
    );
}

#[test]
fn d07_scoped_capability_rejects_out_of_root_paths() {
    let root = std::env::temp_dir().join(format!("mimi_v02_scope_{}", std::process::id()));
    let outside =
        std::env::temp_dir().join(format!("mimi_v02_scope_outside_{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("scope root");
    std::fs::create_dir_all(&outside).expect("outside root");
    let token = issue_path(&root).expect("scoped token");
    assert!(check_path(token, root.join("inside.txt"), PathAccess::Read).is_ok());
    assert!(check_path(token, outside.join("secret.txt"), PathAccess::Read).is_err());
    forget(token);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
    assert_eq!(
        by_id("D-07").map(|record| record.status),
        Some(DecisionStatus::Partial)
    );
}

#[test]
fn d08_unresponsive_cancel_is_pending_and_cannot_be_forged_as_terminal() {
    let mut owner = FlowDomainOwner::new().expect("owner");
    let participant = owner.participant();
    let flow = participant.open_flow(7).expect("flow");
    let _ticket = participant.request_cancel(flow).expect("cancel request");
    assert_eq!(owner.close(), Err(FlowDomainError::UnresolvedCancellation));
    assert_eq!(
        by_id("D-08").map(|record| record.admission),
        Some(DecisionAdmission::NotInTrustedSubset)
    );
}

#[test]
fn d10_no_alloc_graph_rejects_reachable_allocation() {
    let mut graph = CostGraph::default();
    graph.insert("main", CostNode::default().call("helper"));
    graph.insert(
        "helper",
        CostNode::with_event(CostEvent::allocation("helper:grow")),
    );
    let error = analyze_no_alloc(&graph, "main").expect_err("allocation must reject no_alloc");
    assert!(error.to_string().contains("no_alloc rejected"));
    assert_eq!(
        by_id("D-10").map(|record| record.admission),
        Some(DecisionAdmission::FailClosed)
    );
}
