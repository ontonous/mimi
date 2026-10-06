//! Checked FFI compatibility proofs without reopening a retained body File.
//!
//! Call identities/arguments are discovered exhaustively from typed bodies and
//! declaration roots. Requires predicates have their own checker-produced typed
//! roots. Complete directory coverage is checked independently of proof success;
//! a SolverUnknown result never causes an AST retry.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;
use z3::ast::{Bool, Float, Int, String as Z3String};
use z3::SatResult;

use super::{ProofArtifact, SolverSession, TrustedSubsetDomain, VerifStatus, VerificationResult};
use crate::core::ir::lower::ResolvedDeclarationExpressionKind;
use crate::core::ir::*;
use crate::core::{CheckedProgram, NodeId, ResolvedCallKind, ResolvedTypeDef, ResolvedTypeKind};

type Env = BTreeMap<ResolvedLocalId, Term>;

#[derive(Clone)]
enum Value {
    Int(Int),
    Bool(Bool),
    Float(Float),
    String(Z3String),
    Record {
        nominal: NominalTypeId,
        fields: BTreeMap<NodeId, Term>,
    },
}

#[derive(Clone)]
struct Term {
    value: Value,
    defined: Bool,
}

impl Term {
    fn total(value: Value) -> Self {
        Self {
            value,
            defined: Bool::from_bool(true),
        }
    }
    fn bool(&self) -> Option<Bool> {
        if let Value::Bool(value) = &self.value {
            Some(value.clone())
        } else {
            None
        }
    }
}

struct Site<'a> {
    expression: &'a ResolvedExpr,
    call: &'a ResolvedCall,
    body: &'a ResolvedBody,
    caller: String,
    path: PathContext<'a>,
}

/// A default proof is admissible here only when the exact checker-issued
/// default root is used by at least one validated omitted-argument receipt.
/// The root's own local-only profile is deliberately narrower than the FFI
/// caller context: no caller Requires or foreign Ensures become default facts.
struct DefaultMathPlan<'a> {
    callable: &'a ResolvedCallable,
    root: TypedDefaultProofRoot,
    uses: Vec<TypedDefaultProofUse>,
}

#[derive(Clone, Copy)]
struct PathGuard<'a> {
    predicate: PathPredicate<'a>,
    expected: bool,
}

#[derive(Clone, Copy)]
enum PathPredicate<'a> {
    Expression(&'a ResolvedExpr),
    LiteralPattern {
        scrutinee: &'a ResolvedExpr,
        pattern: &'a ResolvedPattern,
    },
}

impl PathGuard<'_> {
    fn evaluate(&self, program: &CheckedProgram, env: &Env) -> Option<Term> {
        match self.predicate {
            PathPredicate::Expression(condition) => eval(condition, program, env),
            PathPredicate::LiteralPattern { scrutinee, pattern } => {
                eval_literal_pattern(scrutinee, pattern, program, env)
            }
        }
    }
}

#[derive(Clone)]
struct PathContext<'a> {
    assumes_requires: bool,
    guards: Vec<PathGuard<'a>>,
    bindings: Vec<(ResolvedLocalId, &'a ResolvedExpr)>,
    complete: bool,
}

impl<'a> PathContext<'a> {
    fn entry(assumes_requires: bool) -> Self {
        Self {
            assumes_requires,
            guards: Vec::new(),
            bindings: Vec::new(),
            complete: true,
        }
    }

    fn with_guard(&self, condition: &'a ResolvedExpr, expected: bool) -> Self {
        let mut next = self.clone();
        next.guards.push(PathGuard {
            predicate: PathPredicate::Expression(condition),
            expected,
        });
        next
    }

    fn with_pattern_guard(
        &self,
        scrutinee: &'a ResolvedExpr,
        pattern: &'a ResolvedPattern,
        expected: bool,
    ) -> Self {
        let mut next = self.clone();
        next.guards.push(PathGuard {
            predicate: PathPredicate::LiteralPattern { scrutinee, pattern },
            expected,
        });
        next
    }

    fn with_binding(&self, local: &ResolvedLocalId, value: &'a ResolvedExpr) -> Self {
        let mut next = self.clone();
        next.bindings.push((local.clone(), value));
        next
    }

    fn incomplete(&self) -> Self {
        let mut next = self.clone();
        next.complete = false;
        next
    }
}

