//! Checker-owned coverage plan for contract verification requests.
//!
//! Execution islands are not the verification library's admission policy:
//! libraries need no `main`, and a Flow may have several states and guarded
//! transitions. This plan accounts for the complete user callable graph before
//! a verifier consumes it. It never retains a surface expression or invents a
//! body for an unsupported declaration. Deferred edges remain the responsibility
//! of the explicitly named compatibility owner at the public entry point.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::core::ir::{
    CheckedConversionKind, ResolvedBlock, ResolvedCallable, ResolvedCallee, ResolvedExpr,
    ResolvedExprKind, ResolvedFStringPart, ResolvedStmtKind, ResolvedValueProjection,
};
use crate::core::{CheckedProgram, NodeId, Origin};
use crate::span::SourceId;

use super::reference::MirProgram;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirVerificationBoundary {
    pub owner: NodeId,
    pub point: Option<NodeId>,
    pub reason: String,
}

/// This is a materialized request, not a proof or a consumer admission receipt.
/// The receiving verifier must still run its whole-graph capability gate.
pub struct MirVerificationRequest {
    canonical: MirProgram,
    names: BTreeMap<NodeId, String>,
}

impl MirVerificationRequest {
    pub fn program(&self) -> &MirProgram {
        &self.canonical
    }

    pub fn names(&self) -> &BTreeMap<NodeId, String> {
        &self.names
    }
}

pub enum MirVerificationPreparation {
    /// Requests already handled by the shared closed route are unchanged.
    NotRequested,
    Deferred(Vec<MirVerificationBoundary>),
    Materialized(MirVerificationRequest),
}

/// A deliberately partial, Checker-derived projection for the actor
/// compatibility verifier.  It names the exact Flow contract roots and their
/// direct-call closure while retaining the complete user-source census on the
/// resulting MIR program.  The actor method set is kept alongside the body
/// scope so no actor implementation body can be swept into this Flow route.
///
/// This is a slice receipt, not an owner-closure claim: roots outside
/// `owner_scope` remain with the existing compatibility consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckerBoundActorFlowProjectionV1 {
    pub(crate) excluded_sources: HashSet<SourceId>,
    pub(crate) actor_scope: BTreeSet<NodeId>,
    pub(crate) source_callable_owners: BTreeSet<NodeId>,
    pub(crate) source_proof_owners: BTreeSet<NodeId>,
    pub(crate) source_declaration_roots: BTreeSet<NodeId>,
    pub(crate) source_proof_declaration_roots: BTreeSet<NodeId>,
    pub(crate) foreign_declaration_roots: BTreeSet<NodeId>,
    pub(crate) flow_transition_roots: BTreeSet<NodeId>,
    pub(crate) transition_callers: BTreeSet<NodeId>,
    pub(crate) owner_scope: BTreeSet<NodeId>,
    pub(crate) call_edges: BTreeSet<(NodeId, NodeId)>,
}

