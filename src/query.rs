//! Checker/MIR backed semantic queries.
//!
//! This module is deliberately a small, read-only facade over the semantic
//! directories that the checker already builds.  It is shared by the CLI and
//! LSP so those two front ends cannot slowly grow different answers.  Query
//! responses carry a versioned envelope and an explicit trust/status pair;
//! callers must not turn an unavailable MIR or an unknown dynamic boundary
//! into a positive fact.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{json, Value};

use crate::core::ir::{ResolvedBlock, ResolvedCallee, ResolvedExpr, ResolvedExprKind};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::MirInstructionKind;
use crate::core::resolved::ResolvedTransition;
use crate::core::{
    Availability, CanonicalActionKind, CheckedProgram, NodeId, Origin, ResolvedCallKind,
    ResolvedItemKind, ResourceAnalysis,
};
use crate::diagnostic::{Diagnostic, Severity};
use crate::span::Span;

/// Version of the machine-facing query envelope.  A consumer should reject a
/// newer major schema rather than guessing at fields.
pub const QUERY_SCHEMA_VERSION: &str = "mimi-query-1";
/// Query service semantic version.  This is separate from the package version
/// because a package rebuild with unchanged query semantics remains cacheable.
pub const QUERY_SEMANTICS_VERSION: &str = "mimi-query-semantics-1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryTrust {
    StaticChecked,
    ConditionalUpperBound,
    Measured,
    Experimental,
    Unsupported,
    CacheInvalidated,
}

impl QueryTrust {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaticChecked => "static_checked",
            Self::ConditionalUpperBound => "conditional_upper_bound",
            Self::Measured => "measured",
            Self::Experimental => "experimental",
            Self::Unsupported => "unsupported",
            Self::CacheInvalidated => "cache_invalidated",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryStatus {
    Checked,
    Unknown,
    Experimental,
    Unsupported,
    CacheInvalidated,
}

impl QueryStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Checked => "checked",
            Self::Unknown => "unknown",
            Self::Experimental => "experimental",
            Self::Unsupported => "unsupported",
            Self::CacheInvalidated => "cache_invalidated",
        }
    }
}

/// Read-only query view.  The checked program is always required.  MIR is
/// optional because some source programs are checker-valid but outside the
/// currently materialised MIR islands; those queries report the boundary
/// explicitly instead of re-deriving facts from an emitter.
pub struct QueryService<'a> {
    checked: &'a CheckedProgram,
    mir: Option<&'a MirProgram>,
    source: Option<String>,
}

impl<'a> QueryService<'a> {
    pub fn new(checked: &'a CheckedProgram) -> Self {
        Self {
            checked,
            mir: None,
            source: None,
        }
    }

    pub fn with_mir(checked: &'a CheckedProgram, mir: &'a MirProgram) -> Self {
        Self {
            checked,
            mir: Some(mir),
            source: None,
        }
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn checked(&self) -> &'a CheckedProgram {
        self.checked
    }