/// `None` is an explicit unmaterialized declaration or call-context boundary
/// for this request. Named deferred proof roots are emitted by this consumer
/// and do not force a whole-request retry; once a called declaration and every
/// site are encoded, results (including Unknown) are final and never re-enter
/// the legacy body consumer.
pub(super) fn verify(
    program: &CheckedProgram,
    requires_names: &BTreeSet<String>,
    source_hash: &str,
) -> Result<Option<Vec<VerificationResult>>, String> {
    if requires_names.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let default_math_plans = default_math_plans(program, requires_names);
    // Nested/default/place-input proof roots are independent obligations.  The
    // old compatibility owner used their presence as a whole-request retry
    // trigger, which reopened the retained AST body even when every FFI call
    // itself had a complete typed context.  Keep the named deferred results in
    // this checker-owned consumer and continue proving the foreign call sites
    // from their receipts.  A genuinely unsupported call expression/path still
    // returns `None` at its precise boundary below; this profile does not turn
    // unsupported encoding into a proof.
    let deferred_context = deferred_context_results(program, requires_names);
    let mut declarations = BTreeMap::new();
    for declaration in program.ffi_declarations().values() {
        if !requires_names.contains(&declaration.symbol) {
            continue;
        }
        declaration
            .validate(program)
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        // The typed compatibility proof currently claims the pre-call
        // Requires predicate only for C declarations. Preserve the existing
        // declaration-boundary error by deferring other ABIs to the legacy
        // boundary checker, rather than proving them through this route.
        if declaration.abi != "C" {
            return Ok(None);
        }
        if declaration.signature.is_err() {
            return Ok(None);
        }
        let body = match &declaration.requires {
            ResolvedFfiContractRoot::Absent => {
                return Err(format!(
                    "TOOL-RESOLUTION-001: called extern '{}' has no checked Requires root",
                    declaration.symbol
                ));
            }
            ResolvedFfiContractRoot::Deferred { .. } => return Ok(None),
            ResolvedFfiContractRoot::Ready { node_id } => {
                let root = program
                    .declaration_expression_bodies()
                    .get(node_id)
                    .ok_or_else(|| {
                        format!(
                            "TOOL-RESOLUTION-001: typed extern Requires root '{}' is absent",
                            node_id.0
                        )
                    })?;
                if &root.node_id != node_id
                    || root.owner != declaration.declaration
                    || root.kind != ResolvedDeclarationExpressionKind::ExternRequires
                {
                    return Err(format!(
                        "TOOL-RESOLUTION-001: typed extern Requires root '{}' has inconsistent declaration identity",
                        node_id.0
                    ));
                }
                match &root.body {
                    Ok(body) => body,
                    Err(_) => return Ok(None),
                }
            }
        };
        if declarations
            .insert(declaration.declaration.clone(), (declaration, body))
            .is_some()
        {
            return Err(format!(
                "TOOL-RESOLUTION-001: duplicate typed extern Requires descriptor '{}'",
                declaration.declaration.0
            ));
        }
    }
    for name in requires_names {
        let matches = program
            .ffi_declarations()
            .values()
            .filter(|declaration| declaration.symbol == *name)
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(format!(
                "TOOL-RESOLUTION-001: missing typed extern declaration '{name}'"
            ));
        }
        if matches.len() != 1 {
            return Err(format!(
                "TOOL-RESOLUTION-001: typed extern symbol '{name}' does not resolve uniquely"
            ));
        }
        if !declarations.contains_key(&matches[0].declaration) {
            return Ok(None);
        }
    }
    let expected = program
        .call_sites_sorted()
        .into_iter()
        .filter(|site| {
            site.kind == ResolvedCallKind::Extern && requires_names.contains(&site.callee)
        })
        .map(|site| site.node_id.clone())
        .collect::<BTreeSet<_>>();
    let mut sites = Vec::new();
    let mut seen = BTreeSet::new();
    for body in program.resolved_bodies().values() {
        let caller = caller_name(program, &body.owner);
        visit_block(
            &body.root,
            body,
            &caller,
            &PathContext::entry(true),
            &mut seen,
            &mut sites,
        );
        for (parameter, value) in &body.default_values {
            let name = program
                .resolved_signature(&body.owner)
                .and_then(|signature| {
                    signature
                        .parameters
                        .iter()
                        .find(|candidate| candidate.id == *parameter)
                })
                .map(|parameter| parameter.name.as_str())
                .unwrap_or("<unknown>");
            visit_expr(
                value,
                body,
                &format!("{caller} default for {name}"),
                &PathContext::entry(false),
                &mut seen,
                &mut sites,
            );
        }
    }
    for root in program.declaration_expression_bodies().values() {
        if let Ok(body) = &root.body {
            visit_block(
                &body.root,
                body,
                &root.display_name,
                &PathContext::entry(false),
                &mut seen,
                &mut sites,
            );
        }
    }
    let mut covered = BTreeSet::new();
    sites.retain(|site| matches!(&site.call.callee, ResolvedCallee::Extern(declaration) if declarations.contains_key(declaration)));
    for site in &sites {
        let Some(directory) = program.call_sites().get(&site.expression.node_id) else {
            return Err(format!("TOOL-RESOLUTION-001: typed extern call '{}' is absent from the call-site directory", site.expression.node_id.0));
        };
        let ResolvedCallee::Extern(declaration) = &site.call.callee else {
            return Err(format!(
                "TOOL-RESOLUTION-001: typed FFI call '{}' has no extern declaration identity",
                site.expression.node_id.0
            ));
        };
        let (checked_declaration, _) = declarations.get(declaration).ok_or_else(|| {
            format!(
                "TOOL-RESOLUTION-001: typed extern callee '{}' has no checked descriptor",
                declaration.0
            )
        })?;
        let typed_context = checked_declaration
            .call_context(program, site.body, site.expression)
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        // Extern call lowering currently records an identity input snapshot,
        // not a target-side conversion. Prove only when the actual checked
        // expression already has the declaration's canonical parameter type;
        // other admitted conversions remain on the compatibility path until
        // the checker emits a call-bound conversion receipt.
        if typed_context
            .inputs
            .iter()
            .any(|input| input.argument.value.ty != input.parameter.ty)
        {
            return Ok(None);
        }
        if directory.kind != ResolvedCallKind::Extern
            || directory.owner != site.body.owner.0
            || directory.callee != checked_declaration.symbol
            || directory.argc != site.call.arguments.len()
            || typed_context.inputs.len() != site.call.arguments.len()
        {
            return Err(format!("TOOL-RESOLUTION-001: typed extern call '{}' disagrees with checked owner, callee or arity", directory.node_id.0));
        }
        if !covered.insert(directory.node_id.clone()) {
            return Err(format!(
                "TOOL-RESOLUTION-001: typed extern call '{}' was discovered more than once",
                directory.node_id.0
            ));
        }
        // Input predicates are snapshots at entry. A body that mutates a
        // parameter needs the separate SSA/effect context migration; preserve
        // that explicit compatibility face instead of proving with stale data.
        if mutates_parameter(&site.body.root, &site.body.parameters) {
            return Ok(None);
        }
    }
    if covered != expected {
        return Ok(None);
    }

    // The finite migrated encoding keeps every historically supported richer
    // predicate on the explicit compatibility boundary. Coverage alone cannot
    // authorize a weaker proof algorithm for unsupported leaves or shapes.
    for (_, contract) in declarations.values() {
        if contract
            .root
            .result
            .as_ref()
            .and_then(|condition| eval(condition, program, &parameter_env(contract, program)))
            .is_none()
        {
            return Ok(None);
        }
    }
    for site in &sites {
        // Only certify contexts with a complete branch model. Other control-flow
        // shapes remain on the explicit compatibility boundary before proof.
        if !site.path.complete {
            return Ok(None);
        }
        let Some(env) = site_env(site.body, &site.path, program) else {
            return Ok(None);
        };
        if site.path.guards.iter().any(|guard| {
            guard
                .evaluate(program, &env)
                .and_then(|term| term.bool())
                .is_none()
        }) {
            return Ok(None);
        }
        // A declaration predicate can be fully typed while a call argument
        // still needs the richer compatibility context (for example an if or
        // match value). Classify that shape before proof begins so its prior
        // positive proof remains on the explicit Deferred boundary. Solver
        // Unknown after complete encoding remains final and never retries.
        if site
            .call
            .arguments
            .iter()
            .any(|argument| eval(&argument.value, program, &env).is_none())
        {
            return Ok(None);
        }
        if site.path.assumes_requires {
            for statement in &site.body.root.statements {
                if let ResolvedStmtKind::Contract {
                    kind: ContractKind::Requires,
                    condition,
                } = &statement.kind
                {
                    if eval(condition, program, &env).is_none() {
                        return Ok(None);
                    }
                }
            }
        }
    }

    let mut hash = blake3::Hasher::new();
    hash.update(b"mimi-typed-ffi-contract-context-v1\n");
    for body in program.resolved_bodies().values() {
        hash.update(format!("{body:?}\n").as_bytes());
    }
    for root in program.declaration_expression_bodies().values() {
        hash.update(format!("{root:?}\n").as_bytes());
    }
    for (declaration_id, (declaration, body)) in &declarations {
        hash.update(format!("{declaration_id:?}\n{declaration:?}\n{body:?}\n").as_bytes());
    }
    let digest = hash.finalize().to_hex().to_string();
    let mut session = SolverSession::new(super::ctx::DEFAULT_TIMEOUT_MS)?;
    let mut results = Vec::with_capacity(sites.len());
    for site in sites {
        let ResolvedCallee::Extern(declaration) = &site.call.callee else {
            return Err(format!(
                "TOOL-RESOLUTION-001: typed FFI call '{}' has no extern declaration identity",
                site.expression.node_id.0
            ));
        };
        let (checked_declaration, _) = declarations.get(declaration).ok_or_else(|| {
            format!(
                "TOOL-RESOLUTION-001: typed extern callee '{}' has no Requires root during proof",
                declaration.0
            )
        })?;
        let typed_context = checked_declaration
            .call_context(program, site.body, site.expression)
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        let contract = match typed_context.requires {
            Ok(Some(contract)) => contract,
            Ok(None) => {
                return Err(format!(
                    "TOOL-RESOLUTION-001: typed extern callee '{}' has no materialized Requires root during proof",
                    checked_declaration.symbol
                ));
            }
            Err(errors) => return Err(format!("TOOL-RESOLUTION-001: {errors:?}")),
        };
        let start = Instant::now();
        session.push();
        let env = site_env(site.body, &site.path, program).ok_or_else(|| {
            "TOOL-RESOLUTION-001: typed FFI site lost its checked local context".to_string()
        })?;
        assert_parameter_domain(&mut session, site.body, program, &env)?;
        let mut failure = None;
        if site.path.assumes_requires {
            for statement in &site.body.root.statements {
                if let ResolvedStmtKind::Contract {
                    kind: ContractKind::Requires,
                    condition,
                } = &statement.kind
                {
                    let Some(term) = eval(condition, program, &env) else {
                        failure = Some((
                            VerifStatus::NotInTrustedSubset,
                            "caller Requires is outside the typed FFI contract subset",
                        ));
                        break;
                    };
                    let Some(predicate) = term.bool() else {
                        failure = Some((
                            VerifStatus::NotInTrustedSubset,
                            "caller Requires has no Boolean encoding",
                        ));
                        break;
                    };
                    match session.check_scope(term.defined.not()).0 {
                        SatResult::Unsat => session.assert(predicate),
                        SatResult::Sat => {
                            failure = Some((
                                VerifStatus::Disproven,
                                "caller Requires can trap before the FFI call",
                            ));
                            break;
                        }
                        SatResult::Unknown => {
                            failure = Some((
                                VerifStatus::SolverUnknown,
                                "caller Requires definedness is inconclusive",
                            ));
                            break;
                        }
                    }
                }
            }
        }
        for (_, binding) in &site.path.bindings {
            if failure.is_some() {
                break;
            }
            let Some(term) = eval(binding, program, &env) else {
                failure = Some((
                    VerifStatus::NotInTrustedSubset,
                    "caller iteration element is outside the typed FFI contract subset",
                ));
                break;
            };
            match session.check_scope(term.defined.not()).0 {
                SatResult::Unsat => {}
                SatResult::Sat => {
                    failure = Some((
                        VerifStatus::Disproven,
                        "caller iteration element can trap before the FFI call",
                    ));
                    break;
                }
                SatResult::Unknown => {
                    failure = Some((
                        VerifStatus::SolverUnknown,
                        "caller iteration element definedness is inconclusive",
                    ));
                    break;
                }
            }
        }
        for guard in &site.path.guards {
            if failure.is_some() {
                break;
            }
            let Some(term) = guard.evaluate(program, &env) else {
                failure = Some((
                    VerifStatus::NotInTrustedSubset,
                    "caller path condition is outside the typed FFI contract subset",
                ));
                break;
            };
            let Some(predicate) = term.bool() else {
                failure = Some((
                    VerifStatus::NotInTrustedSubset,
                    "caller path condition has no Boolean encoding",
                ));
                break;
            };
            match session.check_scope(term.defined.not()).0 {
                SatResult::Unsat => {
                    if guard.expected {
                        session.assert(predicate);
                    } else {
                        session.assert(predicate.not());
                    }
                }
                SatResult::Sat => {
                    failure = Some((
                        VerifStatus::Disproven,
                        "caller path condition can trap before the FFI call",
                    ));
                    break;
                }
                SatResult::Unknown => {
                    failure = Some((
                        VerifStatus::SolverUnknown,
                        "caller path condition definedness is inconclusive",
                    ));
                    break;
                }
            }
        }
        let mut arguments = Env::new();
        let mut argument_definedness = Vec::new();
        let mut arguments_complete = true;
        for (parameter, input) in contract.parameters.iter().zip(&typed_context.inputs) {
            if let Some(term) = eval(&input.argument.value, program, &env) {
                // Entry parameter domains are asserted from their canonical
                // caller symbols above. Do not constrain a computed actual back
                // into its declared result domain here: that would assume away
                // overflow/trap witnesses which `term.defined` must expose.
                argument_definedness.push(term.defined.clone());
                arguments.insert(parameter.clone(), term);
            } else {
                arguments_complete = false;
            }
        }
        let argument_eval_definedness = argument_definedness.clone();
        let predicate = contract
            .root
            .result
            .as_ref()
            .and_then(|condition| eval(condition, program, &arguments));
        let call_requires_assumption = predicate.as_ref().and_then(Term::bool);
        let (status, message, constraints) = if let Some((status, message)) = failure {
            (status, message, 0)
        } else if !arguments_complete {
            (
                VerifStatus::SolverUnknown,
                "typed extern argument definedness is outside the current encoding",
                1,
            )
        } else if let Some(term) = predicate {
            if let Some(predicate) = term.bool() {
                argument_definedness.push(term.defined);
                argument_definedness.push(predicate);
                let obligation = Bool::and(&argument_definedness);
                match session.check_scope(obligation.not()).0 {
                    SatResult::Unsat => (
                        VerifStatus::Proven,
                        "typed extern precondition always satisfied",
                        1,
                    ),
                    SatResult::Sat => (
                        VerifStatus::Disproven,
                        "typed extern precondition may be violated or undefined",
                        1,
                    ),
                    SatResult::Unknown => (
                        VerifStatus::SolverUnknown,
                        "typed extern precondition satisfiability unknown",
                        1,
                    ),
                }
            } else {
                (
                    VerifStatus::SolverUnknown,
                    "typed extern precondition has no Boolean encoding",
                    1,
                )
            }
        } else {
            (
                VerifStatus::SolverUnknown,
                "typed extern precondition or argument is outside the current encoding",
                1,
            )
        };
        if status == VerifStatus::Proven {
            if let Some(predicate) = call_requires_assumption {
                // Once independently proven for this call, the foreign
                // Requires predicate is a valid precondition for checking the
                // declared postcondition. It is scoped to this call only.
                session.assert(predicate);
            }
        }
        let mut artifact =
            ProofArtifact::new(z3::full_version().to_string(), source_hash.to_string());
        artifact.engine = "resolved_ffi".into();
        artifact.integer_model = "checked".into();
        artifact.resolved_ir_hash = digest.clone();
        let diagnostic = (status == VerifStatus::Disproven).then(|| {
            crate::diagnostic::Diagnostic::error(
                format!(
                    "call to extern '{}' may violate its precondition",
                    checked_declaration.symbol
                ),
                site.expression.origin.user_span(),
            )
        });
        results.push(VerificationResult {
            func_name: format!("{} calls {}", site.caller, checked_declaration.symbol),
            status: status.clone(),
            message: message.into(),
            diagnostic,
            duration_us: start.elapsed().as_micros() as u64,
            constraint_count: constraints,
            artifact: Some(artifact),
            trusted_subset_domain: (status == VerifStatus::NotInTrustedSubset)
                .then_some(TrustedSubsetDomain::Contract),
        });

        // A Ready foreign Ensures clause is a separate obligation over a
        // fresh, call-identity-bound result. It is never asserted into the
        // caller's continuation: this verifier has no checked external
        // implementation/effect authority that could justify such a fact.
        // The input environment below is built from the same validated typed
        // call context as Requires. On this base that context contains only
        // explicit fixed actuals; it cannot represent an omitted extern
        // parameter default, which remains a producer boundary below.
        match typed_context.ensures {
            Ok(Some(ensures_body)) => {
                let ensures_name = format!(
                    "{} -> extern {} Ensures",
                    site.caller, checked_declaration.symbol
                );
                let start = Instant::now();
                let mut ensures_env = Env::new();
                let mut failure = None;
                if ensures_body.parameters.len() != typed_context.inputs.len() {
                    failure = Some((
                        VerifStatus::NotInTrustedSubset,
                        "typed foreign Ensures inputs do not cover the canonical fixed parameters",
                    ));
                } else {
                    for (parameter, input) in
                        ensures_body.parameters.iter().zip(&typed_context.inputs)
                    {
                        match eval(&input.argument.value, program, &env) {
                            Some(term) => {
                                ensures_env.insert(parameter.clone(), term);
                            }
                            None => {
                                failure = Some((
                                    VerifStatus::NotInTrustedSubset,
                                    "typed foreign Ensures input is outside the current encoding",
                                ));
                                break;
                            }
                        }
                    }
                }
                let result_type = typed_context
                    .declaration
                    .signature
                    .as_ref()
                    .ok()
                    .map(|signature| &signature.result);
                let fresh_result = result_type.and_then(|ty| {
                    parameter_term(
                        program,
                        ty,
                        &format!(
                            "ffi-result:{}:{}",
                            typed_context.declaration.declaration.0, typed_context.call_node.0
                        ),
                        &mut BTreeSet::new(),
                    )
                });
                match (result_type, fresh_result) {
                    (Some(result_type), Some(result)) => {
                        assert_term_domain(&mut session, program, &result, result_type)?;
                        ensures_env.insert(typed_context.contract_result.clone(), result);
                    }
                    _ if failure.is_none() => {
                        failure = Some((
                            VerifStatus::NotInTrustedSubset,
                            "foreign result type is outside the fresh-result encoding",
                        ));
                    }
                    _ => {}
                }

                let mut obligation_definedness = argument_eval_definedness;
                let obligation = ensures_body
                    .root
                    .result
                    .as_ref()
                    .and_then(|condition| eval(condition, program, &ensures_env));
                let (ensures_status, ensures_message, constraint_count) =
                    if let Some((status, message)) = failure {
                        (status, message, 0)
                    } else if !site.path.complete {
                        (
                            VerifStatus::NotInTrustedSubset,
                            "foreign Ensures call path is outside the complete typed context",
                            0,
                        )
                    } else if let Some(term) = obligation {
                        if let Some(predicate) = term.bool() {
                            obligation_definedness.push(term.defined);
                            let mut violation_terms = obligation_definedness
                                .iter()
                                .map(|defined| defined.not())
                                .collect::<Vec<_>>();
                            violation_terms.push(predicate.not());
                            let violation = Bool::or(&violation_terms);
                            match session.check_scope(violation).0 {
                                SatResult::Unsat => (
                                    VerifStatus::Proven,
                                    "foreign Ensures obligation holds for every encoded result",
                                    1,
                                ),
                                SatResult::Sat => (
                                    VerifStatus::Disproven,
                                    "foreign Ensures may be false for a fresh result",
                                    1,
                                ),
                                SatResult::Unknown => (
                                    VerifStatus::SolverUnknown,
                                    "foreign Ensures obligation is inconclusive",
                                    1,
                                ),
                            }
                        } else {
                            (
                                VerifStatus::NotInTrustedSubset,
                                "foreign Ensures root has no Boolean encoding",
                                0,
                            )
                        }
                    } else {
                        (
                            VerifStatus::NotInTrustedSubset,
                            "foreign Ensures root is outside the current encoding",
                            0,
                        )
                    };
                let mut artifact =
                    ProofArtifact::new(z3::full_version().to_string(), source_hash.to_string());
                artifact.engine = "resolved_ffi".into();
                artifact.integer_model = "checked".into();
                artifact.resolved_ir_hash = digest.clone();
                let diagnostic = (ensures_status == VerifStatus::Disproven).then(|| {
                    crate::diagnostic::Diagnostic::error(
                        format!(
                            "foreign Ensures for '{}' is not valid for every fresh call result",
                            checked_declaration.symbol
                        ),
                        site.expression.origin.user_span(),
                    )
                });
                results.push(VerificationResult {
                    func_name: ensures_name,
                    status: ensures_status.clone(),
                    message: ensures_message.into(),
                    diagnostic,
                    duration_us: start.elapsed().as_micros() as u64,
                    constraint_count,
                    artifact: (ensures_status == VerifStatus::Proven
                        || ensures_status == VerifStatus::Disproven)
                        .then_some(artifact),
                    trusted_subset_domain: (ensures_status == VerifStatus::NotInTrustedSubset)
                        .then_some(TrustedSubsetDomain::Contract),
                });
            }
            Ok(None) => {}
            Err(_) => {
                results.push(VerificationResult {
                    func_name: format!(
                        "{} -> extern {} Ensures",
                        site.caller, checked_declaration.symbol
                    ),
                    status: VerifStatus::NotInTrustedSubset,
                    message: "typed foreign Ensures root is Deferred and has no proof artifact"
                        .into(),
                    diagnostic: None,
                    duration_us: 0,
                    constraint_count: 0,
                    artifact: None,
                    trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
                });
            }
        }
        session.pop();
    }
    for plan in &default_math_plans {
        let mut default_results = Vec::new();
        session.push();
        let replay = replay_default_math_block(
            &plan.root.root,
            plan.callable,
            program,
            &mut Env::new(),
            &mut session,
            &digest,
            source_hash,
            &plan.uses,
            &mut default_results,
        );
        session.pop();
        if !replay {
            // The structural preflight below is intended to make this
            // unreachable. If an encoder and the receipt profile ever drift,
            // do not turn the mismatch into an empty successful result.
            return Err(format!(
                "TOOL-RESOLUTION-001: checked default proof root '{}' changed during typed replay",
                plan.root.expression.0
            ));
        }
        results.extend(default_results);
    }
    results.extend(deferred_context);
    Ok(Some(results))
}