/// Derive the narrow actor/Flow body projection from canonical Checker
/// identities.  Unsupported edges return `None`, leaving the pre-existing
/// compatibility route responsible for every result.  In particular, this
/// function does not infer a callee from display names or reconstruct syntax.
pub(crate) fn checker_bound_actor_flow_projection_v1(
    program: &CheckedProgram,
) -> Option<CheckerBoundActorFlowProjectionV1> {
    if crate::core::mir::scalar_actor_contract_boundary_reason(program).is_none()
        || !crate::core::mir::actor_lower::has_actor_candidate(program, None)
        || program.actors().is_empty()
        || !program.impls().is_empty()
        || !program.sessions().is_empty()
        || !program.extern_blocks().is_empty()
        || !program.ffi_declarations().is_empty()
        || !program.native_declarations().exports.is_empty()
    {
        return None;
    }

    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission.ordinary_map_owned_any_v2.is_candidate()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .is_candidate()
        || admission
            .ordinary_map_owner_lifecycle_observer_v1
            .is_candidate()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .is_candidate()
    {
        return None;
    }

    let excluded_sources = program
        .source_registry()
        .records()
        .iter()
        .filter_map(|record| (record.key.as_str() == "stdlib:prelude.mimi").then_some(record.id))
        .collect::<HashSet<_>>();
    let actor_scope = program
        .actor_descriptors()
        .values()
        .flat_map(|actor| actor.methods.iter().map(|method| method.owner.clone()))
        .collect::<BTreeSet<_>>();
    let source_callable_owners = program
        .callables()
        .iter()
        .filter_map(|(owner, callable)| {
            (!excluded_sources.contains(&callable.body.root.origin.user_span().source_id))
                .then_some(owner.clone())
        })
        .collect::<BTreeSet<_>>();

    let source_proof_owners = program
        .callables()
        .iter()
        .filter_map(|(owner, callable)| {
            (!excluded_sources.contains(&callable.body.root.origin.user_span().source_id)
                && callable.has_proof_obligations())
            .then_some(owner.clone())
        })
        .collect::<BTreeSet<_>>();
    let source_declaration_roots = program
        .declaration_expression_bodies()
        .iter()
        .filter_map(|(owner, root)| {
            let source = root
                .body
                .as_ref()
                .ok()
                .map(|body| body.root.origin.user_span().source_id);
            source
                .is_none_or(|source| !excluded_sources.contains(&source))
                .then_some(owner.clone())
        })
        .collect::<BTreeSet<_>>();
    let source_proof_declaration_roots = program
        .declaration_expression_bodies()
        .iter()
        .filter_map(|(owner, root)| {
            let has_proof = matches!(
                root.kind,
                crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires
                    | crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternEnsures
            ) || root
                .body
                .as_ref()
                .map_or(true, crate::core::ir::ResolvedBody::has_proof_obligations);
            has_proof.then_some(owner.clone())
        })
        .collect::<BTreeSet<_>>();
    let foreign_declaration_roots = program.ffi_declarations().keys().cloned().collect();
    if !source_proof_declaration_roots.is_empty() {
        return None;
    }

    let flow_transition_roots = program
        .transitions()
        .values()
        .filter_map(|transition| {
            let callable = program.callable(&transition.node_id)?;
            (!excluded_sources.contains(&callable.body.root.origin.user_span().source_id)
                && callable.has_proof_obligations())
            .then_some(transition.node_id.clone())
        })
        .collect::<BTreeSet<_>>();
    if flow_transition_roots.is_empty() {
        return None;
    }

    let transition_ids = program
        .transitions()
        .values()
        .filter(|transition| flow_transition_roots.contains(&transition.node_id))
        .map(|transition| (transition.id.clone(), transition.node_id.clone()))
        .collect::<HashMap<_, _>>();
    let mut transition_callers = BTreeSet::new();
    for site in program.call_sites_sorted() {
        let Some(ResolvedCallee::Transition(transition)) = site.resolved_callee.as_ref() else {
            continue;
        };
        if !transition_ids.contains_key(transition) {
            continue;
        }
        let caller = NodeId(site.owner.clone());
        let Some(callable) = program.callable(&caller) else {
            return None;
        };
        if excluded_sources.contains(&callable.body.root.origin.user_span().source_id) {
            continue;
        }
        transition_callers.insert(caller);
    }

    let mut owner_scope = flow_transition_roots.clone();
    owner_scope.extend(transition_callers.iter().cloned());
    let mut call_edges = BTreeSet::new();
    let mut pending = owner_scope.iter().cloned().collect::<VecDeque<_>>();
    while let Some(owner) = pending.pop_front() {
        if actor_scope.contains(&owner) {
            return None;
        }
        let callable = program.callable(&owner)?;
        if excluded_sources.contains(&callable.body.root.origin.user_span().source_id)
            || !callable.signature.generic_parameters.is_empty()
            || !callable.body.captures.is_empty()
            || !callable.body.default_values.is_empty()
            || callable.has_unmodeled_proof_roots()
            || callable.signature.parameters.iter().any(|parameter| {
                matches!(
                    parameter.permission,
                    Some(crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate)
                )
            })
        {
            return None;
        }
        if program.functions().get(&owner).is_some_and(|function| {
            function.is_async
                || function.is_comptime
                || function.extern_abi.is_some()
                || !function.generics.is_empty()
        }) {
            return None;
        }

        // Include every Checker-known caller that can observe a selected
        // transition or helper contract. This admits ordinary `main ->
        // helper -> transition` chains into the same proof receipt, while an
        // Actor method or a declaration root crossing into the slice keeps
        // the whole request on the established compatibility route.
        for site in program.call_sites_sorted() {
            let target = match site.resolved_callee.as_ref() {
                Some(ResolvedCallee::Function(target)) => Some(target.clone()),
                Some(ResolvedCallee::Transition(transition)) => program
                    .transitions()
                    .get(transition)
                    .map(|target| target.node_id.clone()),
                _ => None,
            };
            if target.as_ref() != Some(&owner) {
                continue;
            }
            let caller = NodeId(site.owner.clone());
            if owner_scope.contains(&caller) {
                continue;
            }
            if actor_scope.contains(&caller) {
                return None;
            }
            let caller_callable = program.callable(&caller)?;
            if excluded_sources.contains(&caller_callable.body.root.origin.user_span().source_id) {
                continue;
            }
            if owner_scope.insert(caller.clone()) {
                pending.push_back(caller);
            }
        }

        // The body call directory and the independent sorted source census
        // must agree exactly for every selected owner. This catches missing,
        // retargeted, or declaration-root call edges before MIR lowering.
        let owner_sites = program
            .call_sites_sorted()
            .into_iter()
            .filter(|site| site.owner == owner.0)
            .collect::<Vec<_>>();
        if owner_sites.len() != callable.body.resolved_calls.len()
            || owner_sites.iter().any(|site| {
                callable.body.resolved_calls.get(&site.node_id) != site.resolved_callee.as_ref()
            })
        {
            return None;
        }

        for (call_node, callee) in &callable.body.resolved_calls {
            let Some(site) = program.call_sites().get(call_node) else {
                return None;
            };
            if site.node_id != *call_node
                || site.owner != owner.0
                || site.resolved_callee.as_ref() != Some(callee)
                || !site.arity_matches()
            {
                return None;
            }
            let target = match callee {
                ResolvedCallee::Function(target) => target.clone(),
                ResolvedCallee::Transition(transition) => {
                    transition_ids.get(transition).cloned().or_else(|| {
                        program
                            .transitions()
                            .get(transition)
                            .map(|target| target.node_id.clone())
                    })?
                }
                ResolvedCallee::Constructor(_) | ResolvedCallee::Builtin(_) => continue,
                ResolvedCallee::Extern(_)
                | ResolvedCallee::LocalClosure(_)
                | ResolvedCallee::ActorMethod { .. }
                | ResolvedCallee::ProtocolMethod { .. } => return None,
            };
            if actor_scope.contains(&target) || program.callable(&target).is_none() {
                return None;
            }
            call_edges.insert((owner.clone(), target.clone()));
            if owner_scope.insert(target.clone()) {
                pending.push_back(target);
            }
        }
    }

    // Closing only outgoing calls is insufficient for a verifier owner: a
    // call-site proof in an unselected owner can still inspect a selected
    // callee's contract/body.  Keep the slice closed at both ends.  Actor
    // methods, declaration roots, and unrelated functions that call into the
    // selected Flow closure therefore retain the original compatibility
    // route for the whole request.
    for site in program.call_sites_sorted() {
        let target = match site.resolved_callee.as_ref() {
            Some(ResolvedCallee::Function(target)) => Some(target.clone()),
            Some(ResolvedCallee::Transition(transition)) => program
                .transitions()
                .get(transition)
                .map(|target| target.node_id.clone()),
            _ => None,
        };
        let Some(_target) = target.filter(|target| owner_scope.contains(target)) else {
            continue;
        };
        let caller = NodeId(site.owner.clone());
        if owner_scope.contains(&caller) {
            continue;
        }
        if program.callable(&caller).is_none_or(|callable| {
            !excluded_sources.contains(&callable.body.root.origin.user_span().source_id)
        }) {
            return None;
        }
    }

    Some(CheckerBoundActorFlowProjectionV1 {
        excluded_sources,
        actor_scope,
        source_callable_owners,
        source_proof_owners,
        source_declaration_roots,
        source_proof_declaration_roots,
        foreign_declaration_roots,
        flow_transition_roots,
        transition_callers,
        owner_scope,
        call_edges,
    })
}

