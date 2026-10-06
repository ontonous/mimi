//! Checker-owned coverage plan for contract verification requests.
//!
//! Execution islands are not the verification library's admission policy:
//! libraries need no `main`, and a Flow may have several states and guarded
//! transitions. This plan accounts for the complete user callable graph before
//! a verifier consumes it. It never retains a surface expression or invents a
//! body for an unsupported declaration. Deferred edges remain the responsibility
//! of the explicitly named compatibility owner at the public entry point.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::core::ir::{
    ResolvedBlock, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedStmtKind,
    ResolvedValueProjection,
};
use crate::core::{CheckedProgram, NodeId};

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
            || !callable.signature.effects.is_empty()
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
    for block in program.extern_blocks().values() {
        for declaration in &block.signatures {
            boundaries.push(boundary(&declaration.node_id, None, "extern declaration obligations stay with the existing FFI/Flow owners until declaration proofs are part of this request"));
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