/// Build plans from the checker-owned per-parameter default roots. The
/// complete worklist and every use receipt are rederived against their actual
/// caller/target callables; matching a NodeId or source name alone is never
/// enough to make a default executable.
fn default_math_plans<'a>(
    program: &'a CheckedProgram,
    requires_names: &BTreeSet<String>,
) -> Vec<DefaultMathPlan<'a>> {
    let ffi_owners = program
        .call_sites_sorted()
        .into_iter()
        .filter(|site| {
            site.kind == ResolvedCallKind::Extern && requires_names.contains(&site.callee)
        })
        .map(|site| NodeId(site.owner.clone()))
        .collect::<BTreeSet<_>>();
    let mut plans = Vec::new();
    for owner in ffi_owners {
        let Some(callable) = program.callable(&owner) else {
            continue;
        };
        let worklist = callable.proof_point_worklist();
        if !worklist.validate_against(callable) {
            continue;
        }
        for root in &worklist.default_roots {
            if root.points.is_empty()
                || !root.validate_in_checked_program(program)
                || !root.supports_local_only_profile(callable)
                || !default_root_is_encodable(root, callable, program)
            {
                continue;
            }
            let Some(uses) = checked_default_uses(program, &root.owner, &root.parameter) else {
                continue;
            };
            plans.push(DefaultMathPlan {
                callable,
                root: root.clone(),
                uses,
            });
        }
    }
    plans
}

fn checked_default_uses(
    program: &CheckedProgram,
    callee_owner: &NodeId,
    parameter: &ResolvedParameterId,
) -> Option<Vec<TypedDefaultProofUse>> {
    let mut uses = Vec::new();
    for caller in program.callables().values() {
        let worklist = caller.proof_point_worklist();
        let matching = worklist
            .default_uses
            .iter()
            .filter(|usage| &usage.callee_owner == callee_owner && &usage.parameter == parameter)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            continue;
        }
        if !worklist.validate_against(caller) {
            return None;
        }
        for usage in matching {
            if !usage.validate_in_checked_program(program)
                || !matches!(
                    &usage.disposition,
                    TypedCallExecutionDisposition::Unconditional
                        | TypedCallExecutionDisposition::Guarded
                )
            {
                return None;
            }
            uses.push(usage.clone());
        }
    }
    (!uses.is_empty()).then_some(uses)
}