/// Prepare the selected Checker-bound Actor/Flow slice through the same
/// canonical proof-request envelope used by ordinary callable verification.
/// This remains a partial request: its source census is complete, while the
/// projection's owner scope is the only executable proof component. Owners
/// outside that scope remain with their existing verifier owner.
pub(crate) fn prepare_actor_flow_verification_request_v1(
    program: &CheckedProgram,
) -> Result<Option<(CheckerBoundActorFlowProjectionV1, MirVerificationRequest)>, String> {
    let Some(projection) = checker_bound_actor_flow_projection_v1(program) else {
        return Ok(None);
    };
    let canonical = MirProgram::from_checked_program_actor_flow_projection_v1(program, &projection)
        .map_err(|error| {
            format!(
                "{}: Checker-selected Actor/Flow MIR request could not be materialized: {error:?}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })?;

    let mut names = BTreeMap::new();
    for owner in &projection.owner_scope {
        let name = program
            .functions()
            .get(owner)
            .map(|function| function.qualified_name.clone())
            .or_else(|| {
                program.transitions().values().find_map(|transition| {
                    (transition.node_id == *owner)
                        .then(|| format!("{}::{}", transition.id.flow.0, transition.id.event))
                })
            })
            .ok_or_else(|| {
                format!(
                    "{}: Checker-selected Actor/Flow owner {} has no canonical result identity",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    owner.0
                )
            })?;
        names.insert(owner.clone(), name);
    }
    if names.len() != projection.owner_scope.len()
        || names.values().collect::<BTreeSet<_>>().len() != names.len()
    {
        return Err(format!(
            "{}: Checker-selected Actor/Flow owners do not have unique result identities",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }

    Ok(Some((
        projection,
        MirVerificationRequest { canonical, names },
    )))
}

/// Materialize the complete checked proof-owner graph before a backend runs.
/// Root contracts, ordered Math points, and the stationary scalar loop profile
/// have typed MIR consumers; unsupported mutation/effect/induction boundaries
/// remain explicit and keep the named compatibility owner reachable. A proof
/// root outside the admitted profile is never trimmed into an empty request.
pub fn prepare_verification_request(program: &CheckedProgram) -> MirVerificationPreparation {
    let excluded = program
        .source_registry()
        .records()
        .iter()
        .filter_map(|record| (record.key.as_str() == "stdlib:prelude.mimi").then_some(record.id))
        .collect::<HashSet<_>>();
    let selected = program
        .callables()
        .iter()
        .filter(|(_, callable)| {
            !excluded.contains(&callable.body.root.origin.user_span().source_id)
        })
        .collect::<Vec<_>>();
    let transitions = program
        .transitions()
        .values()
        .map(|transition| (transition.node_id.clone(), transition))
        .collect::<BTreeMap<_, _>>();
    if !selected
        .iter()
        .any(|(_, callable)| callable.has_proof_obligations())
    {
        return MirVerificationPreparation::NotRequested;
    }

    let mut boundaries = Vec::new();
    let mut names = BTreeMap::new();
    let mut graph = BTreeMap::new();
    for (owner, callable) in selected {
        let name = program
            .functions()
            .get(owner)
            .map(|function| function.qualified_name.clone())
            .or_else(|| {
                transitions
                    .get(owner)
                    .map(|transition| format!("{}::{}", transition.id.flow.0, transition.id.event))
            });
        if let Some(name) = name {
            names.insert(owner.clone(), name);
        } else {
            boundaries.push(boundary(
                owner,
                None,
                "callable identity has no function/transition result mapping",
            ));
        }
        if !callable.signature.generic_parameters.is_empty()
            || !callable_effect_model_is_supported(callable, program)
            || !callable.body.captures.is_empty()
            || !callable.body.default_values.is_empty()
            || callable.signature.parameters.iter().any(|parameter| {
                matches!(
                    parameter.permission,
                    Some(crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate)
                )
            })
        {
            boundaries.push(boundary(owner, None, "generic, borrowed, captured, default, or effectful callable needs its typed proof model"));
        }
        if callable.has_unmodeled_proof_roots() {
            boundaries.push(boundary(
                owner,
                None,
                "nested/default/closure proof roots require scoped program-point receipts",
            ));
        }
        let proof_worklist = callable.proof_point_worklist();
        let proof_math_nodes = proof_worklist.math_nodes().into_iter().collect();
        let proof_invariant_nodes: BTreeSet<NodeId> =
            proof_worklist.loop_invariant_nodes().into_iter().collect();
        let proof_loop_nodes = proof_worklist.loop_nodes().into_iter().collect();
        let root_contracts = callable
            .body
            .root
            .statements
            .iter()
            .filter_map(|statement| {
                matches!(statement.kind, ResolvedStmtKind::Contract { .. })
                    .then_some(statement.node_id.clone())
            })
            .collect::<BTreeSet<_>>();
        if callable.contracts.iter().any(|contract| {
            !root_contracts.contains(&contract.node_id)
                && !proof_invariant_nodes.contains(&contract.node_id)
        }) {
            boundaries.push(boundary(
                owner,
                None,
                "nested contracts require program-point receipts; do not hoist them to entry",
            ));
        }
        let mut scanner = Scanner {
            owner,
            boundaries: &mut boundaries,
            callees: BTreeSet::new(),
            root_contracts,
            proof_math_nodes,
            proof_invariant_nodes,
            proof_loop_nodes,
        };
        scanner.block(&callable.body.root);
        for expression in callable.body.place_inputs.values() {
            scanner.expression(expression);
        }
        graph.insert(owner.clone(), scanner.callees);
    }
    for function in program.functions().values() {
        if excluded.contains(&function.origin.user_span().source_id) {
            continue;
        }
        if function.extern_abi.is_some() || function.is_async || function.is_comptime {
            boundaries.push(boundary(
                &function.node_id,
                None,
                "export/comptime/async function has a separate declaration or effect obligation",
            ));
        }
    }
    let mut referenced_extern_declarations = BTreeSet::new();
    let mut unresolved_extern_symbols = BTreeSet::new();
    for site in program.call_sites_sorted() {
        match site.resolved_callee.as_ref() {
            Some(ResolvedCallee::Extern(declaration)) => {
                referenced_extern_declarations.insert((*declaration).clone());
            }
            // An extern-shaped site without an exact typed declaration target
            // cannot prove that a same-symbol declaration is unreachable.
            // Keep that malformed/incomplete source state fail-closed.
            Some(_) | None if site.kind == crate::core::ResolvedCallKind::Extern => {
                unresolved_extern_symbols.insert(site.callee.clone());
            }
            Some(_) | None => {}
        }
    }
    for block in program.extern_blocks().values() {
        for declaration in &block.signatures {
            // Only a declaration with an actual contract root or a typed
            // extern call-site reference contributes an obligation to this
            // proof request.  Keep missing descriptor identity fail-closed:
            // without the checker-owned contract witness we cannot prove the
            // declaration is obligation-free.
            let has_contract = program
                .ffi_declarations()
                .get(&declaration.node_id)
                .map(|descriptor| descriptor.has_declared_contract())
                .unwrap_or(true);
            let is_referenced = referenced_extern_declarations.contains(&declaration.node_id)
                || unresolved_extern_symbols.contains(&declaration.name);
            if has_contract || is_referenced {
                boundaries.push(boundary(
                    &declaration.node_id,
                    None,
                    "called or contract-bearing extern declaration obligations remain with the FFI compatibility owner",
                ));
            }
        }
    }
    if !program.actors().is_empty() || !program.impls().is_empty() || !program.sessions().is_empty()
    {
        boundaries.push(boundary(
            &NodeId("verification:request".into()),
            None,
            "Actor, impl, or Session contract graph needs its canonical effect proof model",
        ));
    }
    // Display labels are not call identities. Until public results carry an
    // owner field, overloaded transition labels cannot safely be dual-merged.
    if names.values().collect::<BTreeSet<_>>().len() != names.len() {
        boundaries.push(boundary(
            &NodeId("verification:request".into()),
            None,
            "overloaded result labels require owner-keyed dual merging",
        ));
    }
    if let Some(owner) = cyclic_owner(&graph) {
        boundaries.push(boundary(&owner, None, "recursive call graph requires inductive summaries rather than recursive symbolic inlining"));
    }
    if !boundaries.is_empty() {
        return MirVerificationPreparation::Deferred(boundaries);
    }
    match MirProgram::from_checked_program_excluding_sources(program, &excluded) {
        Ok(canonical) => {
            for transition in canonical.transitions().values() {
                if transition.effect.is_recoverable()
                    && canonical
                        .functions()
                        .get(&transition.owner)
                        .is_some_and(|function| !function.contracts.is_empty())
                    && !has_exact_product_model(
                        &canonical,
                        &transition.source,
                        &mut BTreeSet::new(),
                    )
                {
                    boundaries.push(boundary(&transition.owner, None, "fails source equality has no exact canonical payload model; opaque type/handle equality would erase rollback semantics"));
                }
            }
            if !boundaries.is_empty() {
                return MirVerificationPreparation::Deferred(boundaries);
            }
            if names
                .keys()
                .any(|owner| !canonical.functions().contains_key(owner))
            {
                return MirVerificationPreparation::Deferred(vec![boundary(
                    &NodeId("verification:request".into()),
                    None,
                    "materialization omitted a selected callable; the request is incomplete",
                )]);
            }
            MirVerificationPreparation::Materialized(MirVerificationRequest { canonical, names })
        }
        Err(error) => MirVerificationPreparation::Deferred(vec![boundary(
            &NodeId("verification:request".into()),
            None,
            &format!(
                "canonical typed body/TypeDesc/ownership lowering remains deferred: {error:?}"
            ),
        )]),
    }
}

const STDOUT_EFFECT_NAME: &str = "shared.modify";
const SHARED_BINDING_ORIGIN_RULE: &str = "resolved_body.shared_binding";

/// Admit an otherwise unsupported effect summary only when its complete typed
/// body accounts for every effect as a direct `println` or an exact checked
/// Flow transition. MIR gives `println` a dedicated `StdoutLine` contract, and
/// transitions have their own typed call/contract receipt; broad effect labels
/// alone never authorize either operation.
fn callable_effect_model_is_supported(
    callable: &ResolvedCallable,
    program: &CheckedProgram,
) -> bool {
    let Some(modeled_effects) = callable_modeled_effects(callable, program, true, None) else {
        return false;
    };
    let declared_effects = callable
        .signature
        .effects
        .iter()
        .map(|effect| effect.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    declared_effects == modeled_effects
}

/// Return whether this callable's entire effect summary is explained by
/// direct, checker-typed `println` calls. The scalar collection island uses
/// this narrow receipt for its stdout face; other effectful callables remain
/// mixed. Flow transition receipts are intentionally disabled here because
/// that island has no transition consumer contract.
pub(super) fn callable_effect_model_is_direct_stdout_only(
    callable: &ResolvedCallable,
    program: &CheckedProgram,
) -> bool {
    if callable.signature.effects.is_empty() {
        return false;
    }
    let Some(modeled_effects) = callable_modeled_effects(callable, program, false, None) else {
        return false;
    };
    let declared_effects = callable
        .signature
        .effects
        .iter()
        .map(|effect| effect.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    !modeled_effects.is_empty()
        && modeled_effects
            .iter()
            .all(|effect| effect == STDOUT_EFFECT_NAME)
        && declared_effects == modeled_effects
}

/// The scalar island permits one additional checked edge: a caller may invoke
/// a callee already discovered as a direct scalar print face. The callee is
/// independently required to have only direct `println` effects, while the
/// call-site directory must repeat that exact effect set and resolved target.
pub(super) fn callable_effect_model_is_scalar_stdout_only(
    callable: &ResolvedCallable,
    program: &CheckedProgram,
    admitted_direct_stdout_callees: &BTreeSet<NodeId>,
) -> bool {
    if callable.signature.effects.is_empty() {
        return false;
    }
    let Some(modeled_effects) = callable_modeled_effects(
        callable,
        program,
        false,
        Some(admitted_direct_stdout_callees),
    ) else {
        return false;
    };
    let declared_effects = callable
        .signature
        .effects
        .iter()
        .map(|effect| effect.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    !modeled_effects.is_empty()
        && modeled_effects
            .iter()
            .all(|effect| effect == STDOUT_EFFECT_NAME)
        && declared_effects == modeled_effects
}

fn callable_modeled_effects(
    callable: &ResolvedCallable,
    program: &CheckedProgram,
    allow_transition_receipts: bool,
    admitted_direct_stdout_callees: Option<&BTreeSet<NodeId>>,
) -> Option<BTreeSet<String>> {
    let mut audit = StdoutEffectAudit {
        program,
        modeled_effects: BTreeSet::new(),
        allow_transition_receipts,
        admitted_direct_stdout_callees,
        valid: true,
    };
    audit.block(&callable.body.root);
    for expression in callable.body.place_inputs.values() {
        audit.expression(expression);
    }
    for expression in callable.body.default_values.values() {
        audit.expression(expression);
    }
    audit.valid.then_some(audit.modeled_effects)
}

struct StdoutEffectAudit<'a> {
    program: &'a CheckedProgram,
    modeled_effects: BTreeSet<String>,
    allow_transition_receipts: bool,
    admitted_direct_stdout_callees: Option<&'a BTreeSet<NodeId>>,
    valid: bool,
}

struct TypedTransitionEffectReceipt {
    target_effects: BTreeSet<String>,
    call_site_effects: BTreeSet<String>,
}

impl StdoutEffectAudit<'_> {
    fn expression_effects_are_stdout_only(&self, expression: &ResolvedExpr) -> bool {
        expression
            .effects
            .iter()
            .all(|effect| effect.as_str() == STDOUT_EFFECT_NAME)
    }

    fn expression_effects_are_supported(&self, expression: &ResolvedExpr) -> bool {
        if let ResolvedExprKind::Call(call) = &expression.kind {
            if matches!(call.callee, ResolvedCallee::Transition(_)) {
                let Some(receipt) = self.typed_transition_effects(expression, call) else {
                    return false;
                };
                let expression_effects = expression
                    .effects
                    .iter()
                    .map(|effect| effect.as_str().to_owned())
                    .collect::<BTreeSet<_>>();
                return receipt.target_effects == expression_effects;
            }
        }
        self.expression_effects_are_stdout_only(expression)
    }

    fn is_exact_stdout_call(
        &self,
        expression: &ResolvedExpr,
        call: &crate::core::ir::ResolvedCall,
    ) -> bool {
        let unit = crate::core::ir::primitive_type_id(crate::core::ir::PrimitiveType::Unit);
        let [argument] = call.arguments.as_slice() else {
            return false;
        };
        let builtin_effects = crate::core::effects::builtin_effects("println");
        let call_effects_are_stdout_only = call.effects.is_empty()
            || (call.effects.len() == 1 && call.effects[0].as_str() == STDOUT_EFFECT_NAME);
        call.callee
            == ResolvedCallee::Builtin(
                crate::core::ir::BuiltinId::new("println")
                    .expect("static builtin identity is non-empty"),
            )
            && builtin_effects.len() == 1
            && builtin_effects.contains(crate::core::effects::EffectKind::SharedMutation)
            && call.result == unit
            && expression.ty == unit
            && call.type_arguments.is_empty()
            && call.permission.is_none()
            && call.session.is_empty()
            && call_effects_are_stdout_only
            && argument.parameter.0 == NodeId("builtin:println/parameter:0".into())
            && argument.conversion.kind == CheckedConversionKind::Identity
            && argument.conversion.from == argument.value.ty
            && argument.conversion.to == argument.value.ty
    }

    fn typed_transition_effects(
        &self,
        expression: &ResolvedExpr,
        call: &crate::core::ir::ResolvedCall,
    ) -> Option<TypedTransitionEffectReceipt> {
        let ResolvedCallee::Transition(transition_id) = &call.callee else {
            return None;
        };
        if !self.allow_transition_receipts
            || !call.type_arguments.is_empty()
            || call.permission != Some(crate::core::ir::Permission::Consume)
            || !call.session.is_empty()
        {
            return None;
        }
        let transition = self.program.transitions().get(transition_id)?;
        let owner = super::transition_owner_from_id(transition_id);
        let target = self.program.callable(&owner)?;
        let site = self.program.call_sites().get(&expression.node_id)?;
        let call_site_effects = site.effects.iter().cloned().collect::<BTreeSet<_>>();
        if transition.node_id != owner
            || target.owner != owner
            || target.signature.owner != owner
            || target.body.owner != owner
            || target.signature.effects != call.effects
            || site.node_id != expression.node_id
            || site.resolved_callee.as_ref() != Some(&call.callee)
            // Flow transition syntax has no function-directory summary at
            // surface collection time. The checker records that exact
            // unresolved source call as Unknown, then typed lowering binds
            // this same call-site identity to the concrete transition. Only
            // the singleton checker fact can account for the caller's
            // conservative `unknown` summary.
            || site.kind != crate::core::ResolvedCallKind::Unknown
            || site.effects.as_slice() != ["unknown"]
            || call_site_effects != BTreeSet::from(["unknown".to_owned()])
        {
            return None;
        }
        // The Flow declaration currently has no effect summary of its own:
        // its caller receives the checker's `unknown` call-site effect. Do
        // not treat that as sufficient evidence that the transition body is
        // effect-free. Audit the exact checker-owned target body and admit
        // this exception only when its complete body has no modeled effects.
        // Nested transition receipts are disabled in this sub-audit to avoid
        // recursively treating another `unknown` edge as proof of purity.
        let mut target_audit = StdoutEffectAudit {
            program: self.program,
            modeled_effects: BTreeSet::new(),
            allow_transition_receipts: false,
            admitted_direct_stdout_callees: None,
            valid: true,
        };
        target_audit.block(&target.body.root);
        for value in target.body.place_inputs.values() {
            target_audit.expression(value);
        }
        for value in target.body.default_values.values() {
            target_audit.expression(value);
        }
        if !target_audit.valid || !target_audit.modeled_effects.is_empty() {
            return None;
        }
        Some(TypedTransitionEffectReceipt {
            target_effects: call
                .effects
                .iter()
                .map(|effect| effect.as_str().to_owned())
                .collect(),
            call_site_effects,
        })
    }

    fn call_is_effectful(&self, call: &crate::core::ir::ResolvedCall) -> bool {
        if !call.effects.is_empty() {
            return true;
        }
        match &call.callee {
            ResolvedCallee::Builtin(builtin) => {
                !crate::core::effects::builtin_effects(builtin.as_str()).is_empty()
            }
            ResolvedCallee::Function(owner) => self
                .program
                .callable(owner)
                .is_none_or(|target| !target.signature.effects.is_empty()),
            // Flow effects have their own typed transition receipt. Other
            // indirect/protocol/extern edges remain rejected by the general
            // request scanner independently of this narrow exception.
            ResolvedCallee::Transition(_)
            | ResolvedCallee::Constructor(_)
            | ResolvedCallee::Extern(_)
            | ResolvedCallee::LocalClosure(_)
            | ResolvedCallee::ActorMethod { .. }
            | ResolvedCallee::ProtocolMethod { .. } => false,
        }
    }

    fn direct_stdout_callee_effects(
        &self,
        expression: &ResolvedExpr,
        call: &crate::core::ir::ResolvedCall,
    ) -> Option<BTreeSet<String>> {
        let ResolvedCallee::Function(owner) = &call.callee else {
            return None;
        };
        if call.permission.is_some()
            || !call.type_arguments.is_empty()
            || !call.session.is_empty()
            || !self
                .admitted_direct_stdout_callees
                .is_some_and(|owners| owners.contains(owner))
        {
            return None;
        }
        let target = self.program.callable(owner)?;
        if !callable_effect_model_is_direct_stdout_only(target, self.program) {
            return None;
        }
        let expected = target
            .signature
            .effects
            .iter()
            .map(|effect| effect.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        let site = self.program.call_sites().get(&expression.node_id)?;
        let site_effects = site.effects.iter().cloned().collect::<BTreeSet<_>>();
        (site.node_id == expression.node_id
            && site.kind == crate::core::ResolvedCallKind::Function
            && site.resolved_callee.as_ref() == Some(&call.callee)
            && site_effects == expected
            && !expected.is_empty()
            && expected.iter().all(|effect| effect == STDOUT_EFFECT_NAME))
        .then_some(expected)
    }

    fn block(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    // SharedLet is desugared to an ordinary typed Bind, but
                    // the checker-owned origin tag preserves its source
                    // effect. It shares the broad `shared.modify` bucket with
                    // println, so it cannot be discharged by a stdout MIR
                    // receipt.
                    if matches!(
                        &pattern.origin,
                        Origin::Desugared { parent, rule, .. }
                            if parent == &statement.node_id
                                && rule == SHARED_BINDING_ORIGIN_RULE
                    ) || pattern.node_id
                        == NodeId(format!("{}/shared-pattern", statement.node_id.0))
                    {
                        self.valid = false;
                    }
                    if let Some(value) = initializer {
                        self.expression(value);
                    }
                }
                ResolvedStmtKind::Assign { value, .. }
                | ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => self.expression(value),
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.expression(value);
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    self.expression(condition);
                    self.block(body);
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                } => {
                    self.expression(initializer);
                    self.block(body);
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    self.expression(initializer);
                    self.block(then_block);
                    if let Some(else_block) = else_block {
                        self.block(else_block);
                    }
                }
                ResolvedStmtKind::Loop(body) => self.block(body),
                ResolvedStmtKind::For { iterable, body, .. } => {
                    self.expression(iterable);
                    self.block(body);
                }
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.expression(expression);
                    }
                }
                ResolvedStmtKind::Scope { body, .. } => self.block(body),
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    self.expression(value);
                    self.block(body);
                }
                ResolvedStmtKind::Drop(_)
                | ResolvedStmtKind::Continue
                | ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            self.expression(result);
        }
    }

    fn expression(&mut self, expression: &ResolvedExpr) {
        if !self.expression_effects_are_supported(expression) {
            self.valid = false;
        }
        match &expression.kind {
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        self.expression(value);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.expression(value);
                if let ResolvedValueProjection::Index(index) = projection {
                    self.expression(index);
                }
            }
            ResolvedExprKind::Binary { left, right, .. }
            | ResolvedExprKind::Range {
                start: left,
                end: right,
            } => {
                self.expression(left);
                self.expression(right);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::OptionalChain {
                receiver: operand, ..
            }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.expression(operand),
            ResolvedExprKind::Call(call) => {
                let is_println = matches!(&call.callee, ResolvedCallee::Builtin(builtin) if builtin.as_str() == "println");
                if matches!(call.callee, ResolvedCallee::Transition(_)) {
                    if let Some(receipt) = self.typed_transition_effects(expression, call) {
                        self.modeled_effects.extend(receipt.target_effects);
                        self.modeled_effects.extend(receipt.call_site_effects);
                    } else {
                        self.valid = false;
                    }
                } else if self.call_is_effectful(call) {
                    if is_println && self.is_exact_stdout_call(expression, call) {
                        self.modeled_effects.insert(STDOUT_EFFECT_NAME.into());
                    } else if let Some(callee_effects) =
                        self.direct_stdout_callee_effects(expression, call)
                    {
                        self.modeled_effects.extend(callee_effects);
                    } else {
                        self.valid = false;
                    }
                } else if is_println {
                    // A `println` call must carry its checker-issued finite
                    // effect fact; do not infer stdout permission from spelling.
                    self.valid = false;
                }
                for argument in &call.arguments {
                    self.expression(&argument.value);
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.expression(value);
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.expression(key);
                    self.expression(value);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.expression(iterable);
                if let Some(guard) = guard {
                    self.expression(guard);
                }
                self.expression(value);
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.expression(&field.value);
                }
                if let Some(rest) = rest {
                    self.expression(rest);
                }
            }
            ResolvedExprKind::Block(body)
            | ResolvedExprKind::Scope { body, .. }
            | ResolvedExprKind::Comptime(body)
            | ResolvedExprKind::Quote(body) => self.block(body),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expression(condition);
                self.block(then_block);
                self.block(else_block);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.expression(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expression(guard);
                    }
                    self.expression(&arm.body);
                }
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.expression(target);
                if let Some(start) = start {
                    self.expression(start);
                }
                if let Some(end) = end {
                    self.expression(end);
                }
            }
            ResolvedExprKind::Lambda(lambda) => self.block(&lambda.body),
        }
    }
}