    pub fn mir(&self) -> Option<&'a MirProgram> {
        self.mir
    }

    /// Query one declaration and its direct references/callers.
    pub fn symbol(&self, name: &str) -> Value {
        let definitions = self.symbol_definitions(name);
        let ids = definitions
            .iter()
            .filter_map(|item| item.get("id").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        let references = self.call_sites_for_symbol(name, &ids);
        let reference_count = references.len();
        self.envelope(
            "symbol",
            name,
            if definitions.is_empty() {
                QueryTrust::Unsupported
            } else {
                QueryTrust::StaticChecked
            },
            if definitions.is_empty() {
                QueryStatus::Unknown
            } else {
                QueryStatus::Checked
            },
            json!({
                "definitions": definitions,
                "references": references,
                "definition_count": ids.len(),
                "reference_count": reference_count,
            }),
        )
    }

    /// Definitions and all checker call sites that resolve to a declaration.
    pub fn references(&self, name: &str) -> Value {
        let definitions = self.symbol_definitions(name);
        let ids = definitions
            .iter()
            .filter_map(|item| item.get("id").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        let references = self.call_sites_for_symbol(name, &ids);
        self.envelope(
            "references",
            name,
            if definitions.is_empty() {
                QueryTrust::Unsupported
            } else {
                QueryTrust::StaticChecked
            },
            if definitions.is_empty() {
                QueryStatus::Unknown
            } else {
                QueryStatus::Checked
            },
            json!({
                "references": references,
                "callers": sorted_unique_strings(
                    references.iter().filter_map(|r| r.get("caller").and_then(Value::as_str))
                ),
            }),
        )
    }

    /// Complete direct call graph, or the outgoing calls for one owner.
    pub fn calls(&self, owner: Option<&str>) -> Value {
        let owner_id = owner.and_then(|name| self.resolve_owner(name));
        let mut calls = self
            .checked
            .call_sites_sorted()
            .into_iter()
            .filter(|site| owner_id.as_ref().is_none_or(|id| site.owner == id.0))
            .map(|site| self.call_site_value(site))
            .collect::<Vec<_>>();
        calls.sort_by(|a, b| {
            a.get("id")
                .and_then(Value::as_str)
                .cmp(&b.get("id").and_then(Value::as_str))
        });
        let status = if owner.is_some() && owner_id.is_none() {
            QueryStatus::Unknown
        } else {
            QueryStatus::Checked
        };
        self.envelope(
            "calls",
            owner.unwrap_or("<program>"),
            if status == QueryStatus::Checked {
                QueryTrust::StaticChecked
            } else {
                QueryTrust::Unsupported
            },
            status,
            json!({
                "calls": calls,
                "call_count": calls.len(),
                "owner": owner,
            }),
        )
    }

    /// Flow declaration and its legal transition edges.  If event/source are
    /// supplied only that edge is returned, while the enclosing flow remains
    /// in the response for stable navigation.
    pub fn flow(&self, flow_name: &str, event: Option<&str>, source: Option<&str>) -> Value {
        let Some(flow) = self.checked.flow(flow_name) else {
            return self.envelope(
                "flow",
                flow_name,
                QueryTrust::Unsupported,
                QueryStatus::Unknown,
                json!({ "reason": "flow is not present in the checked directory" }),
            );
        };
        let mut edges = self
            .checked
            .transitions()
            .values()
            .filter(|transition| transition.id.flow == flow.id)
            .filter(|transition| event.is_none_or(|wanted| transition.id.event == wanted))
            .filter(|transition| source.is_none_or(|wanted| transition.id.source.name == wanted))
            .map(|transition| self.transition_value(transition))
            .collect::<Vec<_>>();
        edges.sort_by(|a, b| {
            (
                a.get("event").and_then(Value::as_str),
                a.get("source").and_then(Value::as_str),
            )
                .cmp(&(
                    b.get("event").and_then(Value::as_str),
                    b.get("source").and_then(Value::as_str),
                ))
        });
        let edge_count = edges.len();
        let mut states = flow
            .states
            .values()
            .map(|state| {
                json!({
                    "id": state.node_id.0,
                    "name": state.id.name,
                    "span": span_value(state.origin.user_span()),
                    "payload": state.payload.iter().map(|(name, ty)| json!({
                        "name": name,
                        "type": crate::core::fmt_type(ty),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        states.sort_by(|a, b| {
            a.get("id")
                .and_then(Value::as_str)
                .cmp(&b.get("id").and_then(Value::as_str))
        });
        self.envelope(
            "flow",
            flow_name,
            QueryTrust::StaticChecked,
            QueryStatus::Checked,
            json!({
                "id": flow.node_id.0,
                "name": flow.id.0,
                "states": states,
                "edges": edges,
                "edge_count": edge_count,
                "persistent_fields": flow.persistent_fields,
            }),
        )
    }

    /// Checker resource ledger for one callable.  Terminal out-state facts
    /// are the source of truth for obligations; action counts are included as
    /// a trace that explains how each obligation was introduced/settled.
    pub fn ownership(&self, function: &str) -> Value {
        let Some(owner) = self.resolve_owner(function) else {
            return self.envelope(
                "ownership",
                function,
                QueryTrust::Unsupported,
                QueryStatus::Unknown,
                json!({ "reason": "callable is not present in the checked directory" }),
            );
        };
        let Some(analysis) = self.checked.resource_analysis(&owner) else {
            return self.envelope(
                "ownership",
                function,
                QueryTrust::Unsupported,
                QueryStatus::Unknown,
                json!({ "reason": "checked callable has no resource analysis" }),
            );
        };
        let actions = analysis
            .actions
            .iter()
            .map(|action| {
                json!({
                    "kind": action_kind(action.kind),
                    "resource": action.resource.0.0.clone(),
                    "source": action.source.as_ref().map(|p| p.display().to_string()),
                    "target": action.target.as_ref().map(|p| p.display().to_string()),
                    "loan": action.loan.as_ref().map(|id| id.0.0.clone()),
                    "point": action.location.point.0,
                    "span": span_value(action.span),
                })
            })
            .collect::<Vec<_>>();
        let loans = analysis
            .loans
            .iter()
            .map(|loan| {
                json!({
                    "id": loan.id.0.0.clone(),
                    "kind": format!("{:?}", loan.kind).to_lowercase(),
                    "place": loan.place.display(),
                    "reference": loan.reference_name,
                    "start": span_value(loan.span),
                    "end_edges": loan.end_edges.iter().map(|edge| edge.0.0.clone()).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        let mut obligations = Vec::new();
        let cfg = self.checked.callable_cfg(&owner);
        for (block, states) in &analysis.out_states {
            let terminal = cfg
                .and_then(|cfg| cfg.blocks.get(block))
                .is_some_and(|block| block.terminator.is_terminal());
            if !terminal {
                continue;
            }
            for (resource, fact) in states {
                if matches!(
                    fact.availability,
                    Availability::Available | Availability::MaybeConsumed
                ) {
                    obligations.push(json!({
                        "resource": resource.0.0.clone(),
                        "availability": format!("{:?}", fact.availability).to_lowercase(),
                        "owner": fact.owner.as_ref().map(|p| p.display().to_string()),
                        "introduced_span": fact.introduced_span.map(span_value),
                        "block": block.0.0.clone(),
                    }));
                }
            }
        }
        obligations.sort_by_key(|a| a.to_string());
        let mir_events = self.mir.and_then(|mir| {
            mir.functions().get(&owner).map(|function| {
                function
                    .ownership
                    .events
                    .iter()
                    .map(|event| {
                        json!({
                            "kind": event.kind.as_str(),
                            "resource": event.resource,
                            "value": event.value.as_ref().map(|value| value.as_str().to_owned()),
                            "source": event.source,
                            "target": event.target,
                            "point": event.point.0,
                            "loan": event.loan.as_ref().map(|id| id.0.clone()),
                        })
                    })
                    .collect::<Vec<_>>()
            })
        });
        self.envelope(
            "ownership",
            function,
            QueryTrust::StaticChecked,
            QueryStatus::Checked,
            json!({
                "owner": owner.0,
                "actions": actions,
                "action_counts": action_counts(analysis),
                "loans": loans,
                "unsettled_obligations": obligations,
                "mir_events": mir_events,
            }),
        )
    }

    /// Effect summary and the exact checker expression/call origins carrying
    /// each effect.  An empty source vector is reported as `unknown_may_read_write`
    /// for typed call receipts rather than being advertised as a no-effect
    /// proof.
    pub fn effects(&self, function: &str) -> Value {
        let Some(owner) = self.resolve_owner(function) else {
            return self.envelope(
                "effects",
                function,
                QueryTrust::Unsupported,
                QueryStatus::Unknown,
                json!({ "reason": "callable is not present in the checked directory" }),
            );
        };
        let Some(declaration) = self.checked.functions().get(&owner).or_else(|| {
            self.checked
                .functions()
                .values()
                .find(|f| f.node_id == owner)
        }) else {
            return self.envelope(
                "effects",
                function,
                QueryTrust::Unsupported,
                QueryStatus::Unknown,
                json!({ "reason": "callable declaration is not present" }),
            );
        };
        let mut entries = Vec::new();
        let mut body_effects = BTreeSet::new();
        if let Some(body) = self.checked.resolved_body(&owner) {
            collect_effects_block(&body.root, &mut entries, &mut body_effects);
        }
        let declared = self
            .checked
            .resolved_signature(&owner)
            .map(|signature| {
                signature
                    .effects
                    .iter()
                    .map(|effect| effect.as_str().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| declaration.effects.clone());
        let mut all = declared.iter().cloned().collect::<BTreeSet<_>>();
        all.extend(body_effects);
        self.envelope(
            "effects",
            function,
            QueryTrust::StaticChecked,
            QueryStatus::Checked,
            json!({
                "owner": owner.0,
                "declared": declared,
                "inferred": all.into_iter().collect::<Vec<_>>(),
                "sources": entries,
            }),
        )
    }

    /// Cost explanation from canonical MIR instructions and ownership events.
    /// Explicit allocations/releases are `must`; calls and capacity-dependent
    /// operations are `may`.  No runtime measurement is invented here.
    pub fn cost(&self, function: &str) -> Value {
        let Some(owner) = self.resolve_owner(function) else {
            return self.envelope(
                "cost",
                function,
                QueryTrust::Unsupported,
                QueryStatus::Unknown,
                json!({ "reason": "callable is not present in the checked directory" }),
            );
        };
        let Some(mir) = self.mir else {
            return self.envelope(
                "cost",
                function,
                QueryTrust::Unsupported,
                QueryStatus::Unsupported,
                json!({
                    "reason": "canonical MIR was not materialized for this checked program",
                    "static_fallback": self
                        .checked
                        .resource_analysis(&owner)
                        .map(action_counts)
                        .unwrap_or_else(|| json!({})),
                }),
            );
        };
        let Some(mir_function) = mir.functions().get(&owner) else {
            return self.envelope(
                "cost",
                function,
                QueryTrust::Unsupported,
                QueryStatus::Unknown,
                json!({ "reason": "callable is outside the materialized MIR graph" }),
            );
        };
        let mut events = Vec::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for block in mir_function.blocks.values() {
            for instruction in &block.instructions {
                let (kind, certainty, reason) = cost_classification(&instruction.kind);
                if let Some(kind) = kind {
                    *counts.entry(kind.to_string()).or_default() += 1;
                    events.push(json!({
                        "kind": kind,
                        "certainty": certainty,
                        "reason": reason,
                        "instruction": instruction.id.as_str(),
                        "point": instruction.id.as_str(),
                        "span": self.point_span(
                            &owner,
                            &NodeId(instruction.id.as_str().to_owned()),
                        ),
                    }));
                }
            }
        }
        self.envelope(
            "cost",
            function,
            QueryTrust::ConditionalUpperBound,
            QueryStatus::Checked,
            json!({
                "owner": owner.0,
                "route": "canonical-mir",
                "counts": counts,
                "events": events,
                "measured": [],
                "measurement_note": "No runtime measurement was supplied for this query.",
            }),
        )
    }

    /// Conservative static impact graph.  The graph is complete for checked
    /// direct call sites; unknown/extern/dynamic boundaries are surfaced in a
    /// separate field instead of claiming an exact impact set.
    pub fn impact(&self, symbol: &str) -> Value {
        let definitions = self.symbol_definitions(symbol);
        let ids = definitions
            .iter()
            .filter_map(|item| item.get("id").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        if ids.is_empty() {
            return self.envelope(
                "impact",
                symbol,
                QueryTrust::Unsupported,
                QueryStatus::Unknown,
                json!({ "reason": "changed symbol is absent from the checked directory" }),
            );
        }
        let mut direct = BTreeSet::new();
        let mut unknown = BTreeSet::new();
        for site in self.checked.call_sites().values() {
            if call_site_targets(site, symbol, &ids) {
                direct.insert(site.owner.clone());
            }
            if matches!(
                site.kind,
                ResolvedCallKind::Unknown | ResolvedCallKind::Extern
            ) && ids.contains(&site.owner)
            {
                unknown.insert(site.callee.clone());
            }
        }
        // Reverse transitive closure over the checked direct graph.
        // The edited declarations themselves need recompilation too.  Starting
        // only from their callers loses the owner body (and also makes the
        // transitive walk depend on whether it has callers).
        let mut affected = ids.clone();
        affected.extend(direct);
        let mut queue = affected.iter().cloned().collect::<VecDeque<_>>();
        while let Some(callee) = queue.pop_front() {
            for site in self.checked.call_sites().values() {
                if self.call_site_targets_owner(site, &callee) {
                    if affected.insert(site.owner.clone()) {
                        queue.push_back(site.owner.clone());
                    }
                }
            }
        }
        for site in self.checked.call_sites().values() {
            if matches!(
                site.kind,
                ResolvedCallKind::Unknown | ResolvedCallKind::Extern
            ) && affected.contains(&site.owner)
            {
                unknown.insert(site.callee.clone());
            }
        }
        let mut tests = BTreeSet::new();
        let mut behavior = BTreeSet::new();
        for owner in &affected {
            if owner
                .rsplit("::")
                .next()
                .is_some_and(|name| name.starts_with("test_"))
            {
                tests.insert(owner.clone());
            } else {
                behavior.insert(owner.clone());
            }
        }
        let public = definitions
            .iter()
            .any(|item| item.get("public").and_then(Value::as_bool) == Some(true));
        self.envelope(
            "impact",
            symbol,
            QueryTrust::ConditionalUpperBound,
            QueryStatus::Checked,
            json!({
                "changed": symbol,
                "api": {
                    "public": public,
                    "signature_or_contract": public,
                    "reason": if public { "public declaration or contract may change" } else { "private declaration; public API summary is unchanged" },
                },
                "recompile": sorted_strings(affected.iter()),
                "behavior": sorted_strings(behavior.iter()),
                "tests": sorted_strings(tests.iter()),
                "unknown_boundaries": sorted_strings(unknown.iter()),
                "conservative": true,
                "reasons": [
                    "checked direct call sites are included",
                    "transitive callers are included",
                    "dynamic/extern boundaries are reported as unknown",
                ],
            }),
        )
    }

    /// Q-06 research surface.  Checked programs are closed, so there are no
    /// type holes to silently bless; callers get an explicit experimental
    /// boundary and a shape for future hole facts.
    pub fn holes(&self) -> Value {
        self.envelope(
            "holes",
            "<program>",
            QueryTrust::Experimental,
            QueryStatus::Experimental,
            json!({
                "complete": false,
                "holes": [],
                "reason": "the checked-program contract intentionally rejects residual type holes; hole inference is research-only",
                "build_allowed": false,
                "verify_allowed": false,
                "future_fields": ["expected_type", "held_resources", "allowed_effects", "unfinished_obligations"],
            }),
        )
    }

    /// Normalize compiler diagnostics into the same versioned query envelope.
    pub fn diagnostics(&self, diagnostics: &[Diagnostic]) -> Value {
        diagnostics_response(diagnostics, self.source.as_deref())
    }

    fn envelope(
        &self,
        query: &str,
        identity: &str,
        trust: QueryTrust,
        status: QueryStatus,
        data: Value,
    ) -> Value {
        let (route, mir_digest) = self
            .mir
            .map(|mir| {
                let receipt = mir.route_receipt(format!("query-{query}"));
                ("canonical-mir", Some(receipt.mir_digest))
            })
            .unwrap_or(("resolved", None));
        let cache_key = format!(
            "{}:{}:{}:{}:{}",
            QUERY_SCHEMA_VERSION,
            QUERY_SEMANTICS_VERSION,
            route,
            mir_digest.as_deref().unwrap_or("checked"),
            identity
        );
        let cache = json!({
            "key": cache_key,
            "valid": status != QueryStatus::CacheInvalidated,
            "invalidation": "source edits, checker diagnostics, compiler version, or MIR route changes invalidate this result",
        });
        let metadata = json!({
            "schema_version": QUERY_SCHEMA_VERSION,
            "semantics_version": QUERY_SEMANTICS_VERSION,
            "compiler_version": env!("CARGO_PKG_VERSION"),
            "resolved_ir_version": crate::core::RESOLVED_IR_VERSION,
            "route": route,
            "mir_digest": mir_digest,
            "trust": trust.as_str(),
            "status": status.as_str(),
            "source": self.source,
            "identity": identity,
            "cache": cache.clone(),
        });
        json!({
            "schema_version": QUERY_SCHEMA_VERSION,
            "semantics_version": QUERY_SEMANTICS_VERSION,
            "compiler_version": env!("CARGO_PKG_VERSION"),
            "resolved_ir_version": crate::core::RESOLVED_IR_VERSION,
            "route": route,
            "mir_digest": mir_digest,
            "trust": trust.as_str(),
            "status": status.as_str(),
            "query": query,
            "identity": identity,
            "cache": cache,
            "metadata": metadata,
            "data": data,
        })
    }

    fn symbol_definitions(&self, name: &str) -> Vec<Value> {
        let mut definitions = Vec::new();
        for item in self.checked.items().values() {
            if !name_matches(&item.qualified_name, name, &item.node_id.0) {
                continue;
            }
            let public = match item.kind {
                ResolvedItemKind::Function => self
                    .checked
                    .functions()
                    .get(&item.node_id)
                    .is_some_and(|function| function.pub_),
                _ => false,
            };
            definitions.push(self.declaration_value(
                &item.node_id,
                &item.qualified_name,
                item_kind(item.kind),
                item.origin.clone(),
                public,
            ));
        }
        // Constructor/method declarations may be represented only in the
        // callable directory.  Include those identities when they match.
        for function in self.checked.functions().values() {
            if name_matches(&function.qualified_name, name, &function.node_id.0)
                && !definitions.iter().any(|item| {
                    item.get("id").and_then(Value::as_str) == Some(function.node_id.0.as_str())
                })
            {
                definitions.push(self.declaration_value(
                    &function.node_id,
                    &function.qualified_name,
                    "function",
                    function.origin.clone(),
                    function.pub_,
                ));
            }
        }
        definitions.sort_by(|a, b| {
            a.get("id")
                .and_then(Value::as_str)
                .cmp(&b.get("id").and_then(Value::as_str))
        });
        definitions
    }

    fn declaration_value(
        &self,
        id: &NodeId,
        name: &str,
        kind: &str,
        origin: Origin,
        public: bool,
    ) -> Value {
        let (precision, span) = self
            .checked
            .node_meta()
            .get(id)
            .map(|meta| {
                (
                    format!("{:?}", meta.precision).to_lowercase(),
                    meta.origin.user_span(),
                )
            })
            .unwrap_or_else(|| ("exact".into(), origin.user_span()));
        json!({
            "id": id.0,
            "name": name,
            "kind": kind,
            "public": public,
            "span": span_value(span),
            "span_precision": precision,
            "origin": origin_value(&origin),
        })
    }

    fn call_sites_for_symbol(&self, name: &str, ids: &BTreeSet<String>) -> Vec<Value> {
        let mut sites = self
            .checked
            .call_sites_sorted()
            .into_iter()
            .filter(|site| call_site_targets(site, name, ids))
            .map(|site| self.call_site_value(site))
            .collect::<Vec<_>>();
        sites.sort_by(|a, b| {
            a.get("id")
                .and_then(Value::as_str)
                .cmp(&b.get("id").and_then(Value::as_str))
        });
        sites
    }

    fn call_site_value(&self, site: &crate::core::ResolvedCallSite) -> Value {
        json!({
            "id": site.node_id.0,
            "caller": site.owner,
            "callee": site.callee,
            "kind": format!("{:?}", site.kind).to_lowercase(),
            "argc": site.argc,
            "expected_argc": site.expected_argc,
            "effects": site.effects,
            "return_type": site.ret,
            "span": span_value(site.origin.user_span()),
            "resolved_callee": site.resolved_callee.as_ref().map(callee_value),
        })
    }

    fn transition_value(&self, transition: &ResolvedTransition) -> Value {
        json!({
            "id": transition.node_id.0,
            "flow": transition.id.flow.0,
            "event": transition.id.event,
            "source": transition.id.source.name,
            "targets": transition.targets.iter().map(|target| target.name.clone()).collect::<Vec<_>>(),
            "params": transition.params.iter().map(|(name, ty)| json!({ "name": name, "type": crate::core::fmt_type(ty) })).collect::<Vec<_>>(),
            "fallback": transition.is_fallback,
            "ffi_pinned": transition.is_ffi_pinned,
            "silent": transition.silent_transition,
            "span": span_value(transition.span),
        })
    }

    fn point_span(&self, owner: &NodeId, point: &NodeId) -> Value {
        self.checked
            .node_meta()
            .get(point)
            .map(|meta| span_value(meta.origin.user_span()))
            .or_else(|| {
                self.checked
                    .callable(owner)
                    .map(|callable| span_value(callable.body.root.origin.user_span()))
            })
            .unwrap_or_else(|| span_value(Span::UNKNOWN))
    }

    fn resolve_owner(&self, name: &str) -> Option<NodeId> {
        self.checked
            .functions()
            .values()
            .find(|function| name_matches(&function.qualified_name, name, &function.node_id.0))
            .map(|function| function.node_id.clone())
            .or_else(|| {
                self.checked
                    .callables()
                    .keys()
                    .find(|id| id.0 == name || id.0.ends_with(name))
                    .cloned()
            })
    }

    fn call_site_targets_owner(&self, site: &crate::core::ResolvedCallSite, owner: &str) -> bool {
        site.target_function
            .as_ref()
            .is_some_and(|id| id.0 == owner)
            || site
                .resolved_callee
                .as_ref()
                .and_then(callee_node_id)
                .is_some_and(|id| id.0 == owner)
            || site.callee == owner
            || self
                .resolve_owner(&site.callee)
                .is_some_and(|id| id.0 == owner)
    }
}

/// Build a diagnostic query response when checking itself failed and there is
/// therefore no `CheckedProgram` to attach to a `QueryService`.
pub fn diagnostics_response(diagnostics: &[Diagnostic], source: Option<&str>) -> Value {
    let entries = diagnostics.iter().map(diagnostic_value).collect::<Vec<_>>();
    let cache = json!({
        "key": format!(
            "{}:{}:checker:{}",
            QUERY_SCHEMA_VERSION, QUERY_SEMANTICS_VERSION, diagnostics.len()
        ),
        "valid": true,
        "invalidation": "source edits, checker diagnostics, or compiler version invalidate this result",
    });
    let metadata = json!({
        "schema_version": QUERY_SCHEMA_VERSION,
        "semantics_version": QUERY_SEMANTICS_VERSION,
        "compiler_version": env!("CARGO_PKG_VERSION"),
        "resolved_ir_version": crate::core::RESOLVED_IR_VERSION,
        "route": "checker",
        "mir_digest": Value::Null,
        "trust": QueryTrust::StaticChecked.as_str(),
        "status": QueryStatus::Checked.as_str(),
        "source": source,
        "identity": "<diagnostics>",
        "cache": cache.clone(),
    });
    json!({
        "schema_version": QUERY_SCHEMA_VERSION,
        "semantics_version": QUERY_SEMANTICS_VERSION,
        "compiler_version": env!("CARGO_PKG_VERSION"),
        "resolved_ir_version": crate::core::RESOLVED_IR_VERSION,
        "route": "checker",
        "mir_digest": Value::Null,
        "trust": QueryTrust::StaticChecked.as_str(),
        "status": QueryStatus::Checked.as_str(),
        "query": "diagnostics",
        "identity": "<diagnostics>",
        "cache": cache,
        "metadata": metadata,
        "data": { "diagnostics": entries, "count": diagnostics.len() },
    })
}

fn name_matches(qualified: &str, wanted: &str, id: &str) -> bool {
    qualified == wanted
        || qualified.rsplit("::").next() == Some(wanted)
        || id == wanted
        || id.ends_with(wanted)
}

fn call_site_targets(
    site: &crate::core::ResolvedCallSite,
    wanted: &str,
    ids: &BTreeSet<String>,
) -> bool {
    if site.callee == wanted || site.callee.rsplit("::").next() == Some(wanted) {
        return true;
    }
    if site
        .target_function
        .as_ref()
        .is_some_and(|id| ids.contains(&id.0) || id.0 == wanted)
    {
        return true;
    }
    site.resolved_callee
        .as_ref()
        .and_then(callee_node_id)
        .is_some_and(|id| ids.contains(&id.0) || id.0 == wanted)
}

fn callee_node_id(callee: &ResolvedCallee) -> Option<NodeId> {
    match callee {
        ResolvedCallee::Function(id)
        | ResolvedCallee::Constructor(id)
        | ResolvedCallee::Extern(id) => Some(id.clone()),
        ResolvedCallee::ActorMethod { actor, .. }
        | ResolvedCallee::ProtocolMethod {
            protocol: actor, ..
        } => Some(actor.clone()),
        ResolvedCallee::Transition(id) => Some(crate::core::mir::transition_owner_from_id(id)),
        ResolvedCallee::Builtin(_) | ResolvedCallee::LocalClosure(_) => None,
    }
}

fn item_kind(kind: ResolvedItemKind) -> &'static str {
    match kind {
        ResolvedItemKind::Function => "function",
        ResolvedItemKind::Type => "type",
        ResolvedItemKind::Constant => "constant",
        ResolvedItemKind::Capability => "capability",
        ResolvedItemKind::Trait => "trait",
        ResolvedItemKind::Impl => "impl",
        ResolvedItemKind::ExternBlock => "extern_block",
        ResolvedItemKind::Actor => "actor",
        ResolvedItemKind::Flow => "flow",
        ResolvedItemKind::Session => "session",
    }
}

fn origin_value(origin: &Origin) -> Value {
    match origin {
        Origin::User(span) => json!({ "kind": "user", "span": span_value(*span) }),
        Origin::Desugared { parent, rule, span } => {
            json!({ "kind": "desugared", "parent": parent.0, "rule": rule, "span": span_value(*span) })
        }
        Origin::PrototypeFallback { parent, rule, span } => {
            json!({ "kind": "prototype_fallback", "parent": parent.0, "rule": rule, "span": span_value(*span) })
        }
        Origin::RuntimeSystem { parent, rule, span } => {
            json!({ "kind": "runtime_system", "parent": parent.0, "rule": rule, "span": span_value(*span) })
        }
    }
}

fn span_value(span: Span) -> Value {
    json!({
        "source_id": span.source_id.raw(),
        "start": { "line": span.start_line, "column": span.start_col },
        "end": { "line": span.end_line, "column": span.end_col },
    })
}

fn action_kind(kind: CanonicalActionKind) -> &'static str {
    match kind {
        CanonicalActionKind::Read => "read",
        CanonicalActionKind::Write => "write",
        CanonicalActionKind::Introduce => "introduce",
        CanonicalActionKind::Move => "move",
        CanonicalActionKind::Drop => "drop",
        CanonicalActionKind::Return => "return",
        CanonicalActionKind::TransferSession => "transfer_session",
        CanonicalActionKind::TransferChild => "transfer_child",
        CanonicalActionKind::BorrowShared => "borrow_shared",
        CanonicalActionKind::BorrowMut => "borrow_mut",
        CanonicalActionKind::BorrowEnd => "borrow_end",
    }
}

fn action_counts(analysis: &ResourceAnalysis) -> Value {
    let mut counts = BTreeMap::new();
    for action in &analysis.actions {
        *counts.entry(action_kind(action.kind)).or_insert(0usize) += 1;
    }
    json!(counts)
}

fn callee_value(callee: &ResolvedCallee) -> Value {
    match callee {
        ResolvedCallee::Function(id) => json!({ "kind": "function", "id": id.0 }),
        ResolvedCallee::Constructor(id) => json!({ "kind": "constructor", "id": id.0 }),
        ResolvedCallee::Extern(id) => json!({ "kind": "extern", "id": id.0 }),
        ResolvedCallee::Builtin(id) => json!({ "kind": "builtin", "id": id.as_str() }),
        ResolvedCallee::LocalClosure(local) => json!({ "kind": "local_closure", "id": local.0.0 }),
        ResolvedCallee::ActorMethod { actor, method } => {
            json!({ "kind": "actor_method", "actor": actor.0, "method": method.as_str() })
        }
        ResolvedCallee::ProtocolMethod { protocol, method } => {
            json!({ "kind": "protocol_method", "protocol": protocol.0, "method": method.as_str() })
        }
        ResolvedCallee::Transition(id) => {
            json!({ "kind": "transition", "flow": id.flow.0, "event": id.event, "source": id.source.name })
        }
    }
}

fn collect_effects_block(
    block: &ResolvedBlock,
    entries: &mut Vec<Value>,
    effects: &mut BTreeSet<String>,
) {
    for statement in &block.statements {
        collect_effects_stmt(statement, entries, effects);
    }
    if let Some(result) = &block.result {
        collect_effects_expr(result, entries, effects);
    }
}

fn collect_effects_stmt(
    statement: &crate::core::ir::ResolvedStmt,
    entries: &mut Vec<Value>,
    effects: &mut BTreeSet<String>,
) {
    use crate::core::ir::ResolvedStmtKind as S;
    match &statement.kind {
        S::Bind { initializer, .. } => initializer
            .as_ref()
            .into_iter()
            .for_each(|e| collect_effects_expr(e, entries, effects)),
        S::Assign { value, .. }
        | S::Expr(value)
        | S::Contract {
            condition: value, ..
        } => collect_effects_expr(value, entries, effects),
        S::Return { value, .. } | S::Break(value) => value
            .as_ref()
            .into_iter()
            .for_each(|e| collect_effects_expr(e, entries, effects)),
        S::Continue | S::Drop(_) | S::NestedCallable(_) => {}
        S::While { condition, body } => {
            collect_effects_expr(condition, entries, effects);
            collect_effects_block(body, entries, effects);
        }
        S::WhileLet {
            initializer, body, ..
        } => {
            collect_effects_expr(initializer, entries, effects);
            collect_effects_block(body, entries, effects);
        }
        S::IfLet {
            initializer,
            then_block,
            else_block,
            ..
        } => {
            collect_effects_expr(initializer, entries, effects);
            collect_effects_block(then_block, entries, effects);
            if let Some(body) = else_block {
                collect_effects_block(body, entries, effects);
            }
        }
        S::Loop(body) | S::Scope { body, .. } => collect_effects_block(body, entries, effects),
        S::For { iterable, body, .. } => {
            collect_effects_expr(iterable, entries, effects);
            collect_effects_block(body, entries, effects);
        }
        S::Math(values) => values
            .iter()
            .for_each(|e| collect_effects_expr(e, entries, effects)),
        S::Pinned { value, body, .. } => {
            collect_effects_expr(value, entries, effects);
            collect_effects_block(body, entries, effects);
        }
    }
}

fn collect_effects_expr(
    expression: &ResolvedExpr,
    entries: &mut Vec<Value>,
    effects: &mut BTreeSet<String>,
) {
    for effect in &expression.effects {
        let id = effect.as_str().to_string();
        effects.insert(id.clone());
        entries.push(json!({ "effect": id, "node": expression.node_id.0, "span": span_value(expression.origin.user_span()) }));
    }
    use ResolvedExprKind as E;
    match &expression.kind {
        E::Project { value, projection } => {
            collect_effects_expr(value, entries, effects);
            if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                collect_effects_expr(index, entries, effects);
            }
        }
        E::FString(parts) => parts.iter().for_each(|part| {
            if let crate::core::ir::ResolvedFStringPart::Interpolation(e) = part {
                collect_effects_expr(e, entries, effects);
            }
        }),
        E::Binary { left, right, .. } => {
            collect_effects_expr(left, entries, effects);
            collect_effects_expr(right, entries, effects);
        }
        E::Unary { operand, .. }
        | E::TypeOf(operand)
        | E::Old(operand)
        | E::Try { value: operand, .. }
        | E::Spawn(operand)
        | E::Await(operand)
        | E::Cast { value: operand, .. } => collect_effects_expr(operand, entries, effects),
        E::Call(call) => {
            for effect in &call.effects {
                let id = effect.as_str().to_string();
                if effects.insert(id.clone()) {
                    entries.push(json!({
                        "effect": id,
                        "node": expression.node_id.0,
                        "span": span_value(expression.origin.user_span()),
                        "source": "resolved-call",
                    }));
                }
            }
            call.arguments
                .iter()
                .for_each(|argument| collect_effects_expr(&argument.value, entries, effects));
        }
        E::Tuple(values) | E::List(values) | E::Set(values) => values
            .iter()
            .for_each(|e| collect_effects_expr(e, entries, effects)),
        E::Map(entries_map) => entries_map.iter().for_each(|(k, v)| {
            collect_effects_expr(k, entries, effects);
            collect_effects_expr(v, entries, effects);
        }),
        E::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            collect_effects_expr(value, entries, effects);
            collect_effects_expr(iterable, entries, effects);
            if let Some(guard) = guard {
                collect_effects_expr(guard, entries, effects);
            }
        }
        E::OptionalChain { receiver, .. } => collect_effects_expr(receiver, entries, effects),
        E::Record { fields, rest, .. } => {
            fields
                .iter()
                .for_each(|field| collect_effects_expr(&field.value, entries, effects));
            if let Some(rest) = rest {
                collect_effects_expr(rest, entries, effects);
            }
        }
        E::Block(body) | E::Comptime(body) | E::Quote(body) => {
            collect_effects_block(body, entries, effects)
        }
        E::Scope { body, .. } => collect_effects_block(body, entries, effects),
        E::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_effects_expr(condition, entries, effects);
            collect_effects_block(then_block, entries, effects);
            collect_effects_block(else_block, entries, effects);
        }
        E::Match { scrutinee, arms } => {
            collect_effects_expr(scrutinee, entries, effects);
            arms.iter().for_each(|arm| {
                if let Some(guard) = &arm.guard {
                    collect_effects_expr(guard, entries, effects);
                }
                collect_effects_expr(&arm.body, entries, effects);
            });
        }
        E::Range { start, end } => {
            collect_effects_expr(start, entries, effects);
            collect_effects_expr(end, entries, effects);
        }
        E::Slice { target, start, end } => {
            collect_effects_expr(target, entries, effects);
            if let Some(start) = start {
                collect_effects_expr(start, entries, effects);
            }
            if let Some(end) = end {
                collect_effects_expr(end, entries, effects);
            }
        }
        E::Lambda(lambda) => collect_effects_block(&lambda.body, entries, effects),
        E::Literal(_)
        | E::Load(_)
        | E::Constant(_)
        | E::Callable(_)
        | E::DefaultArgument { .. }
        | E::ComptimeValue(_)
        | E::TypeValue(_) => {}
    }
}

fn cost_classification(
    kind: &MirInstructionKind,
) -> (Option<&'static str>, &'static str, &'static str) {
    use MirInstructionKind as K;
    match kind {
        K::ConstructList { .. }
        | K::ConstructSet { .. }
        | K::MapRootNew { .. }
        | K::OrdinaryMapOwnerLifecycleNew { .. }
        | K::OrdinaryMapOwnedGetI32StringV1New { .. }
        | K::ConstructVariantMove { .. } => (
            Some("allocation"),
            "must",
            "MIR constructs a move-owned heap value or opaque root",
        ),
        K::Construct { .. } | K::ConstructVariant { .. } => (
            Some("allocation"),
            "may",
            "aggregate construction may allocate according to its TypeDesc",
        ),
        K::Clone { .. } | K::ManagedAny { .. } => (
            Some("clone"),
            "may",
            "ownership-preserving copy/clone operation",
        ),
        K::Drop { .. }
        | K::MapRootDrop { .. }
        | K::OrdinaryMapOwnerLifecycleDrop { .. }
        | K::OrdinaryMapOwnedGetI32StringV1Drop { .. } => (
            Some("release"),
            "must",
            "MIR explicitly releases an owned value",
        ),
        K::ListOp { .. } | K::SetOp { .. } | K::UpdateRecord { .. } => (
            Some("copy_or_resize"),
            "may",
            "capacity, copy-on-write, or residual glue depends on runtime shape",
        ),
        K::Call { .. }
        | K::BuiltinCall { .. }
        | K::Actor { .. }
        | K::SessionCall { .. }
        | K::FlowTransition { .. }
        | K::ClosureInvoke { .. } => (
            Some("call_boundary"),
            "may",
            "callee or runtime boundary has no closed cost receipt",
        ),
        _ => (None, "must", ""),
    }
}

fn diagnostic_value(diagnostic: &Diagnostic) -> Value {
    let root = json!({
        "code": diagnostic.code,
        "severity": match diagnostic.severity { Severity::Error => "error", Severity::Warning => "warning", Severity::Note => "note", Severity::Help => "help" },
        "message": diagnostic.message,
        "span": span_value(diagnostic.span),
    });
    let related = diagnostic
        .notes
        .iter()
        .map(|note| json!({ "message": note.message, "span": span_value(note.span) }))
        .collect::<Vec<_>>();
    let repair_safe = diagnostic.help.is_some()
        && !matches!(
            diagnostic.code.as_deref(),
            Some("E0256" | "E0304" | "E0423" | "E0425")
        );
    json!({
        "root": root,
        "cause_chain": [{ "kind": "diagnostic", "code": diagnostic.code, "message": diagnostic.message, "span": span_value(diagnostic.span) }],
        "related": related,
        "help": diagnostic.help,
        "repair": { "safe": repair_safe, "action": if repair_safe { diagnostic.help.as_deref() } else { None::<&str> } },
        "origin": diagnostic.origin.as_ref().map(|origin| json!({ "kind": format!("{:?}", origin.kind).to_lowercase(), "rule": origin.rule, "parent": origin.parent_node_id })),
    })
}

fn sorted_unique_strings<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    values
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn sorted_strings<'a>(values: impl Iterator<Item = &'a String>) -> Vec<String> {
    values
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::loader;
    use crate::parser::Parser;

    fn checked(source: &str) -> CheckedProgram {
        let tokens = Lexer::new(source).tokenize().expect("tokens");
        let (mut file, errors) = Parser::new(tokens).parse_file_with_recovery();
        assert!(errors.is_empty(), "parse errors: {errors:?}");
        loader::merge_prelude_into(&mut file);
        crate::core::check_program(&file).expect("checked program")
    }

    #[test]
    fn symbol_query_has_versioned_identity_and_source_span() {
        let checked =
            checked("func helper(x: i32) -> i32 { x + 1 }\nfunc main() -> i32 { helper(1) }");
        let result = QueryService::new(&checked).symbol("helper");
        assert_eq!(result["schema_version"], QUERY_SCHEMA_VERSION);
        assert_eq!(result["status"], "checked");
        assert!(result["data"]["definitions"][0]["id"].as_str().is_some());
        assert!(result["data"]["definitions"][0]["span"]["start"]["line"]
            .as_u64()
            .is_some());
        assert_eq!(result["data"]["references"][0]["callee"], "helper");
    }

    #[test]
    fn unknown_query_does_not_claim_static_fact() {
        let checked = checked("func main() -> i32 { 0 }");
        let result = QueryService::new(&checked).symbol("missing");
        assert_eq!(result["status"], "unknown");
        assert_eq!(result["trust"], "unsupported");
    }

    #[test]
    fn holes_are_explicitly_experimental() {
        let checked = checked("func main() -> i32 { 0 }");
        let result = QueryService::new(&checked).holes();
        assert_eq!(result["status"], "experimental");
        assert_eq!(result["data"]["build_allowed"], false);
    }

    #[test]
    fn impact_reports_extern_boundary_inside_short_named_definition() {
        let checked = checked(
            r#"
extern "C" {
    func c_abs(x: i32) -> i32
}
func helper(x: i32) -> i32 { c_abs(x) }
func main() -> i32 { helper(1) }
"#,
        );
        let result = QueryService::new(&checked).impact("helper");
        assert_eq!(result["status"], "checked");
        assert_eq!(result["trust"], "conditional_upper_bound");
        let boundaries = result["data"]["unknown_boundaries"]
            .as_array()
            .expect("impact must expose unknown boundaries as an array");
        assert!(
            boundaries.iter().any(|boundary| boundary == "c_abs"),
            "short-name impact query must include extern calls in the changed callable: {boundaries:?}"
        );
        let recompiles = result["data"]["recompile"]
            .as_array()
            .expect("impact must expose recompile owners");
        assert!(recompiles.iter().any(|owner| owner == "function:helper"));
        assert!(recompiles.iter().any(|owner| owner == "function:main"));
    }
}