/// Evaluate the exact local-only receipt shape once without making solver
/// assumptions. This preflight ensures the later solver replay cannot encounter
/// an unencoded leaf after the root has been removed from the Deferred census.
fn default_root_is_encodable(
    root: &TypedDefaultProofRoot,
    callable: &ResolvedCallable,
    program: &CheckedProgram,
) -> bool {
    fn walk(
        block: &TypedProofBlock,
        callable: &ResolvedCallable,
        program: &CheckedProgram,
        env: &mut Env,
    ) -> bool {
        for operation in &block.operations {
            match operation {
                TypedProofOperation::Bind {
                    local, initializer, ..
                } => {
                    let Some(local_info) = callable.body.locals.get(local) else {
                        return false;
                    };
                    if local_info.mutable
                        || !matches!(&initializer.kind, ResolvedExprKind::Literal(_))
                    {
                        return false;
                    }
                    let Some(term) = eval(initializer, program, env) else {
                        return false;
                    };
                    if !default_scalar_type_supported(program, &local_info.ty, &term)
                        || env.insert(local.clone(), term).is_some()
                    {
                        return false;
                    }
                }
                TypedProofOperation::Math(point) => {
                    if eval(&point.condition, program, env)
                        .and_then(|term| term.bool().map(|_| term))
                        .is_none()
                    {
                        return false;
                    }
                }
                TypedProofOperation::Eval { expression, .. } => {
                    if eval(expression, program, env).is_none() {
                        return false;
                    }
                }
                TypedProofOperation::Scope(inner) => {
                    let mut nested = env.clone();
                    if !walk(inner, callable, program, &mut nested) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }

    root.validate_in_checked_program(program)
        && root.supports_local_only_profile(callable)
        && walk(&root.root, callable, program, &mut Env::new())
}

fn default_scalar_type_supported(
    program: &CheckedProgram,
    ty: &ResolvedTypeId,
    term: &Term,
) -> bool {
    match (&term.value, canonical_primitive(program, ty)) {
        (Value::Bool(_), Some(PrimitiveType::Bool))
        | (Value::String(_), Some(PrimitiveType::String))
        | (Value::Float(_), Some(PrimitiveType::F64)) => true,
        (Value::Int(_), Some(primitive)) => integer_parameter_bounds(primitive).is_some(),
        _ => false,
    }
}

fn replay_default_math_block(
    block: &TypedProofBlock,
    callable: &ResolvedCallable,
    program: &CheckedProgram,
    env: &mut Env,
    session: &mut SolverSession,
    context_digest: &str,
    source_hash: &str,
    uses: &[TypedDefaultProofUse],
    results: &mut Vec<VerificationResult>,
) -> bool {
    for operation in &block.operations {
        match operation {
            TypedProofOperation::Bind {
                point,
                local,
                initializer,
            } => {
                let Some(local_info) = callable.body.locals.get(local) else {
                    return false;
                };
                let Some(term) = eval(initializer, program, env) else {
                    return false;
                };
                if !default_scalar_type_supported(program, &local_info.ty, &term)
                    || assert_term_domain(session, program, &term, &local_info.ty).is_err()
                {
                    return false;
                }
                if !append_default_definedness_result(
                    callable,
                    &point.0,
                    &term.defined,
                    initializer.origin.user_span(),
                    "default binding initializer",
                    session,
                    context_digest,
                    source_hash,
                    uses,
                    results,
                ) {
                    return false;
                }
                env.insert(local.clone(), term);
            }
            TypedProofOperation::Math(point) => {
                let Some(term) = eval(&point.condition, program, env) else {
                    return false;
                };
                let Some(predicate) = term.bool() else {
                    return false;
                };
                let result_index = results.len();
                let point_key = format!("{}#math:{}", point.id.statement.0, point.id.ordinal);
                if !append_default_definedness_result(
                    callable,
                    &point_key,
                    &Bool::and(&[&term.defined, &predicate]),
                    point.condition.origin.user_span(),
                    "default Math condition",
                    session,
                    context_digest,
                    source_hash,
                    uses,
                    results,
                ) {
                    return false;
                }
                if results
                    .get(result_index)
                    .is_some_and(|result| result.status == VerifStatus::Proven)
                {
                    session.assert(predicate);
                }
            }
            TypedProofOperation::Eval { point, expression } => {
                let Some(term) = eval(expression, program, env) else {
                    return false;
                };
                if !append_default_definedness_result(
                    callable,
                    &point.0,
                    &term.defined,
                    expression.origin.user_span(),
                    "default expression evaluation",
                    session,
                    context_digest,
                    source_hash,
                    uses,
                    results,
                ) {
                    return false;
                }
            }
            TypedProofOperation::Scope(inner) => {
                let mut nested_env = env.clone();
                session.push();
                let nested_ok = replay_default_math_block(
                    inner,
                    callable,
                    program,
                    &mut nested_env,
                    session,
                    context_digest,
                    source_hash,
                    uses,
                    results,
                );
                session.pop();
                if !nested_ok {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

fn append_default_definedness_result(
    callable: &ResolvedCallable,
    point: &str,
    obligation: &Bool,
    span: crate::span::Span,
    label: &str,
    session: &mut SolverSession,
    context_digest: &str,
    source_hash: &str,
    uses: &[TypedDefaultProofUse],
    results: &mut Vec<VerificationResult>,
) -> bool {
    let start = Instant::now();
    let status = match session.check_scope(obligation.not()).0 {
        SatResult::Unsat => VerifStatus::Proven,
        SatResult::Sat => VerifStatus::Disproven,
        SatResult::Unknown => session.unknown_status(),
    };
    let (message, proof_was_attempted) = match &status {
        VerifStatus::Proven => (
            format!("checked default {label} is defined and satisfied"),
            true,
        ),
        VerifStatus::Disproven => (
            format!("checked default {label} may be false or undefined"),
            true,
        ),
        VerifStatus::SolverUnknown => (
            format!("checked default {label} proof is inconclusive"),
            true,
        ),
        VerifStatus::Timeout => (
            format!("checked default {label} proof timed out"),
            true,
        ),
        VerifStatus::InfrastructureError => (
            format!("checked default {label} could not be checked because of a verifier infrastructure error"),
            false,
        ),
        VerifStatus::RuntimeOnlyContract => (
            format!("checked default {label} depends on runtime-only contract state"),
            false,
        ),
        VerifStatus::NoObligations => (
            format!("checked default {label} generated no proof obligations"),
            false,
        ),
        VerifStatus::NotInTrustedSubset => return false,
    };
    let root = uses
        .first()
        .map(|usage| usage.callee_owner.0.as_str())
        .unwrap_or(callable.owner.0.as_str());
    let use_fingerprint = format!("{uses:#?}");
    let digest = blake3::hash(
        format!(
            "mimi-typed-ffi-default-proof-v1\ncontext={context_digest}\nowner={}\npoint={}\nuses={use_fingerprint}",
            callable.owner.0, point
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string();
    let artifact = proof_was_attempted.then(|| {
        let mut artifact =
            ProofArtifact::new(z3::full_version().to_string(), source_hash.to_string());
        artifact.engine = "resolved_ffi".into();
        artifact.integer_model = "checked".into();
        artifact.resolved_ir_hash = digest;
        artifact
    });
    let diagnostic = (status == VerifStatus::Disproven).then(|| {
        crate::diagnostic::Diagnostic::error(
            format!("default proof root '{}' may be false or undefined", point),
            span,
        )
    });
    results.push(VerificationResult {
        func_name: format!("{} default proof {} ({label})", root, point),
        status: status.clone(),
        message,
        diagnostic,
        duration_us: start.elapsed().as_micros() as u64,
        constraint_count: usize::from(proof_was_attempted),
        artifact,
        trusted_subset_domain: None,
    });
    true
}

/// Verify only default Math roots that have a checker-authenticated omitted
/// argument use and fit the local-only profile. Canonical MIR owns the body
/// call result; this separate consumer never imports caller Requires or
/// foreign Ensures as facts for a declaration default.
pub(super) fn verify_default_math_roots(
    program: &CheckedProgram,
    requires_names: &BTreeSet<String>,
    source_hash: &str,
) -> Result<Vec<VerificationResult>, String> {
    let plans = default_math_plans(program, requires_names);
    if plans.is_empty() {
        return Ok(Vec::new());
    }

    let mut hash = blake3::Hasher::new();
    hash.update(b"mimi-typed-ffi-default-context-v1\n");
    for body in program.resolved_bodies().values() {
        hash.update(format!("{body:?}\n").as_bytes());
    }
    for root in program.declaration_expression_bodies().values() {
        hash.update(format!("{root:?}\n").as_bytes());
    }
    for plan in &plans {
        hash.update(format!("{:#?}\n{:#?}\n", plan.root, plan.uses).as_bytes());
    }
    let digest = hash.finalize().to_hex().to_string();
    let mut session = SolverSession::new(super::ctx::DEFAULT_TIMEOUT_MS)?;
    let mut results = Vec::new();
    for plan in &plans {
        let mut default_results = Vec::new();
        session.push();
        let replay = replay_default_math_block(
            &plan.root.root,
            plan.callable,
            program,
            &mut Env::new(),
            &mut session,
            &digest,
            source_hash,
            &plan.uses,
            &mut default_results,
        );
        session.pop();
        if !replay {
            return Err(format!(
                "TOOL-RESOLUTION-001: checked default proof root '{}' changed during typed replay",
                plan.root.expression.0
            ));
        }
        results.extend(default_results);
    }
    Ok(results)
}

/// Preserve named obligations for contract scopes that this entry-snapshot
/// consumer does not yet model. This census uses every checked owner, including
/// dormant callables/defaults, and never treats the flattened contract catalog
/// as a list of assumptions at callable entry.
pub(super) fn deferred_context_results(
    program: &CheckedProgram,
    requires_names: &BTreeSet<String>,
) -> Vec<VerificationResult> {
    let supported_default_math = default_math_plans(program, requires_names)
        .into_iter()
        .flat_map(|plan| plan.root.points.into_iter().map(|point| point.id.statement))
        .collect::<BTreeSet<_>>();
    let owners = program
        .call_sites_sorted()
        .into_iter()
        .filter(|site| {
            site.kind == ResolvedCallKind::Extern && requires_names.contains(&site.callee)
        })
        .map(|site| NodeId(site.owner.clone()))
        .collect::<BTreeSet<_>>();
    let mut results = Vec::new();
    let mut recorded = BTreeSet::new();
    let mut defer = |owner: &NodeId, root: &NodeId, span, detail: &str| {
        if !recorded.insert(root.clone()) {
            return;
        }
        let message = format!(
            "{}: typed FFI context '{}' has unmodeled proof root '{}': {}; its obligation is Deferred independently of the foreign Requires result",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE, owner.0, root.0, detail
        );
        results.push(VerificationResult {
            func_name: format!("{} FFI context {}", caller_name(program, owner), root.0),
            status: VerifStatus::NotInTrustedSubset,
            diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message.clone(),
                span,
            )),
            message,
            duration_us: 0,
            constraint_count: 0,
            artifact: None,
            trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
        });
    };
    for owner in &owners {
        let Some(callable) = program.callable(owner) else {
            continue;
        };
        if !callable.has_unmodeled_proof_roots() {
            continue;
        }
        // The general proof-worklist census also flags caller-path shapes
        // (literal branches, singleton iteration and exits) which this FFI
        // consumer models directly. Do not turn those unrelated boundaries
        // into a fabricated Math obligation: incomplete FFI paths already
        // take the compatibility route in `verify`, while complete paths can
        // use their typed guards/bindings here.
        let direct = callable
            .body
            .root
            .statements
            .iter()
            .filter_map(|statement| {
                matches!(&statement.kind, ResolvedStmtKind::Contract { .. })
                    .then_some(&statement.node_id)
            })
            .collect::<BTreeSet<_>>();
        for contract in &callable.contracts {
            if !direct.contains(&contract.node_id) {
                defer(
                    owner,
                    &contract.node_id,
                    contract.origin.user_span(),
                    "nested/default/place-input contract scope is not an entry Requires assumption",
                );
            }
        }
        // A Math context is a real independent root only when the typed
        // body contains one. Generic worklist boundaries can also come from
        // calls/control-flow that have no Math statement at all (for example
        // generated Flow bodies), so they must not be relabelled as Math.
        for (root, origin) in callable.body.math_proof_roots() {
            if supported_default_math.contains(&root) {
                continue;
            }
            defer(
                owner,
                &root,
                origin.user_span(),
                "Math proof root has an unmodeled typed context",
            );
        }
    }
    let required_declarations = program
        .ffi_declarations()
        .values()
        .filter(|declaration| requires_names.contains(&declaration.symbol))
        .map(|declaration| declaration.declaration.clone())
        .collect::<BTreeSet<_>>();
    for root in program.declaration_expression_bodies().values() {
        let selected = owners.contains(&root.owner)
            || (root.kind == ResolvedDeclarationExpressionKind::ExternRequires
                && required_declarations.contains(&root.owner));
        if !selected {
            continue;
        }
        if let Ok(body) = &root.body {
            for contract in body.contract_proof_roots() {
                defer(
                    &root.owner,
                    &contract.node_id,
                    contract.origin.user_span(),
                    "declaration expression contains a nested contract proof root",
                );
            }
            for (math_root, origin) in body.math_proof_roots() {
                if supported_default_math.contains(&math_root) {
                    continue;
                }
                defer(
                    &root.owner,
                    &math_root,
                    origin.user_span(),
                    "declaration expression contains a Math proof root without an FFI context receipt",
                );
            }
        }
    }
    results
}

pub(super) fn caller_name(program: &CheckedProgram, owner: &NodeId) -> String {
    if let Some(function) = program.functions().get(owner) {
        return function.qualified_name.clone();
    }
    if let Some(transition) = program
        .transitions()
        .values()
        .find(|transition| &transition.node_id == owner)
    {
        return format!(
            "{}::{} from {}",
            transition.id.flow.0, transition.id.event, transition.id.source.name
        );
    }
    owner.0.clone()
}

fn visit_block<'a>(
    block: &'a ResolvedBlock,
    body: &'a ResolvedBody,
    caller: &str,
    path: &PathContext<'a>,
    seen: &mut BTreeSet<NodeId>,
    sites: &mut Vec<Site<'a>>,
) {
    let mut current_path = path.clone();
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(value) = initializer {
                    visit_expr(value, body, caller, &current_path, seen, sites);
                }
            }
            ResolvedStmtKind::Assign { target, value, .. } => {
                visit_place(target, body, caller, &current_path, seen, sites);
                visit_expr(value, body, caller, &current_path, seen, sites);
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    visit_expr(value, body, caller, &current_path, seen, sites);
                }
            }
            ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => visit_expr(value, body, caller, &current_path, seen, sites),
            ResolvedStmtKind::While {
                condition,
                body: nested,
            } => {
                visit_expr(condition, body, caller, &current_path, seen, sites);
                let body_path = current_path.with_guard(condition, true);
                visit_block(nested, body, caller, &body_path, seen, sites);
            }
            ResolvedStmtKind::WhileLet {
                initializer,
                body: nested,
                ..
            } => {
                visit_expr(initializer, body, caller, &current_path, seen, sites);
                let body_path = current_path.incomplete();
                visit_block(nested, body, caller, &body_path, seen, sites);
            }
            ResolvedStmtKind::For {
                pattern,
                iterable: initializer,
                body: nested,
            } => {
                visit_expr(initializer, body, caller, &current_path, seen, sites);
                let body_path = singleton_for_binding(pattern, initializer, body)
                    .filter(|(local, _)| !mutates_parameter(nested, std::slice::from_ref(local)))
                    .map(|(local, value)| current_path.with_binding(local, value))
                    .unwrap_or_else(|| current_path.incomplete());
                visit_block(nested, body, caller, &body_path, seen, sites);
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                visit_expr(initializer, body, caller, &current_path, seen, sites);
                let branch_path = current_path.incomplete();
                visit_block(then_block, body, caller, &branch_path, seen, sites);
                if let Some(block) = else_block {
                    visit_block(block, body, caller, &branch_path, seen, sites);
                }
            }
            ResolvedStmtKind::Loop(nested) | ResolvedStmtKind::Scope { body: nested, .. } => {
                visit_block(nested, body, caller, &current_path, seen, sites)
            }
            ResolvedStmtKind::Pinned {
                value,
                body: nested,
                ..
            } => {
                visit_expr(value, body, caller, &current_path, seen, sites);
                visit_block(nested, body, caller, &current_path, seen, sites);
            }
            ResolvedStmtKind::Drop(places) => {
                for place in places {
                    visit_place(place, body, caller, &current_path, seen, sites);
                }
            }
            ResolvedStmtKind::Math(values) => {
                let math_path = current_path.incomplete();
                for value in values {
                    visit_expr(value, body, caller, &math_path, seen, sites);
                }
            }
            ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {}
        }
        current_path = continuation_path(statement, &current_path);
    }
    if let Some(value) = &block.result {
        visit_expr(value, body, caller, &current_path, seen, sites);
    }
}

fn continuation_path<'a>(statement: &'a ResolvedStmt, path: &PathContext<'a>) -> PathContext<'a> {
    match &statement.kind {
        ResolvedStmtKind::Break(_)
        | ResolvedStmtKind::Continue
        | ResolvedStmtKind::Return { .. } => path.incomplete(),
        ResolvedStmtKind::Expr(ResolvedExpr {
            kind:
                ResolvedExprKind::If {
                    condition,
                    then_block,
                    else_block,
                },
            ..
        }) => match (
            block_always_transfers(then_block),
            block_always_transfers(else_block),
        ) {
            (true, false) => path.with_guard(condition, false),
            (false, true) => path.with_guard(condition, true),
            (true, true) => path.incomplete(),
            (false, false) => path.clone(),
        },
        _ => path.clone(),
    }
}

fn block_always_transfers(block: &ResolvedBlock) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            ResolvedStmtKind::Break(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::Return { .. } => true,
            ResolvedStmtKind::Expr(value) => expr_always_transfers(value),
            _ => false,
        })
}

fn expr_always_transfers(value: &ResolvedExpr) -> bool {
    match &value.kind {
        ResolvedExprKind::If {
            then_block,
            else_block,
            ..
        } => block_always_transfers(then_block) && block_always_transfers(else_block),
        _ => false,
    }
}

fn singleton_for_binding<'a>(
    pattern: &'a ResolvedPattern,
    iterable: &'a ResolvedExpr,
    body: &'a ResolvedBody,
) -> Option<(&'a ResolvedLocalId, &'a ResolvedExpr)> {
    let ResolvedPatternKind::Binding {
        local,
        by_reference: None,
    } = &pattern.kind
    else {
        return None;
    };
    let ResolvedExprKind::List(values) = &iterable.kind else {
        return None;
    };
    let [value] = values.as_slice() else {
        return None;
    };
    let declared_local = body.locals.get(local)?;
    if pattern.ty != value.ty || declared_local.ty != pattern.ty {
        return None;
    }
    Some((local, value))
}