fn boundary(owner: &NodeId, point: Option<&NodeId>, reason: &str) -> MirVerificationBoundary {
    MirVerificationBoundary {
        owner: owner.clone(),
        point: point.cloned(),
        reason: reason.into(),
    }
}

fn cyclic_owner(graph: &BTreeMap<NodeId, BTreeSet<NodeId>>) -> Option<NodeId> {
    let mut counts = BTreeMap::new();
    let mut callers = BTreeMap::<NodeId, Vec<NodeId>>::new();
    let mut ready = Vec::new();
    for (owner, targets) in graph {
        let mut count = 0;
        for target in targets.iter().filter(|target| graph.contains_key(*target)) {
            count += 1;
            callers
                .entry(target.clone())
                .or_default()
                .push(owner.clone());
        }
        counts.insert(owner.clone(), count);
        if count == 0 {
            ready.push(owner.clone());
        }
    }
    while let Some(owner) = ready.pop() {
        for caller in callers.get(&owner).into_iter().flatten() {
            let count = counts.get_mut(caller).expect("call graph node");
            *count -= 1;
            if *count == 0 {
                ready.push(caller.clone());
            }
        }
    }
    counts
        .into_iter()
        .find_map(|(owner, count)| (count != 0).then_some(owner))
}

fn has_exact_product_model(
    canonical: &MirProgram,
    ty: &crate::core::ResolvedTypeId,
    seen: &mut BTreeSet<crate::core::ResolvedTypeId>,
) -> bool {
    use super::types::{MirAbiClass, MirLayout};
    if !seen.insert(ty.clone()) {
        return false;
    }
    let result =
        canonical
            .type_catalog()
            .get(ty)
            .is_some_and(|descriptor| match &descriptor.layout {
                MirLayout::Unit => descriptor.is_canonical_ffi_unit(),
                MirLayout::Scalar => matches!(
                    descriptor.abi,
                    MirAbiClass::Integer {
                        bits: 32 | 64,
                        signed: true
                    } | MirAbiClass::Bool
                ),
                MirLayout::Tuple(fields) => fields
                    .iter()
                    .all(|field| has_exact_product_model(canonical, field, seen)),
                MirLayout::Record { fields, .. } => fields
                    .iter()
                    .all(|field| has_exact_product_model(canonical, &field.ty, seen)),
                _ => false,
            });
    seen.remove(ty);
    result
}