fn visit_place<'a>(
    place: &'a ResolvedPlace,
    body: &'a ResolvedBody,
    caller: &str,
    path: &PathContext<'a>,
    seen: &mut BTreeSet<NodeId>,
    sites: &mut Vec<Site<'a>>,
) {
    for projection in &place.projections {
        if let ResolvedProjection::Index {
            index: ResolvedIndex::Dynamic(node),
            ..
        } = projection
        {
            if let Some(input) = body.place_inputs.get(node) {
                visit_expr(input, body, caller, path, seen, sites);
            }
        }
    }
}

fn visit_expr<'a>(
    value: &'a ResolvedExpr,
    body: &'a ResolvedBody,
    caller: &str,
    path: &PathContext<'a>,
    seen: &mut BTreeSet<NodeId>,
    sites: &mut Vec<Site<'a>>,
) {
    if !seen.insert(value.node_id.clone()) {
        return;
    }
    macro_rules! child {
        ($child:expr) => {
            visit_expr($child, body, caller, path, seen, sites)
        };
    }
    match &value.kind {
        ResolvedExprKind::Call(call) => {
            for argument in &call.arguments {
                child!(&argument.value);
            }
            if matches!(call.callee, ResolvedCallee::Extern(_)) {
                sites.push(Site {
                    expression: value,
                    call,
                    body,
                    caller: caller.into(),
                    path: path.clone(),
                });
            }
        }
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let ResolvedFStringPart::Interpolation(value) = part {
                    child!(value);
                }
            }
        }
        ResolvedExprKind::Load(place) => visit_place(place, body, caller, path, seen, sites),
        ResolvedExprKind::Project { value, projection } => {
            child!(value);
            if let ResolvedValueProjection::Index(index) = projection {
                child!(index);
            }
        }
        ResolvedExprKind::Binary { op, left, right } => {
            visit_expr(left, body, caller, path, seen, sites);
            match op {
                ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr => {
                    let expected = *op == ResolvedBinaryOp::LogicalAnd;
                    let right_path = path.with_guard(left, expected);
                    visit_expr(right, body, caller, &right_path, seen, sites);
                }
                _ => visit_expr(right, body, caller, path, seen, sites),
            }
        }
        ResolvedExprKind::Range { start, end } => {
            child!(start);
            child!(end);
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
        | ResolvedExprKind::Await(operand) => child!(operand),
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => {
            for value in values {
                child!(value);
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                child!(key);
                child!(value);
            }
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            child!(iterable);
            let element_path = path.incomplete();
            if let Some(guard) = guard {
                visit_expr(guard, body, caller, &element_path, seen, sites);
            }
            visit_expr(value, body, caller, &element_path, seen, sites);
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                child!(&field.value);
            }
            if let Some(rest) = rest {
                child!(rest);
            }
        }
        ResolvedExprKind::Block(nested)
        | ResolvedExprKind::Scope { body: nested, .. }
        | ResolvedExprKind::Comptime(nested) => {
            visit_block(nested, body, caller, path, seen, sites)
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            visit_expr(condition, body, caller, path, seen, sites);
            let then_path = path.with_guard(condition, true);
            let else_path = path.with_guard(condition, false);
            visit_block(then_block, body, caller, &then_path, seen, sites);
            visit_block(else_block, body, caller, &else_path, seen, sites);
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            child!(scrutinee);
            let supported = arms.iter().enumerate().all(|(index, arm)| {
                arm.guard.is_none()
                    && match &arm.pattern.kind {
                        ResolvedPatternKind::Literal(
                            ResolvedLiteral::Int(_)
                            | ResolvedLiteral::Bool(_)
                            | ResolvedLiteral::FloatBits(_)
                            | ResolvedLiteral::String(_),
                        ) => true,
                        ResolvedPatternKind::Wildcard => index + 1 == arms.len(),
                        _ => false,
                    }
            });
            if supported {
                let mut unmatched_path = path.clone();
                for arm in arms {
                    match &arm.pattern.kind {
                        ResolvedPatternKind::Literal(_) => {
                            let arm_path =
                                unmatched_path.with_pattern_guard(scrutinee, &arm.pattern, true);
                            visit_expr(&arm.body, body, caller, &arm_path, seen, sites);
                            unmatched_path =
                                unmatched_path.with_pattern_guard(scrutinee, &arm.pattern, false);
                        }
                        ResolvedPatternKind::Wildcard => {
                            visit_expr(&arm.body, body, caller, &unmatched_path, seen, sites);
                        }
                        _ => {
                            let arm_path = unmatched_path.incomplete();
                            visit_expr(&arm.body, body, caller, &arm_path, seen, sites);
                        }
                    }
                }
            } else {
                let arm_path = path.incomplete();
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit_expr(guard, body, caller, &arm_path, seen, sites);
                    }
                    visit_expr(&arm.body, body, caller, &arm_path, seen, sites);
                }
            }
        }
        ResolvedExprKind::Slice { target, start, end } => {
            child!(target);
            if let Some(start) = start {
                child!(start);
            }
            if let Some(end) = end {
                child!(end);
            }
        }
        // Lambda parameters have their own identities. They cannot borrow an
        // outer callable's Requires path by display-name coincidence.
        ResolvedExprKind::Lambda(lambda) => {
            let lambda_path = PathContext::entry(false);
            visit_block(
                &lambda.body,
                body,
                &format!("{caller} closure {}", lambda.owner.0),
                &lambda_path,
                seen,
                sites,
            );
        }
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_)
        | ResolvedExprKind::Quote(_) => {}
    }
}

fn parameter_env(body: &ResolvedBody, program: &CheckedProgram) -> Env {
    body.parameters
        .iter()
        .filter_map(|identity| {
            let local = body.locals.get(identity)?;
            let key = format!("ffi:{}", identity.0 .0);
            let term = parameter_term(program, &local.ty, &key, &mut BTreeSet::new())?;
            Some((identity.clone(), term))
        })
        .collect()
}

fn site_env(body: &ResolvedBody, path: &PathContext<'_>, program: &CheckedProgram) -> Option<Env> {
    let mut env = parameter_env(body, program);
    for (identity, value) in &path.bindings {
        let local = body.locals.get(identity)?;
        if local.ty != value.ty {
            return None;
        }
        let term = eval(value, program, &env)?;
        env.insert(identity.clone(), term);
    }
    Some(env)
}

// Use only stable member identities and canonical field types. In particular,
// declaration snapshots and field display types are not part of this encoder.
fn record_schema<'a>(
    program: &'a CheckedProgram,
    ty: &ResolvedTypeId,
) -> Option<(&'a NominalTypeId, &'a ResolvedTypeDef)> {
    let mut current = ty;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return None;
        }
        let ResolvedType::Nominal {
            item,
            arguments,
            is_linear: false,
        } = program.resolved_types().get(current)?
        else {
            return None;
        };
        if !arguments.is_empty() {
            return None;
        }
        let owner = NodeId(item.as_str().to_string());
        let definition = program.type_defs().get(&owner)?;
        match definition.kind {
            ResolvedTypeKind::Record if definition.generic_parameters.is_empty() => {
                return Some((item, definition));
            }
            ResolvedTypeKind::Alias => current = program.resolved_type_target(&owner)?,
            _ => return None,
        }
    }
}

fn parameter_term(
    program: &CheckedProgram,
    ty: &ResolvedTypeId,
    key: &str,
    visiting: &mut BTreeSet<ResolvedTypeId>,
) -> Option<Term> {
    if let Some(primitive) = canonical_primitive(program, ty) {
        let value = match primitive {
            PrimitiveType::Bool => Value::Bool(Bool::new_const(key)),
            PrimitiveType::String => Value::String(Z3String::new_const(key)),
            PrimitiveType::I8
            | PrimitiveType::I16
            | PrimitiveType::I32
            | PrimitiveType::I64
            | PrimitiveType::U8
            | PrimitiveType::U16
            | PrimitiveType::U32
            | PrimitiveType::U64 => Value::Int(Int::new_const(key)),
            // Mimi f64 values have IEEE-754 binary64 semantics. The Z3
            // floating-point sort preserves NaN, infinities, signed zero,
            // and ordered comparison behavior; no Real relaxation is used.
            PrimitiveType::F64 => Value::Float(Float::new_const_double(key)),
            _ => return None,
        };
        return Some(Term::total(value));
    }
    // Recursive by-value records cannot be finite input values. An incomplete
    // canonical shape stays on the explicit compatibility boundary.
    if !visiting.insert(ty.clone()) {
        return None;
    }
    let (nominal, definition) = record_schema(program, ty)?;
    let mut fields = BTreeMap::new();
    for field in definition.field_ids.values() {
        let field_type = program.resolved_field_type(field)?;
        let term = parameter_term(program, field_type, &format!("{key}/{}", field.0), visiting)?;
        fields.insert(field.clone(), term);
    }
    visiting.remove(ty);
    Some(Term::total(Value::Record {
        nominal: nominal.clone(),
        fields,
    }))
}

fn project_field(
    program: &CheckedProgram,
    aggregate: &Term,
    field: &NodeId,
    ty: &ResolvedTypeId,
) -> Option<Term> {
    let Value::Record { nominal, fields } = &aggregate.value else {
        return None;
    };
    let definition = program
        .type_defs()
        .get(&NodeId(nominal.as_str().to_string()))?;
    if definition.kind != ResolvedTypeKind::Record
        || !definition
            .field_ids
            .values()
            .any(|identity| identity == field)
        || program.resolved_field_type(field) != Some(ty)
    {
        return None;
    }
    let mut value = fields.get(field)?.clone();
    // Constructing a record evaluates all its fields, even when the contract
    // subsequently projects one of them. Keep that aggregate definedness.
    value.defined = Bool::and(&[&aggregate.defined, &value.defined]);
    Some(value)
}

fn assert_parameter_domain(
    session: &mut SolverSession,
    body: &ResolvedBody,
    program: &CheckedProgram,
    env: &Env,
) -> Result<(), String> {
    for (identity, term) in env {
        let ty = &body
            .locals
            .get(identity)
            .ok_or_else(|| {
                format!(
                    "TOOL-RESOLUTION-001: typed FFI parameter '{}' has no local",
                    identity.0 .0
                )
            })?
            .ty;
        assert_term_domain(session, program, term, ty)?;
    }
    Ok(())
}

fn assert_term_domain(
    session: &mut SolverSession,
    program: &CheckedProgram,
    term: &Term,
    ty: &ResolvedTypeId,
) -> Result<(), String> {
    match &term.value {
        Value::Int(integer) => {
            let (minimum, maximum) = canonical_primitive(program, ty)
                .and_then(integer_parameter_bounds)
                .ok_or_else(|| format!("TOOL-RESOLUTION-001: typed FFI integer leaf '{}' has no exact canonical machine domain", ty.as_str()))?;
            session.assert(integer.ge(&minimum));
            session.assert(integer.le(&maximum));
        }
        Value::Float(_) => {
            if canonical_primitive(program, ty) != Some(PrimitiveType::F64) {
                return Err(format!(
                    "TOOL-RESOLUTION-001: typed FFI floating leaf '{}' is not canonical f64",
                    ty.as_str()
                ));
            }
        }
        Value::Record { nominal, fields } => {
            let (expected, definition) = record_schema(program, ty).ok_or_else(|| {
                format!(
                    "TOOL-RESOLUTION-001: typed FFI record '{}' has no canonical field schema",
                    ty.as_str()
                )
            })?;
            if nominal != expected || fields.len() != definition.field_ids.len() {
                return Err("TOOL-RESOLUTION-001: typed FFI record parameter disagrees with its nominal field schema".into());
            }
            for field in definition.field_ids.values() {
                let value = fields.get(field).ok_or_else(|| {
                    format!(
                        "TOOL-RESOLUTION-001: typed FFI record is missing field '{}'",
                        field.0
                    )
                })?;
                let field_type = program.resolved_field_type(field).ok_or_else(|| {
                    format!(
                        "TOOL-RESOLUTION-001: typed FFI field '{}' has no canonical type",
                        field.0
                    )
                })?;
                assert_term_domain(session, program, value, field_type)?;
            }
        }
        Value::Bool(_) | Value::String(_) => {}
    }
    Ok(())
}