struct Scanner<'a> {
    owner: &'a NodeId,
    boundaries: &'a mut Vec<MirVerificationBoundary>,
    callees: BTreeSet<NodeId>,
    root_contracts: BTreeSet<NodeId>,
    proof_math_nodes: BTreeSet<NodeId>,
    proof_invariant_nodes: BTreeSet<NodeId>,
    proof_loop_nodes: BTreeSet<NodeId>,
}

impl Scanner<'_> {
    fn defer(&mut self, point: &NodeId, reason: &str) {
        self.boundaries
            .push(boundary(self.owner, Some(point), reason));
    }

    fn block(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Contract { condition, .. } => {
                    // Do not depend on a flattened census to identify proof
                    // scope: a missed embedded expression must stay deferred.
                    if !self.root_contracts.contains(&statement.node_id)
                        && !self.proof_invariant_nodes.contains(&statement.node_id)
                    {
                        self.defer(&statement.node_id, "nested contract needs its scoped program-point receipt; it is not an entry assumption");
                    }
                    self.expression(condition);
                }
                ResolvedStmtKind::Expr(condition) => self.expression(condition),
                ResolvedStmtKind::Bind {
                    initializer: Some(value),
                    ..
                } => self.expression(value),
                ResolvedStmtKind::Return { value, .. } => {
                    if let Some(value) = value {
                        self.expression(value);
                    }
                }
                ResolvedStmtKind::Scope {
                    kind: crate::core::ir::ResolvedScopeKind::Lexical,
                    body,
                } => self.block(body),
                ResolvedStmtKind::Drop(_) => {}
                ResolvedStmtKind::Math(expressions) => {
                    if !self.proof_math_nodes.contains(&statement.node_id) {
                        self.defer(
                            &statement.node_id,
                            "Math assertion is absent from the typed MIR proof-point census",
                        );
                    }
                    for expression in expressions {
                        self.expression(expression);
                    }
                }
                ResolvedStmtKind::Assign { .. } => self.defer(
                    &statement.node_id,
                    "assignment needs mutable invariant preservation and snapshot receipts",
                ),
                ResolvedStmtKind::While { condition, body } => {
                    if !self.proof_loop_nodes.contains(&statement.node_id) {
                        self.defer(
                            &statement.node_id,
                            "loop needs a typed stationary header invariant/math proof receipt",
                        );
                    }
                    self.expression(condition);
                    self.block(body);
                }
                ResolvedStmtKind::WhileLet { .. }
                | ResolvedStmtKind::Loop(_)
                | ResolvedStmtKind::For { .. } => self.defer(
                    &statement.node_id,
                    "loop needs establishment/preservation/exit induction obligations",
                ),
                _ => self.defer(
                    &statement.node_id,
                    "statement needs a canonical verification program-point/effect model",
                ),
            }
        }
        if let Some(value) = &block.result {
            self.expression(value);
        }
    }

    fn expression(&mut self, value: &ResolvedExpr) {
        match &value.kind {
            ResolvedExprKind::Literal(_) | ResolvedExprKind::Load(_) => {},
            ResolvedExprKind::Unary { operand, .. } | ResolvedExprKind::Old(operand) | ResolvedExprKind::Cast { value: operand, .. } | ResolvedExprKind::Try { value: operand, .. } => self.expression(operand),
            ResolvedExprKind::Binary { left, right, .. } => { self.expression(left); self.expression(right); },
            ResolvedExprKind::Project { value, projection } => {
                self.expression(value);
                if let ResolvedValueProjection::Index(index) = projection { self.expression(index); }
            }
            ResolvedExprKind::Call(call) => {
                match &call.callee {
                    ResolvedCallee::Function(owner) => { self.callees.insert(owner.clone()); },
                    ResolvedCallee::Transition(transition) => { self.callees.insert(super::transition_owner_from_id(transition)); },
                    ResolvedCallee::Builtin(_) | ResolvedCallee::Constructor(_) => {},
                    _ => self.defer(&value.node_id, "indirect/protocol/extern call needs its explicit typed summary/effect proof"),
                }
                for argument in &call.arguments { self.expression(&argument.value); }
            }
            ResolvedExprKind::Tuple(elements) | ResolvedExprKind::List(elements) | ResolvedExprKind::Set(elements) => {
                for element in elements { self.expression(element); }
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields { self.expression(&field.value); }
                if let Some(rest) = rest { self.expression(rest); }
            }
            ResolvedExprKind::Block(block) => self.block(block),
            ResolvedExprKind::If { condition, then_block, else_block } => {
                self.expression(condition); self.block(then_block); self.block(else_block);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.expression(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard { self.expression(guard); }
                    self.expression(&arm.body);
                }
            }
            _ => self.defer(&value.node_id, "expression needs a canonical proof/effect receipt before this request can retire its compatibility read"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{prepare_verification_request, MirVerificationPreparation};
    use crate::core::CheckedProgram;

    fn checked_program(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file).expect("check")
    }

    fn assert_deferred(source: &str, label: &str) {
        let checked = checked_program(source);
        let MirVerificationPreparation::Deferred(boundaries) =
            prepare_verification_request(&checked)
        else {
            panic!("{label} must remain outside the exact stdout proof model");
        };
        assert!(
            boundaries.iter().any(|boundary| boundary
                .reason
                .contains("generic, borrowed, captured, default, or effectful callable")),
            "{label} should retain the effectful-callable boundary: {boundaries:?}"
        );
    }

    #[test]
    fn admits_the_recoverable_cross_state_fixture_with_exact_println_effects() {
        let source = include_str!(
            "../../../tests/real_world/flow_state_match_fail_result_failure_dual_backend.mimi"
        );
        let checked = checked_program(source);
        let transition_sites = checked
            .call_sites()
            .values()
            .filter(|site| {
                matches!(
                    site.resolved_callee.as_ref(),
                    Some(crate::core::ir::ResolvedCallee::Transition(_))
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(transition_sites.len(), 1);
        assert_eq!(
            transition_sites[0].kind,
            crate::core::ResolvedCallKind::Unknown
        );
        assert_eq!(transition_sites[0].effects, vec!["unknown".to_owned()]);
        let main = checked
            .functions()
            .values()
            .find(|function| function.qualified_name == "main")
            .and_then(|function| checked.callable(&function.node_id))
            .expect("checked main callable");
        assert!(main
            .signature
            .effects
            .iter()
            .any(|effect| effect.as_str() == "unknown"));
        match prepare_verification_request(&checked) {
            MirVerificationPreparation::Materialized(_) => {}
            MirVerificationPreparation::Deferred(boundaries) => {
                panic!("recoverable Flow/println request should be typed: {boundaries:?}")
            }
            MirVerificationPreparation::NotRequested => {
                panic!("fixture contains a root contract and must request typed MIR")
            }
        }
    }

    #[test]
    fn effectful_non_stdout_calls_remain_deferred() {
        assert_deferred(
            r#"
func main() -> i32 {
    requires: true
    ensures: result == 0
    eprintln("diagnostic")
    0
}
"#,
            "eprintln sharing the shared.modify bucket",
        );

        assert_deferred(
            r#"
func main() -> i32 {
    requires: true
    ensures: result == 0
    let value = input()
    println(value)
    0
}
"#,
            "blocking input plus stdout",
        );

        assert_deferred(
            r#"
func main() -> i32 {
    requires: true
    ensures: result == 0
    match read_file("/tmp/mimi-verification-effect-boundary") {
        Ok(_) => 0,
        Err(_) => 0,
    }
}
"#,
            "file.read",
        );
    }

    #[test]
    fn shared_binding_effects_are_not_misattributed_to_stdout_or_flow() {
        assert_deferred(
            r#"
func main() -> i32 {
    requires: true
    ensures: result == 0
    shared retained = 42
    println(0)
    0
}
"#,
            "shared binding and stdout in the same effect bucket",
        );

        let source = include_str!(
            "../../../tests/real_world/flow_state_match_fail_result_failure_dual_backend.mimi"
        );
        assert!(source.contains("let result = F::go"));
        let source = source.replace(
            "let result = F::go",
            "shared retained = 42\n    let result = F::go",
        );
        assert_deferred(
            &source,
            "shared binding plus exact Flow transition and stdout in one caller",
        );

        let source = include_str!(
            "../../../tests/real_world/flow_state_match_fail_result_failure_dual_backend.mimi"
        );
        let source = source.replace(
            "transition go(A) -> B fails string {",
            "transition go(A) -> B fails string {\n        println(\"transition output\")",
        );
        assert_deferred(&source, "stdout effect in a typed Flow transition body");
    }

    #[test]
    fn effectful_user_function_call_is_not_a_direct_println_receipt() {
        assert_deferred(
            r#"
func emit() -> i32 {
    println(9)
    9
}

func main() -> i32 {
    requires: true
    ensures: result == 0
    emit()
    0
}
"#,
            "effectful user helper call",
        );
    }
}