// These canonical primitives are broader than the current i32/i64 surface.
// Their exact domains do not advertise new parser, Checker or FFI ABI support.
fn integer_parameter_bounds(primitive: PrimitiveType) -> Option<(Int, Int)> {
    Some(match primitive {
        PrimitiveType::I8 => (Int::from_i64(i8::MIN as i64), Int::from_i64(i8::MAX as i64)),
        PrimitiveType::I16 => (
            Int::from_i64(i16::MIN as i64),
            Int::from_i64(i16::MAX as i64),
        ),
        PrimitiveType::I32 => (
            Int::from_i64(i32::MIN as i64),
            Int::from_i64(i32::MAX as i64),
        ),
        PrimitiveType::I64 => (Int::from_i64(i64::MIN), Int::from_i64(i64::MAX)),
        PrimitiveType::U8 => (Int::from_u64(0), Int::from_u64(u8::MAX as u64)),
        PrimitiveType::U16 => (Int::from_u64(0), Int::from_u64(u16::MAX as u64)),
        PrimitiveType::U32 => (Int::from_u64(0), Int::from_u64(u32::MAX as u64)),
        PrimitiveType::U64 => (Int::from_u64(0), Int::from_u64(u64::MAX)),
        _ => return None,
    })
}

// Follow the checker-owned alias target directory rather than deriving a
// machine width from a display name. Cycles or unsupported types remain an
// explicit encoding boundary.
fn canonical_primitive(program: &CheckedProgram, ty: &ResolvedTypeId) -> Option<PrimitiveType> {
    let mut current = ty;
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return None;
        }
        current = match program.resolved_types().get(current)? {
            ResolvedType::Primitive(primitive) => return Some(*primitive),
            ResolvedType::Nominal {
                item, arguments, ..
            } if arguments.is_empty() => {
                program.resolved_type_target(&NodeId(item.as_str().to_string()))?
            }
            _ => return None,
        };
    }
}

fn signed_integer_range(program: &CheckedProgram, ty: &ResolvedTypeId) -> Option<(i64, i64)> {
    match canonical_primitive(program, ty)? {
        PrimitiveType::I32 => Some((i32::MIN as i64, i32::MAX as i64)),
        PrimitiveType::I64 => Some((i64::MIN, i64::MAX)),
        _ => None,
    }
}

fn eval_literal_pattern(
    scrutinee: &ResolvedExpr,
    pattern: &ResolvedPattern,
    program: &CheckedProgram,
    env: &Env,
) -> Option<Term> {
    if scrutinee.ty != pattern.ty {
        return None;
    }
    let ResolvedPatternKind::Literal(literal) = &pattern.kind else {
        return None;
    };
    let term = eval(scrutinee, program, env)?;
    let predicate = match (&term.value, literal) {
        (Value::Int(value), ResolvedLiteral::Int(literal)) => value.eq(Int::from_i64(*literal)),
        (Value::Bool(value), ResolvedLiteral::Bool(literal)) => value.eq(Bool::from_bool(*literal)),
        (Value::Float(value), ResolvedLiteral::FloatBits(literal)) => {
            value.eq_fpa(Float::from_f64(f64::from_bits(*literal)))
        }
        (Value::String(value), ResolvedLiteral::String(literal)) => {
            let literal = super::mir::symbolic_string_literal(literal).ok()?;
            value.eq(&literal)
        }
        _ => return None,
    };
    Some(Term {
        value: Value::Bool(predicate),
        defined: term.defined,
    })
}

fn eval(value: &ResolvedExpr, program: &CheckedProgram, env: &Env) -> Option<Term> {
    use ResolvedBinaryOp as B;
    let term = match &value.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => {
            Term::total(Value::Int(Int::from_i64(*value)))
        }
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(value)) => {
            Term::total(Value::Bool(Bool::from_bool(*value)))
        }
        ResolvedExprKind::Literal(ResolvedLiteral::FloatBits(bits)) => {
            Term::total(Value::Float(Float::from_f64(f64::from_bits(*bits))))
        }
        ResolvedExprKind::Literal(ResolvedLiteral::String(value)) => Term::total(Value::String(
            super::mir::symbolic_string_literal(value).ok()?,
        )),
        ResolvedExprKind::Load(place) => {
            let mut aggregate = env.get(&place.base)?.clone();
            for projection in &place.projections {
                let ResolvedProjection::Field { field, ty, .. } = projection else {
                    return None;
                };
                aggregate = project_field(program, &aggregate, field, ty)?;
            }
            aggregate
        }
        ResolvedExprKind::Project {
            value: aggregate,
            projection: ResolvedValueProjection::Field(field),
        } => {
            let aggregate = eval(aggregate, program, env)?;
            project_field(program, &aggregate, field, &value.ty)?
        }
        ResolvedExprKind::Record {
            nominal,
            fields,
            rest,
        } => {
            let (expected, definition) = record_schema(program, &value.ty)?;
            if nominal != expected {
                return None;
            }
            let mut record_fields = BTreeMap::new();
            let mut definedness = Vec::new();
            if let Some(rest) = rest {
                let rest = eval(rest, program, env)?;
                definedness.push(rest.defined);
                let Value::Record {
                    nominal: rest_nominal,
                    fields: rest_fields,
                } = rest.value
                else {
                    return None;
                };
                if &rest_nominal != nominal {
                    return None;
                }
                record_fields = rest_fields;
            }
            let mut assigned = BTreeSet::new();
            for field in fields {
                if !definition
                    .field_ids
                    .values()
                    .any(|identity| identity == &field.field)
                    || !assigned.insert(field.field.clone())
                    || field.conversion.from != field.value.ty
                    || Some(&field.conversion.to) != program.resolved_field_type(&field.field)
                    || !matches!(
                        field.conversion.kind,
                        CheckedConversionKind::Identity
                            | CheckedConversionKind::AliasWrap
                            | CheckedConversionKind::AliasUnwrap
                            | CheckedConversionKind::NumericWiden
                            | CheckedConversionKind::ContainerAliasErase
                    )
                {
                    return None;
                }
                let term = eval(&field.value, program, env)?;
                definedness.push(term.defined.clone());
                record_fields.insert(field.field.clone(), term);
            }
            if record_fields.len() != definition.field_ids.len() {
                return None;
            }
            Term {
                value: Value::Record {
                    nominal: nominal.clone(),
                    fields: record_fields,
                },
                defined: Bool::and(&definedness),
            }
        }
        ResolvedExprKind::Cast { value, conversion }
            if matches!(
                conversion.kind,
                CheckedConversionKind::Identity
                    | CheckedConversionKind::AliasWrap
                    | CheckedConversionKind::AliasUnwrap
                    | CheckedConversionKind::NumericWiden
                    | CheckedConversionKind::ContainerAliasErase
            ) =>
        {
            eval(value, program, env)?
        }
        ResolvedExprKind::Unary { op, operand } => {
            let operand = eval(operand, program, env)?;
            let result = match (op, &operand.value) {
                (ResolvedUnaryOp::Not, Value::Bool(value)) => Value::Bool(value.not()),
                (ResolvedUnaryOp::Negate, Value::Int(value)) => Value::Int(value.unary_minus()),
                (ResolvedUnaryOp::Negate, Value::Float(value)) => Value::Float(value.unary_neg()),
                _ => return None,
            };
            Term {
                value: result,
                defined: operand.defined,
            }
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let operand_type = &left.ty;
            let left = eval(left, program, env)?;
            let right = eval(right, program, env)?;
            let mut defined = Bool::and(&[&left.defined, &right.defined]);
            let result = match (&left.value, &right.value) {
                (Value::Int(left), Value::Int(right)) => match op {
                    B::Add => Value::Int(Int::add(&[left, right])),
                    B::Subtract => Value::Int(Int::sub(&[left, right])),
                    B::Multiply => Value::Int(Int::mul(&[left, right])),
                    B::Divide | B::Remainder => {
                        let (minimum, _) = signed_integer_range(program, operand_type)?;
                        // Native and VM trap for both MIN / -1 and MIN % -1.
                        // Remainder's mathematical result (zero) fits, so a
                        // result bounds check alone cannot establish safety.
                        let overflow = Bool::and(&[
                            &left.eq(Int::from_i64(minimum)),
                            &right.eq(Int::from_i64(-1)),
                        ]);
                        defined =
                            Bool::and(&[&defined, &right.ne(Int::from_i64(0)), &overflow.not()]);
                        let zero = Int::from_i64(0);
                        let a = left.ge(&zero).ite(left, &left.unary_minus());
                        let b = right.ge(&zero).ite(right, &right.unary_minus());
                        let result = if *op == B::Divide {
                            left.ge(&zero)
                                .eq(right.ge(&zero))
                                .ite(&a.div(&b), &a.div(&b).unary_minus())
                        } else {
                            left.ge(&zero)
                                .ite(&a.modulo(&b), &a.modulo(&b).unary_minus())
                        };
                        Value::Int(result)
                    }
                    B::Equal => Value::Bool(left.eq(right)),
                    B::NotEqual => Value::Bool(left.ne(right)),
                    B::Less => Value::Bool(left.lt(right)),
                    B::Greater => Value::Bool(left.gt(right)),
                    B::LessEqual => Value::Bool(left.le(right)),
                    B::GreaterEqual => Value::Bool(left.ge(right)),
                    _ => return None,
                },
                (Value::Bool(left), Value::Bool(right)) => match op {
                    B::LogicalAnd => Value::Bool(Bool::and(&[left, right])),
                    B::LogicalOr => Value::Bool(Bool::or(&[left, right])),
                    B::Equal => Value::Bool(left.eq(right)),
                    B::NotEqual => Value::Bool(left.ne(right)),
                    _ => return None,
                },
                (Value::Float(left), Value::Float(right)) => match op {
                    // FPA comparisons model Mimi's IEEE ordered comparisons:
                    // NaN is unordered, and equality treats +0.0 == -0.0.
                    B::Equal => Value::Bool(left.eq_fpa(right)),
                    B::NotEqual => Value::Bool(left.eq_fpa(right).not()),
                    B::Less => Value::Bool(left.lt(right)),
                    B::Greater => Value::Bool(left.gt(right)),
                    B::LessEqual => Value::Bool(left.le(right)),
                    B::GreaterEqual => Value::Bool(left.ge(right)),
                    // Float arithmetic/conversions need their own checked
                    // IEEE operation lowering and stay explicitly deferred.
                    _ => return None,
                },
                (Value::String(left), Value::String(right)) => match op {
                    B::Equal => Value::Bool(left.eq(right)),
                    B::NotEqual => Value::Bool(left.ne(right)),
                    B::Add => Value::String(Z3String::concat(&[left, right])),
                    _ => return None,
                },
                _ => return None,
            };
            // Short-circuit only evaluates the right predicate on its runtime
            // path, including its division/overflow definedness obligations.
            if let Value::Bool(predicate) = &left.value {
                if *op == B::LogicalAnd {
                    defined = Bool::and(&[&left.defined, &predicate.implies(&right.defined)]);
                }
                if *op == B::LogicalOr {
                    defined = Bool::and(&[&left.defined, &predicate.not().implies(&right.defined)]);
                }
            }
            Term {
                value: result,
                defined,
            }
        }
        ResolvedExprKind::Call(call) => {
            if matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "len")
                && call.arguments.len() == 1
            {
                let argument = eval(&call.arguments[0].value, program, env)?;
                if let Value::String(value) = argument.value {
                    Term {
                        value: Value::Int(value.length()),
                        defined: argument.defined,
                    }
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            // A conditional actual is a pure checker-owned expression when
            // both arms are expression-only blocks.  Encode the selected
            // branch with an ite and retain branch-local definedness; an
            // unmodelled statement/effect in either arm still returns `None`
            // and remains on the explicit compatibility boundary.
            let condition = eval(condition, program, env)?;
            let predicate = condition.bool()?;
            let then_term = eval_expression_only_block(then_block, program, env)?;
            let else_term = eval_expression_only_block(else_block, program, env)?;
            let value = select_value(&predicate, &then_term.value, &else_term.value)?;
            let defined = Bool::and(&[
                &condition.defined,
                &predicate.implies(&then_term.defined),
                &predicate.not().implies(&else_term.defined),
            ]);
            Term { value, defined }
        }
        ResolvedExprKind::Block(block) if block.statements.is_empty() => {
            eval(block.result.as_ref()?, program, env)?
        }
        _ => return None,
    };
    if let Value::Int(integer) = &term.value {
        // Every machine arithmetic intermediate carries its checker-selected
        // width. Do not use i64 as a guessed default for predicates or casts.
        if matches!(
            &value.kind,
            ResolvedExprKind::Binary {
                op: B::Add | B::Subtract | B::Multiply | B::Divide | B::Remainder,
                ..
            } | ResolvedExprKind::Unary {
                op: ResolvedUnaryOp::Negate,
                ..
            }
        ) {
            let (lo, hi) = signed_integer_range(program, &value.ty)?;
            return Some(Term {
                value: term.value.clone(),
                defined: Bool::and(&[
                    &term.defined,
                    &integer.ge(Int::from_i64(lo)),
                    &integer.le(Int::from_i64(hi)),
                ]),
            });
        }
    }
    Some(term)
}

fn eval_expression_only_block(
    block: &ResolvedBlock,
    program: &CheckedProgram,
    env: &Env,
) -> Option<Term> {
    if !block.statements.is_empty() {
        return None;
    }
    eval(block.result.as_ref()?, program, env)
}

fn select_value(condition: &Bool, then_value: &Value, else_value: &Value) -> Option<Value> {
    match (then_value, else_value) {
        (Value::Int(then_value), Value::Int(else_value)) => {
            Some(Value::Int(condition.ite(then_value, else_value)))
        }
        (Value::Bool(then_value), Value::Bool(else_value)) => {
            Some(Value::Bool(condition.ite(then_value, else_value)))
        }
        (Value::Float(then_value), Value::Float(else_value)) => {
            Some(Value::Float(condition.ite(then_value, else_value)))
        }
        (Value::String(then_value), Value::String(else_value)) => {
            Some(Value::String(condition.ite(then_value, else_value)))
        }
        (
            Value::Record {
                nominal: then_nominal,
                fields: then_fields,
            },
            Value::Record {
                nominal: else_nominal,
                fields: else_fields,
            },
        ) if then_nominal == else_nominal && then_fields.len() == else_fields.len() => {
            let mut fields = BTreeMap::new();
            for (field, then_value) in then_fields {
                let else_value = else_fields.get(field)?;
                fields.insert(
                    field.clone(),
                    select_term(condition, then_value, else_value)?,
                );
            }
            Some(Value::Record {
                nominal: then_nominal.clone(),
                fields,
            })
        }
        _ => None,
    }
}

fn select_term(condition: &Bool, then_term: &Term, else_term: &Term) -> Option<Term> {
    Some(Term {
        value: select_value(condition, &then_term.value, &else_term.value)?,
        defined: Bool::and(&[
            &condition.implies(&then_term.defined),
            &condition.not().implies(&else_term.defined),
        ]),
    })
}

fn mutates_parameter(block: &ResolvedBlock, parameters: &[ResolvedLocalId]) -> bool {
    for statement in &block.statements {
        let found = match &statement.kind {
            ResolvedStmtKind::Assign { target, value, .. } => {
                parameters.contains(&target.base) || expr_mutates_parameter(value, parameters)
            }
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|value| expr_mutates_parameter(value, parameters)),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|value| expr_mutates_parameter(value, parameters)),
            ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_mutates_parameter(value, parameters),
            ResolvedStmtKind::Math(values) => values
                .iter()
                .any(|value| expr_mutates_parameter(value, parameters)),
            ResolvedStmtKind::While { condition, body } => {
                expr_mutates_parameter(condition, parameters) || mutates_parameter(body, parameters)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                expr_mutates_parameter(initializer, parameters)
                    || mutates_parameter(body, parameters)
            }
            ResolvedStmtKind::Scope { body, .. } | ResolvedStmtKind::Loop(body) => {
                mutates_parameter(body, parameters)
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_mutates_parameter(value, parameters) || mutates_parameter(body, parameters)
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_mutates_parameter(initializer, parameters)
                    || mutates_parameter(then_block, parameters)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| mutates_parameter(block, parameters))
            }
            ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_)
            | ResolvedStmtKind::Drop(_) => false,
        };
        if found {
            return true;
        }
    }
    block
        .result
        .as_ref()
        .is_some_and(|value| expr_mutates_parameter(value, parameters))
}

fn expr_mutates_parameter(value: &ResolvedExpr, parameters: &[ResolvedLocalId]) -> bool {
    let child = |value: &ResolvedExpr| expr_mutates_parameter(value, parameters);
    match &value.kind {
        ResolvedExprKind::Call(call) => {
            call.permission == Some(Permission::Mutate)
                || call.arguments.iter().any(|argument| child(&argument.value))
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            child(condition)
                || mutates_parameter(then_block, parameters)
                || mutates_parameter(else_block, parameters)
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block) => mutates_parameter(block, parameters),
        ResolvedExprKind::Match { scrutinee, arms } => {
            child(scrutinee)
                || arms
                    .iter()
                    .any(|arm| arm.guard.as_ref().is_some_and(child) || child(&arm.body))
        }
        ResolvedExprKind::FString(parts) => parts
            .iter()
            .any(|part| matches!(part, ResolvedFStringPart::Interpolation(value) if child(value))),
        ResolvedExprKind::Unary { op, operand } => {
            *op == ResolvedUnaryOp::BorrowMutable || child(operand)
        }
        ResolvedExprKind::Binary { left, right, .. }
        | ResolvedExprKind::Range {
            start: left,
            end: right,
        } => child(left) || child(right),
        ResolvedExprKind::Project { value, projection } => {
            child(value)
                || matches!(projection, ResolvedValueProjection::Index(index) if child(index))
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => values.iter().any(child),
        ResolvedExprKind::Map(entries) => entries
            .iter()
            .any(|(key, value)| child(key) || child(value)),
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => child(value) || child(iterable) || guard.as_ref().is_some_and(|guard| child(guard)),
        ResolvedExprKind::Record { fields, rest, .. } => {
            fields.iter().any(|field| child(&field.value))
                || rest.as_ref().is_some_and(|rest| child(rest))
        }
        ResolvedExprKind::Slice { target, start, end } => {
            child(target)
                || start.as_ref().is_some_and(|start| child(start))
                || end.as_ref().is_some_and(|end| child(end))
        }
        ResolvedExprKind::Old(value)
        | ResolvedExprKind::TypeOf(value)
        | ResolvedExprKind::Spawn(value)
        | ResolvedExprKind::Await(value)
        | ResolvedExprKind::Try { value, .. }
        | ResolvedExprKind::Cast { value, .. }
        | ResolvedExprKind::OptionalChain {
            receiver: value, ..
        } => child(value),
        ResolvedExprKind::Lambda(lambda) => mutates_parameter(&lambda.body, parameters),
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_)
        | ResolvedExprKind::Quote(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{integer_parameter_bounds, PrimitiveType};
    use std::str::FromStr;
    use z3::ast::{Bool, Int};
    use z3::{SatResult, Solver};

    #[test]
    fn typed_ffi_canonical_integer_domains_are_exact_without_host_width_guessing() {
        if !super::super::is_z3_available() {
            return;
        }
        // This is an internal enum/domain oracle. The unsigned and narrow
        // canonical shapes are not legal public Mimi signatures today.
        for (primitive, expected_minimum, expected_maximum) in [
            (PrimitiveType::I8, "-128", "127"),
            (PrimitiveType::I16, "-32768", "32767"),
            (PrimitiveType::I32, "-2147483648", "2147483647"),
            (
                PrimitiveType::I64,
                "-9223372036854775808",
                "9223372036854775807",
            ),
            (PrimitiveType::U8, "0", "255"),
            (PrimitiveType::U16, "0", "65535"),
            (PrimitiveType::U32, "0", "4294967295"),
            (PrimitiveType::U64, "0", "18446744073709551615"),
        ] {
            let (minimum, maximum) =
                integer_parameter_bounds(primitive).expect("known exact domain");
            let input = Int::new_const(format!("parameter-domain:{primitive:?}"));
            let actual = Bool::and(&[&input.ge(minimum), &input.le(maximum)]);
            let expected = Bool::and(&[
                &input.ge(Int::from_str(expected_minimum).expect("independent decimal oracle")),
                &input.le(Int::from_str(expected_maximum).expect("independent decimal oracle")),
            ]);
            let solver = Solver::new();
            solver.assert(actual.eq(expected).not());
            assert_eq!(
                solver.check(),
                SatResult::Unsat,
                "{primitive:?} domain disagrees with machine range"
            );
        }
        for primitive in [PrimitiveType::Isize, PrimitiveType::Usize] {
            assert!(
                integer_parameter_bounds(primitive).is_none(),
                "{primitive:?} requires an explicit host model"
            );
        }
    }
}
