pub(crate) mod checked_power;
mod ctx;
// The surface-AST verifier adapters are retained only by the unit-test
// equivalence harness.  Production verification enters through CheckedProgram
// and the canonical MIR/resolved consumers below; compiling these modules in
// the library would leave an externally reachable Flow/VIR compatibility
// engine even though no production route can legally call it.
#[cfg(test)]
mod expr;
mod ffi_checked;
#[cfg(test)]
mod flow;
#[cfg(test)]
mod flow_retirement_tests;
#[cfg(test)]
mod func;
#[cfg(test)]
mod helpers;
mod mir;
mod mir_capability;
mod mir_request;
pub(crate) mod resolved_expr;
#[cfg(test)]
pub mod vir;

pub(crate) use ctx::Z3VarMap;
pub use ctx::{
    Counterexample, ProofArtifact, TrustedSubsetDomain, VerifStatus, VerificationResult, Verifier,
};
pub(crate) use ctx::{SolverSession, VerifierCtx};
#[cfg(test)] // §11-#48 regression tests (audit_fix_verifier.rs)
pub(crate) use expr::encode_match_bool;
#[cfg(test)]
pub use flow::{FlowAcc, FlowEvent, VerifierState};

/// Convert a canonical route verifier error into the shared diagnostic
/// representation without changing the public `Result<_, String>` API used by
/// existing callers.  Registered MIR route codes are recognized anywhere in
/// an adapter-wrapped message using the shared token-boundary classifier;
/// ordinary verifier failures remain generic.
pub fn mir_route_error_to_diagnostic(message: impl Into<String>) -> crate::diagnostic::Diagnostic {
    crate::diagnostic::mir_route_error_diagnostic(message, crate::span::Span::UNKNOWN)
}

/// Verify a previously validated canonical MIR program with the experimental
/// MIR-only scalar contract engine.  The input boundary intentionally has no
/// AST/ResolvedBody parameter and never falls back to another verifier.
pub fn verify_mir(
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let profile = if mir_program_has_ordinary_map_owned_any_v2_actions(program) {
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
    } else {
        "verifier-mir-v1"
    };
    let receipt = program.route_receipt(profile);
    verify_mir_with_route_receipt(program, &receipt, source_hash)
}

fn verify_mir_results_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<Vec<VerificationResult>, String> {
    mir::verify_program_with_route_receipt(program, source_hash, receipt)
}

/// Verify canonical MIR after checking the caller-supplied route receipt.
/// This keeps the general contract verifier on the same immutable graph
/// identity as bytecode, native, and FFI-only verification consumers.
pub fn verify_mir_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    validate_ordinary_map_owned_any_v2_receipt_binding(program, receipt)?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    let results = enforce_ordinary_map_owned_any_v2_mir_readiness(program, receipt, results);
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_mir_with_route_receipt",
    )?;
    Ok(results)
}

/// Internal verifier diagnostic path. It validates graph identity and keeps
/// raw symbolic observations so the CheckedProgram layer can add one
/// source-owner NITS row after correlating MIR owner IDs with source names.
/// Public execution-verifier APIs must use `verify_mir_with_route_receipt`.
fn verify_mir_diagnostic_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: canonical route receipt rejected: {message}",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(&results, receipt, &source_hash, "verifier_diagnostic")?;
    Ok(results)
}

fn mir_program_has_ordinary_map_owned_any_v2_actions(
    program: &crate::core::mir::reference::MirProgram,
) -> bool {
    program.functions().values().any(|function| {
        function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_actions()
    })
}

fn validate_ordinary_map_owned_any_v2_receipt_binding(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), String> {
    let program_has_v2 = mir_program_has_ordinary_map_owned_any_v2_actions(program);
    let receipt_is_v2 = receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND;
    if program_has_v2 != receipt_is_v2 {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 MIR and route receipt profile must be bound together (program_v2={program_has_v2}, receipt_profile='{}')",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            receipt.profile
        ));
    }
    Ok(())
}

/// Enforce that one MIR verifier result batch carries one immutable execution
/// identity. A verifier emits one result per contract-bearing function and
/// one result per FFI call-site; checking only the first result would allow a
/// future aggregation change to silently mix source, MIR, or route witnesses
/// across the batch. Results without an artifact are inconclusive observations
/// and therefore have no provenance to validate.
fn validate_mir_result_provenance(
    results: &[VerificationResult],
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: &str,
    consumer: &str,
) -> Result<(), String> {
    for (index, result) in results.iter().enumerate() {
        let Some(artifact) = result.artifact.as_ref() else {
            continue;
        };
        if artifact.engine != ProofArtifact::ENGINE_MIR {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries engine '{}'",
                result.func_name, artifact.engine
            ));
        }
        if artifact.source_hash != source_hash {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries a different source_hash",
                result.func_name
            ));
        }
        if artifact.mir_hash != receipt.mir_digest {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries a different mir_hash",
                result.func_name
            ));
        }
        if artifact.mir_route_receipt.as_ref() != Some(receipt) {
            return Err(format!(
                "MIR-PROVENANCE-001: {consumer} result {index} for '{}' carries a different route receipt",
                result.func_name
            ));
        }
    }
    Ok(())
}

/// Replay a canonical MIR verification from the line-oriented receipt
/// manifest emitted by `mimi mir --receipt`. Parsing is strict and the
/// reconstituted receipt is still checked against the immutable MIR graph
/// before any proof observation is produced.
pub fn verify_mir_with_route_manifest(
    program: &crate::core::mir::reference::MirProgram,
    manifest: &str,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let receipt = crate::core::mir::CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)
        .map_err(|message| {
            format!(
                "{}: canonical route manifest rejected: {message}",
                crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE
            )
        })?;
    verify_mir_with_route_receipt(program, &receipt, source_hash)
}

/// Decide whether verifier observations are compatible with an execution
/// route.  This is not a proof verdict: `NotInTrustedSubset` remains visible
/// to callers and is only tolerated for the two runtime-only boundaries whose
/// native and bytecode consumers have an executable MIR contract — the
/// recoverable Flow/f64 island and the multi-target Flow union face.
pub fn canonical_execution_route_verifier_ready(
    results: &[VerificationResult],
    allow_recoverable_float_boundary: bool,
    allow_flow_union_boundary: bool,
) -> bool {
    if !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
        && results.iter().any(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.mir_route_receipt.as_ref().is_some_and(|receipt| {
                    receipt.profile == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
                })
            })
        })
    {
        return false;
    }
    let runtime_only_boundary = |result: &VerificationResult| {
        result.status == VerifStatus::NotInTrustedSubset
            && ((allow_recoverable_float_boundary
                && result
                    .message
                    .contains(crate::core::mir::types::MIR_VERIFIER_FLOAT_BOUNDARY_CODE))
                || (allow_flow_union_boundary
                    && result
                        .message
                        .contains(crate::core::mir::types::MIR_VERIFIER_FLOW_UNION_BOUNDARY_CODE)))
    };
    results.iter().all(|result| {
        matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
        ) || runtime_only_boundary(result)
    })
}

/// Check whether a canonical MIR program is fully consumable by the current
/// MIR verifier capability.  This is a structural route gate, not a contract
/// verdict; callers use it before selecting a default producer/consumer
/// island.
pub fn validate_mir_capabilities(
    program: &crate::core::mir::reference::MirProgram,
) -> Result<(), Vec<String>> {
    mir_capability::validate_mir_capabilities(program)
}

fn parse_memory_source(source: &str, label: &str) -> Result<crate::ast::File, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    crate::parser::Parser::new_memory(tokens, "verifier.source", label, source)
        .map_err(|error| error.to_string())?
        .parse_file()
        .map_err(|error| error.message)
}

/// Build the retained FFI compatibility input while omitting only the injected
/// stdlib prelude bodies. The prelude remains present in `CheckedProgram` for
/// type resolution and route construction; its implementation details are not
/// user proof targets. The public checked contract route never uses this AST
/// snapshot; only the still-retained FFI declaration adapter does.
fn without_injected_prelude(file: &crate::ast::File) -> crate::ast::File {
    let mut filtered = file.clone();
    let Some(prelude_source) = filtered
        .sources
        .records()
        .iter()
        .find(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
    else {
        return filtered;
    };
    filtered
        .items
        .retain(|item| item_source_id(item) != Some(prelude_source));
    filtered
}

/// Keep checked extern declarations available to the ordinary Flow body
/// verifier while removing their foreign Requires/Ensures from that verifier's
/// assumption and declaration-proof surface. Actor compatibility verification
/// checks called Requires through the dedicated FFI owner and never treats a
/// foreign Ensures as evidence about an implementation.
fn is_actor_proof_compatibility(program: &crate::core::CheckedProgram) -> bool {
    crate::core::mir::scalar_actor_contract_boundary_reason(program).is_some()
}

/// Turn a checker-owned typed verification boundary into an explicit result.
///
/// A deferred request is not permission to reopen the retained surface AST:
/// the boundary already names the owner and the unsupported proof shape.  The
/// public checked verifier therefore reports one deterministic
/// `NotInTrustedSubset` result per owner and leaves the legacy body owner
/// unreachable.  Canonical MIR profiles are selected before this adapter and
/// never produce a deferred result.
fn deferred_verification_results(
    program: &crate::core::CheckedProgram,
    boundaries: &[crate::core::mir::verification::MirVerificationBoundary],
) -> Vec<VerificationResult> {
    use std::collections::BTreeMap;

    let mut reasons = BTreeMap::<crate::core::NodeId, Vec<&str>>::new();
    for boundary in boundaries {
        reasons
            .entry(boundary.owner.clone())
            .or_default()
            .push(boundary.reason.as_str());
    }
    // A request-level boundary makes the whole proof batch deferred. Preserve
    // every affected callable owner in the result set, including callers that
    // only carry a checker-owned precondition obligation and therefore do not
    // have a direct contract node of their own.
    for (owner, callable) in program.callables() {
        let prelude = program.functions().get(owner).is_some_and(|function| {
            program
                .source_registry()
                .key(function.origin.user_span().source_id)
                .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
        });
        if prelude
            || (!callable.has_proof_obligations()
                && !crate::verifier::resolved_expr::has_resolved_callsite_obligations(
                    callable,
                    program.callables(),
                    program.resolved_types(),
                ))
        {
            continue;
        }
        reasons
            .entry(owner.clone())
            .or_default()
            .push("the request contains an unresolved checker boundary");
    }
    reasons
        .into_iter()
        .map(|(owner, reasons)| {
            let name = program
                .functions()
                .get(&owner)
                .map(|function| function.qualified_name.clone())
                .or_else(|| {
                    program.transitions().values().find_map(|transition| {
                        (transition.node_id == owner)
                            .then(|| format!("{}::{}", transition.id.flow.0, transition.id.event))
                    })
                })
                .unwrap_or_else(|| owner.0.clone());
            let reason = reasons.join("; ");
            let message = format!(
                "{}: typed verifier request for '{}' remains deferred: {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                name,
                reason
            );
            let span = program
                .node_meta()
                .get(&owner)
                .map(|meta| meta.origin.user_span())
                .or_else(|| program.entry_span())
                .unwrap_or(crate::span::Span::UNKNOWN);
            VerificationResult {
                func_name: name,
                status: VerifStatus::NotInTrustedSubset,
                message: message.clone(),
                diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    message,
                    span,
                )),
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
            }
        })
        .collect()
}

/// Enforce the checked verifier's owner-scoped V2 coverage boundary. MIR
/// observations remain useful for staged diagnostics, but neither staged nor
/// incomplete coverage may leave a proof-shaped result for one of its source
/// owners. Results for unrelated callable owners stay intact alongside these
/// explicit boundaries.
pub(super) fn enforce_ordinary_map_owned_any_v2_coverage(
    program: &crate::core::CheckedProgram,
    mut results: Vec<VerificationResult>,
) -> Vec<VerificationResult> {
    let admission =
        crate::core::mir::classify_canonical_mir_route_admission(program).ordinary_map_owned_any_v2;
    let coverage = match admission {
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::StagedGetValues => "StagedGetValues",
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage => {
            "IncompleteCoverage"
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
            if !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY =>
        {
            "CompleteCoverage (production readiness disabled)"
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::OutsideProfile
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage => return results,
    };
    let owners = program
        .resource_analyses()
        .iter()
        .filter_map(|(owner, analysis)| {
            (!analysis.ordinary_map_owned_any_v2_actions.is_empty()
                || !analysis
                    .ordinary_map_owned_any_v2_operation_attempts
                    .is_empty())
            .then_some(owner.clone())
        })
        .collect::<std::collections::BTreeSet<_>>();
    for owner in owners {
        let name = program
            .functions()
            .get(&owner)
            .map(|function| function.qualified_name.clone())
            .unwrap_or_else(|| owner.0.clone());
        let message = format!(
            "{}: ordinary Map/Any owner-v2 callable '{}' remains NotInTrustedSubset because Checker admission is {coverage} and V2 production readiness is disabled",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            name
        );
        let span = program
            .node_meta()
            .get(&owner)
            .map(|meta| meta.origin.user_span())
            .or_else(|| program.entry_span())
            .unwrap_or(crate::span::Span::UNKNOWN);
        let mut matched = false;
        for result in &mut results {
            // The owner NodeId is the identity boundary. Public result rows
            // use the checked callable's qualified source name, so match it
            // exactly (or its canonical `function:` spelling) rather than
            // relying on substring/name suffix heuristics.
            let result_owner = result
                .func_name
                .strip_prefix("function:")
                .unwrap_or(&result.func_name);
            if result_owner != name && result.func_name != owner.0 {
                continue;
            }
            matched = true;
            result.status = VerifStatus::NotInTrustedSubset;
            result.message = message.clone();
            result.diagnostic = Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message.clone(),
                span,
            ));
            result.duration_us = 0;
            result.constraint_count = 0;
            result.artifact = None;
            result.trusted_subset_domain = Some(TrustedSubsetDomain::Contract);
        }
        if !matched {
            results.push(VerificationResult {
                func_name: format!("{name} [ordinary-map-owned-any-v2 coverage]"),
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
        }
    }
    results
}

/// Make the public MIR proof API's result batch visibly diagnostic while
/// owner-v2 production readiness is closed. The verifier may still analyze
/// the MIR, but neither a proof artifact nor a `Proven` owner row can be fed
/// through the public execution-readiness predicate.
fn enforce_ordinary_map_owned_any_v2_mir_readiness(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    mut results: Vec<VerificationResult>,
) -> Vec<VerificationResult> {
    if crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
        || receipt.profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
    {
        return results;
    }
    for (owner, function) in program.functions().iter().filter(|(_, function)| {
        function
            .ownership
            .has_checker_ordinary_map_owned_any_v2_actions()
    }) {
        let message = format!(
            "{}: ordinary Map/Any owner-v2 callable '{}' is diagnostic-only while V2 production readiness is disabled",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            owner.0
        );
        let mut matched = false;
        for result in &mut results {
            if result.func_name != owner.0 {
                continue;
            }
            matched = true;
            result.status = VerifStatus::NotInTrustedSubset;
            result.message = message.clone();
            result.diagnostic = Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message.clone(),
                crate::span::Span::UNKNOWN,
            ));
            result.duration_us = 0;
            result.constraint_count = 0;
            result.artifact = None;
            result.trusted_subset_domain = Some(TrustedSubsetDomain::Contract);
        }
        if !matched {
            results.push(VerificationResult {
                func_name: owner.0.clone(),
                status: VerifStatus::NotInTrustedSubset,
                message: message.clone(),
                diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    message,
                    crate::span::Span::UNKNOWN,
                )),
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
            });
        }
    }
    results
}

fn actor_ffi_error_result(message: String) -> VerificationResult {
    let resolution_error = message.starts_with("TOOL-RESOLUTION-001:");
    VerificationResult {
        func_name: "extern call-site obligations".into(),
        status: if resolution_error {
            VerifStatus::InfrastructureError
        } else {
            VerifStatus::NotInTrustedSubset
        },
        message: format!("FFI call-site verification failed closed: {message}"),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
    }
}

pub(crate) fn actor_ffi_results_or_boundary(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Vec<VerificationResult> {
    match verify_ffi_checked_with_source_hash(program, source_hash) {
        Ok(results) => results,
        Err(message) => vec![actor_ffi_error_result(message)],
    }
}

/// Build the no-solver result set for the checked Actor compatibility route.
///
/// The ordinary mock verifier is still the result owner for accepted callable
/// proofs, so they stay explicit infrastructure failures with no artifact.
/// Preserve owner-specific checker boundaries alongside those rows: a
/// deferred owner is a coverage result, not a solver failure. FFI remains on
/// its dedicated checked call-site route, which distinguishes Requires from
/// foreign Ensures even when Z3 cannot be created.
fn actor_compatibility_no_z3_results(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Vec<VerificationResult> {
    use std::collections::{BTreeMap, BTreeSet};

    let boundaries = match mir_request::prepare(program) {
        mir_request::Preparation::Deferred(boundaries) => boundaries,
        mir_request::Preparation::Complete(_) => Vec::new(),
    };
    let mut owner_reasons = BTreeMap::<crate::core::NodeId, Vec<String>>::new();
    for boundary in boundaries {
        // The Actor/impl graph boundary applies to the overall compatibility
        // request. Keep it from downgrading every otherwise eligible scalar
        // proof owner; only a boundary attached to that owner becomes NITS.
        if boundary.owner.0 == "verification:request" || boundary.owner.0 == "verification:consumer"
        {
            continue;
        }
        let Some(callable) = program.callable(&boundary.owner) else {
            continue;
        };
        let has_callsite_obligations = resolved_expr::has_resolved_callsite_obligations(
            callable,
            program.callables(),
            program.resolved_types(),
        );
        if !callable.has_proof_obligations() && !has_callsite_obligations {
            continue;
        }
        owner_reasons
            .entry(boundary.owner)
            .or_default()
            .push(boundary.reason);
    }

    let mut deferred = owner_reasons
        .into_iter()
        .map(|(owner, reasons)| {
            let name = program
                .functions()
                .get(&owner)
                .map(|function| function.qualified_name.clone())
                .or_else(|| {
                    program.transitions().values().find_map(|transition| {
                        (transition.node_id == owner)
                            .then(|| format!("{}::{}", transition.id.flow.0, transition.id.event))
                    })
                })
                .unwrap_or_else(|| owner.0.clone());
            let message = format!(
                "{}: typed Actor proof owner '{}' remains outside the admitted proof request: {}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                name,
                reasons.join("; ")
            );
            let span = program
                .node_meta()
                .get(&owner)
                .map(|meta| meta.origin.user_span())
                .or_else(|| program.entry_span())
                .unwrap_or(crate::span::Span::UNKNOWN);
            VerificationResult {
                func_name: name,
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
            }
        })
        .collect::<Vec<_>>();

    // Declaration expressions do not have callable entries in the mock
    // result table. Retain proof-bearing user initializers as span-bearing
    // owner boundaries rather than silently dropping them from an Actor
    // proof census. Extern roots are classified by the FFI call-site adapter.
    for root in program.declaration_expression_bodies().values() {
        if matches!(
            &root.kind,
            crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternRequires
                | crate::core::ir::lower::ResolvedDeclarationExpressionKind::ExternEnsures
        ) {
            continue;
        }
        let has_obligation = match &root.body {
            Ok(body) => body.has_proof_obligations(),
            Err(_) => true,
        };
        if !has_obligation {
            continue;
        }
        let message = format!(
            "{}: typed Actor declaration proof owner '{}' is outside the admitted proof request",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            root.display_name
        );
        let span = program
            .node_meta()
            .get(&root.node_id)
            .or_else(|| program.node_meta().get(&root.owner))
            .map(|meta| meta.origin.user_span())
            .or_else(|| {
                root.body
                    .as_ref()
                    .ok()
                    .map(|body| body.root.origin.user_span())
            })
            .or_else(|| program.entry_span())
            .unwrap_or(crate::span::Span::UNKNOWN);
        deferred.push(VerificationResult {
            func_name: root.display_name.clone(),
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
    }

    let deferred_names = deferred
        .iter()
        .map(|result| result.func_name.clone())
        .collect::<BTreeSet<_>>();
    let mut results = ctx::mock_verify_checked(program);
    results.retain(|result| {
        !result.func_name.starts_with("extern ") && !deferred_names.contains(&result.func_name)
    });
    results.extend(deferred);
    results.extend(actor_ffi_results_or_boundary(program, source_hash));
    results
}

fn item_source_id(item: &crate::ast::Item) -> Option<crate::span::SourceId> {
    use crate::ast::Item;
    let span = match item {
        Item::Func(item) => item.meta.span,
        Item::Type(item) => item.meta.span,
        Item::Actor(item) => item.meta.span,
        Item::Cap(item) => item.meta.span,
        Item::Trait(item) => item.meta.span,
        Item::Impl(item) => item.meta.span,
        Item::ExternBlock(item) => item.meta.span,
        Item::Const { meta, .. } => meta.span,
        Item::Flow(item) => item.meta.span,
        Item::Session(item) => item.meta.span,
    };
    span.source_id.is_known().then_some(span.source_id)
}

/// Verify contracts in source text.
pub fn verify_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let file = parse_memory_source(source, "contracts")?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    // P1-24: compute source hash for tamper detection in ProofArtifact.
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    verify_checked(&program, source_hash)
}

/// Verify contracts using a caller-provided verifier (for timeout/config tests).
pub fn verify_source_with(
    source: &str,
    verifier: &mut Verifier,
) -> Result<Vec<VerificationResult>, String> {
    let file = parse_memory_source(source, "contracts")?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    // P1-24: compute source hash for tamper detection in ProofArtifact.
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    // A caller-supplied verifier is the retained compatibility engine used by
    // timeout/configuration tests.  Once Checker admission recognizes an
    // ordinary Map owned-get candidate, keep this source entry on the same
    // canonical route as `verify_checked`; otherwise the configured engine
    // could silently consume the old Resolved body and bypass the owner
    // settlement gate.
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&program);
    if admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission.ordinary_map_owned_any_v2.has_profile_signal()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal()
    {
        validate_public_verifier_mir_boundary(&program)?;
        return verify_checked(&program, source_hash);
    }
    verifier.set_source_hash(source_hash);
    Ok(verifier.verify_checked(&program))
}

/// Enforce the checker-owned boundaries that prevent a recognized but
/// incomplete Canonical MIR profile from falling through to a compatibility
/// verifier. Keep this shared by the single- and dual-engine public APIs so
/// callers cannot obtain different answers merely by selecting an engine
/// wrapper.
fn validate_public_verifier_mir_boundary(
    program: &crate::core::CheckedProgram,
) -> Result<(), String> {
    if let Some(reason) = crate::core::mir::scalar_ffi_boundary_reason(program) {
        return Err(format!(
            "{}: canonical scalar FFI declaration boundary: {reason}",
            crate::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE
        ));
    }

    // Result projection candidates are checker-owned route admissions. Once
    // the front end recognizes an unsupported shape, verification must not
    // silently enter the retained AST/Flow compatibility engine. Complete
    // admission proceeds through the canonical MIR profile below; mixed or
    // unsupported admission is a stable hard error.
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if let Some(reason) =
        crate::core::mir::unsupported_nested_callable_boundary_reason(program, None)
    {
        // Actor proof compatibility deliberately retains the complete checker
        // owner census. A dormant nested owner can be outside the scalar
        // execution slice while its proof result still belongs in the
        // compatibility verifier; that verifier keeps the shape explicitly
        // non-Proven. Every other route keeps this checker-owned boundary
        // hard.
        if !is_actor_proof_compatibility(program) {
            return Err(format!(
                "{}: {reason}",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
    }
    if admission.map_root.is_candidate() && !admission.map_root_complete() {
        return Err(format!(
            "{}: Checker recognized a MapRoot candidate but withheld its complete local lifecycle receipt",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.map_any_owner.is_candidate() && !admission.map_any_owner_complete() {
        return Err(format!(
            "{}: Checker recognized a MapAnyOwner v1 candidate but withheld its complete independent lifecycle receipt",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map/Any owner-v2 candidate but withheld complete operation-attempt and result-receipt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission
        .ordinary_map_owned_get_i32_string_v1
        .has_profile_signal()
        && !admission.ordinary_map_owned_get_i32_string_v1_complete()
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map owned-get attempt without complete consumer coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if let Some(reason) =
        crate::core::mir::ordinary_map_owned_get_result_settlement_reason_checked(program)
    {
        return Err(format!(
            "{}: {reason}",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    // Ordinary Map attempts outside the first owner lifecycle slice remain
    // on the legacy verifier by design. Direct MIR construction/materializing
    // APIs still reject this partial graph, while a separate complete V1
    // receipt may cover every ordinary attempt through its explicit bridge.
    if !admission.scalar_ffi
        && matches!(
            admission.flow,
            crate::core::mir::S8FlowAdmission::MixedCoverage
        )
    {
        return Err(
            "MIR-COVERAGE-001: S8 Flow transition candidate is not complete coverage".into(),
        );
    }
    if matches!(
        admission.generic_variant,
        crate::core::mir::GenericVariantPredicateAdmission::MixedCoverage
    ) || (!admission.generic_variant_complete()
        && crate::core::mir::has_generic_variant_predicate_operation_candidate(program))
    {
        return Err(
            "MIR-COVERAGE-001: generic variant predicate candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.record,
        crate::core::mir::FlatCopyRecordAdmission::MixedCoverage
    ) && crate::core::mir::has_unsupported_generic_record_projection_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic record projection candidate did not materialize a supported scalar MIR shape"
                .into(),
        );
    }
    if matches!(
        admission.record,
        crate::core::mir::FlatCopyRecordAdmission::MixedCoverage
    ) && crate::core::mir::has_unsupported_generic_record_update_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic record update candidate did not materialize a supported scalar MIR shape"
                .into(),
        );
    }
    if !matches!(
        admission.collection,
        crate::core::mir::ScalarCollectionAdmission::OutsideProfile
    ) && crate::core::mir::has_unsupported_list_reverse_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: canonical List.reverse candidate did not materialize a supported MIR shape"
                .into(),
        );
    }
    if !matches!(
        admission.collection,
        crate::core::mir::ScalarCollectionAdmission::OutsideProfile
    ) && crate::core::mir::has_unsupported_list_concat_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: canonical List.concat candidate did not materialize a supported MIR shape"
                .into(),
        );
    }
    if !matches!(
        admission.collection,
        crate::core::mir::ScalarCollectionAdmission::OutsideProfile
    ) && crate::core::mir::has_unsupported_generic_list_facade_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: canonical generic List facade candidate did not materialize a supported scalar MIR shape"
                .into(),
        );
    }
    if admission.collection_complete()
        && crate::core::mir::has_unsupported_generic_set_facade_candidate(program)
    {
        return Err("MIR-COVERAGE-001: canonical generic Set facade construction failed".into());
    }
    if matches!(
        admission.generic_option_projection,
        crate::core::mir::GenericOptionProjectionAdmission::MixedCoverage
    ) || crate::core::mir::has_unsupported_generic_option_projection_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic Option projection candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.generic_option_projection_fallback,
        crate::core::mir::GenericOptionProjectionFallbackAdmission::MixedCoverage
    ) || crate::core::mir::has_unsupported_generic_option_projection_fallback_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic Option fallback projection candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.generic_result_projection,
        crate::core::mir::GenericResultProjectionAdmission::MixedCoverage
    ) || crate::core::mir::has_unsupported_generic_result_projection_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic Result projection candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.generic_result_projection_fallback,
        crate::core::mir::GenericResultProjectionFallbackAdmission::MixedCoverage
    ) || crate::core::mir::has_unsupported_generic_result_projection_fallback_candidate(program)
    {
        return Err(
            "MIR-COVERAGE-001: generic Result fallback projection candidate is outside complete coverage"
                .into(),
        );
    }
    if matches!(
        admission.managed_result_call,
        crate::core::mir::ManagedResultCallAdmission::MixedCoverage
    ) || (crate::core::mir::has_managed_result_call_candidate(program)
        && !matches!(
            admission.managed_result_call,
            crate::core::mir::ManagedResultCallAdmission::CompleteCoverage
        ))
    {
        return Err(
            "MIR-COVERAGE-001: managed Result direct-call candidate is outside complete coverage"
                .into(),
        );
    }
    for (mixed, profile) in [
        (
            matches!(
                admission.copy_option_i32,
                crate::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
            ),
            "Copy Option<i32>",
        ),
        (
            matches!(
                admission.copy_option_bool,
                crate::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
            ),
            "Copy Option<bool>",
        ),
        (
            matches!(
                admission.copy_option_i64,
                crate::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
            ),
            "Copy Option<i64>",
        ),
        (
            matches!(
                admission.copy_option_f64,
                crate::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
            ),
            "Copy Option<f64>",
        ),
    ] {
        if mixed {
            return Err(format!(
                "MIR-COVERAGE-001: {profile} projection candidate is outside complete coverage"
            ));
        }
    }
    if matches!(
        admission.copy_result_i32,
        crate::core::mir::CopyResultI32VariantAdmission::MixedCoverage
    ) {
        return Err(
            "MIR-COVERAGE-001: Copy Result<i32, i32> projection candidate is outside complete coverage"
                .into(),
        );
    }

    Ok(())
}

/// Verify contracts in a type-checked program (supports pre-merged imports).
///
/// `source_hash` is the BLAKE3 hash of the source text (for ProofArtifact
/// tamper detection). Pass an empty string if source text is unavailable.
///
/// Closed scalar FFI, scalar collection, flat Copy-record, S8 Flow, recoverable
/// Flow, and exact non-Copy `Option<string>` programs are verified from one
/// canonical MIR graph. A typed request outside those profiles returns an
/// explicit checker-owned coverage result; it never reopens a retained
/// surface-body compatibility owner. When Z3 is unavailable, the existing
/// CheckedProgram infrastructure result remains the only fallback.
///
/// The Resolved IR contract path (`verify_checked_contracts`) remains available
/// through callers that hold a configured `Verifier`; this public checked entry
/// uses the canonical MIR request or reports a typed coverage boundary.
pub fn verify_checked(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    program
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    // Run the checker-owned route boundary before MIR request preparation.
    // `mir_request::prepare` can complete and return directly when Z3 is
    // available; placing this check below that early return would let an
    // incomplete ordinary Map/GetAny or Values owner reach a proof engine.
    validate_public_verifier_mir_boundary(program)?;
    let z3_available = is_z3_available();
    let actor_compatibility = is_actor_proof_compatibility(program);
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::StagedGetValues
    {
        return verify_staged_ordinary_map_owned_any_v2(program, source_hash);
    }
    // A complete owner-v2 profile remains diagnostic-only while the shared
    // production gate is closed. Keep this ahead of the generic typed request
    // fast path so a future request model cannot produce a proof artifact for
    // an owner whose backend route is not production-ready.
    if z3_available
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
        && !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
    {
        return verify_closed_mir_program(program, source_hash)?.ok_or_else(|| {
            format!(
                "{}: complete ordinary Map/Any owner-v2 diagnostic verifier did not materialize a route",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        });
    }
    let deferred_boundaries = if z3_available {
        match mir_request::prepare(program) {
            mir_request::Preparation::Complete(request) => {
                return mir_request::verify(&request, program, source_hash);
            }
            mir_request::Preparation::Deferred(boundaries) => Some(boundaries),
        }
    } else {
        None
    };
    if !z3_available && actor_compatibility {
        return Ok(actor_compatibility_no_z3_results(program, source_hash));
    }
    if !z3_available {
        // A closed MIR profile cannot produce a proof without its solver.
        // Keep the checked no-solver result algebra instead of materializing
        // that profile and failing during SolverSession initialization.
        return Ok(ctx::mock_verify_checked(program));
    }
    match verify_closed_mir_program(program, source_hash.clone()) {
        Ok(Some(results))
            if deferred_boundaries
                .as_deref()
                .is_some_and(|boundaries| !boundaries.is_empty())
                && results.iter().any(|result| {
                    result
                        .message
                        .contains("proof worklist coverage gap: DeferredBoundary")
                }) =>
        {
            return Ok(deferred_verification_results(
                program,
                deferred_boundaries.as_deref().unwrap_or_default(),
            ));
        }
        Ok(Some(results)) => return Ok(results),
        Ok(None) => {}
        Err(error)
            if deferred_boundaries
                .as_deref()
                .is_some_and(|boundaries| !boundaries.is_empty())
                && error.contains("proof worklist coverage gap: DeferredBoundary") =>
        {
            // The checker has already sealed this owner as a deferred proof
            // boundary.  A later scalar island probe may discover that the
            // same body cannot be materialized, but that is still an
            // owner-scoped coverage result, not a verifier infrastructure
            // failure.  Preserve the typed boundary and never reopen the
            // retained AST/Flow owner.
            return Ok(deferred_verification_results(
                program,
                deferred_boundaries.as_deref().unwrap_or_default(),
            ));
        }
        Err(error) => return Err(error),
    }
    if actor_compatibility {
        let mut verifier = Verifier::new()?;
        verifier.set_source_hash(source_hash);
        return Ok(verifier.verify_checked(program));
    }
    if let Some(boundaries) = deferred_boundaries.as_deref() {
        // A typed request that did not reach a closed MIR profile is an
        // explicit coverage boundary.  Do not reconstruct a surface file or
        // invoke the retired FlowVerifierCompatibility owner: the checker has
        // already identified the exact owner and unsupported proof shape.
        return Ok(deferred_verification_results(program, boundaries));
    }
    // No callable proof roots were requested. The old Flow verifier used to
    // return an empty batch here after reading the source file; an empty typed
    // result is equivalent and keeps the AST owner unreachable.
    Ok(Vec::new())
}

/// Parse source and verify extern call sites using Z3.
pub fn verify_ffi_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let file = parse_memory_source(source, "ffi-call-sites")?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    verify_ffi_checked_with_source_hash(
        &program,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
}

/// Verify extern call sites from a checked program.
///
/// Complete scalar C ABIs consume the shared canonical MIR route and
/// call-site receipts, including scalar `requires`/`ensures` predicates.
/// Declarations outside that profile stay checker-owned as explicit
/// `MIR-COVERAGE-001` observations at their declaration/call-site spans;
/// they never reopen a retained source body. Compatibility never claims an
/// `ensures` proof. Declarations and calls with no FFI obligations return an
/// empty result, while malformed identity/arity and mixed canonical owners
/// fail closed before either verifier route.
pub fn verify_ffi_checked(
    program: &crate::core::CheckedProgram,
) -> Result<Vec<VerificationResult>, String> {
    verify_ffi_checked_with_source_hash(program, String::new())
}

/// Verify FFI obligations in an already-materialized canonical MIR program.
///
/// Build and other route owners use this adapter when they already hold the
/// checker-approved scalar FFI `MirProgram`. Keeping the verifier on that
/// exact object prevents a second frontend/materialization pass from drifting
/// the route receipt between proof and code generation. The input is still
/// validated at the verifier consumer boundary and never opens a legacy AST
/// compatibility owner.
pub fn verify_ffi_mir(
    program: &crate::core::mir::reference::MirProgram,
) -> Result<Vec<VerificationResult>, String> {
    verify_ffi_mir_with_source_hash(program, String::new())
}

/// Verify an already-materialized canonical FFI MIR program while preserving
/// the caller's source provenance in each proof artifact.
///
/// `source_hash` should be the BLAKE3 hash of the source text that produced
/// the checked program. The MIR identity remains independent of this value;
/// callers can therefore compare one canonical route across entry points
/// while still detecting which source snapshot requested the proof.
pub fn verify_ffi_mir_with_source_hash(
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let receipt = program.route_receipt("verifier-mir-v1");
    let results =
        verify_ffi_mir_results_with_route_receipt(program, source_hash.clone(), &receipt)?;
    validate_mir_result_provenance(&results, &receipt, &source_hash, "verify_ffi_mir")?;
    Ok(results)
}

fn verify_ffi_mir_results_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
) -> Result<Vec<VerificationResult>, String> {
    validate_mir_capabilities(program).map_err(|errors| {
        format!("MIR-FFI-CAPABILITY-001: canonical verifier rejected scalar FFI MIR: {errors:?}")
    })?;
    mir::verify_ffi_program_with_route_receipt(program, source_hash, receipt)
}

/// Verify canonical FFI MIR after checking the caller-supplied route receipt.
/// The verifier therefore proves the same immutable graph that the execution
/// consumers admitted, including its ABI and FFI sub-digests.
pub fn verify_ffi_mir_with_route_receipt(
    program: &crate::core::mir::reference::MirProgram,
    receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    receipt
        .validate_against_program(program)
        .map_err(|message| {
            format!(
                "{}: canonical route receipt rejected: {message}",
                crate::core::mir::MIR_FFI_ROUTE_RECEIPT_ERROR_CODE
            )
        })?;
    let results = verify_ffi_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?;
    validate_mir_result_provenance(
        &results,
        receipt,
        &source_hash,
        "verify_ffi_mir_with_route_receipt",
    )?;
    Ok(results)
}

/// Replay the FFI-only canonical verifier from a CLI route receipt manifest.
pub fn verify_ffi_mir_with_route_manifest(
    program: &crate::core::mir::reference::MirProgram,
    manifest: &str,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let receipt = crate::core::mir::CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)
        .map_err(|message| {
            format!(
                "{}: canonical route manifest rejected: {message}",
                crate::core::mir::MIR_FFI_ROUTE_MANIFEST_ERROR_CODE
            )
        })?;
    verify_ffi_mir_with_route_receipt(program, &receipt, source_hash)
}

/// Verify FFI call-site obligations from a checked program and preserve the
/// source snapshot identity in any canonical proof artifacts.
pub fn verify_ffi_checked_with_source_hash(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    // CheckedProgram owns declaration identity and arity. Validate them before
    // deciding whether this API has an obligation, so malformed call-site
    // metadata cannot be hidden by an otherwise empty result.
    let mut has_called_extern = false;
    let mut called_contract_names = std::collections::BTreeSet::new();
    let mut called_requires_names = std::collections::BTreeSet::new();
    let mut unmodeled_ensures_call_sites = Vec::new();
    for site in program.call_sites_sorted() {
        if site.kind != crate::core::ResolvedCallKind::Extern {
            continue;
        }
        has_called_extern = true;
        let same_identity_count = program
            .call_sites()
            .values()
            .filter(|candidate| candidate.node_id == site.node_id)
            .count();
        if !program.call_sites().contains_key(&site.node_id) || same_identity_count != 1 {
            return Err(format!(
                "TOOL-RESOLUTION-001: malformed extern call-site identity '{}'",
                site.node_id.0
            ));
        }
        if !site.arity_matches() {
            return Err(format!(
                "TOOL-RESOLUTION-001: extern call '{}' has inconsistent call-site arity metadata: expected {}, got {}",
                site.callee,
                site.expected_argc.unwrap_or(site.argc),
                site.argc
            ));
        }
        let declarations = program
            .ffi_declarations()
            .values()
            .filter(|declaration| declaration.symbol == site.callee)
            .collect::<Vec<_>>();
        if declarations.len() != 1 {
            return Err(format!(
                "TOOL-RESOLUTION-001: extern call '{}' resolves to {} typed declaration descriptors",
                site.callee,
                declarations.len()
            ));
        }
        let declaration = declarations[0];
        declaration
            .validate(program)
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        if site.argc != declaration.fixed_parameter_ids.len() {
            return Err(format!(
                "TOOL-RESOLUTION-001: extern call '{}' expects {} arguments, got {}",
                site.callee,
                declaration.fixed_parameter_ids.len(),
                site.argc
            ));
        }
        if declaration.has_declared_contract() {
            called_contract_names.insert(declaration.symbol.clone());
        }
        if declaration.requires.is_declared() {
            called_requires_names.insert(declaration.symbol.clone());
        }
        if declaration.ensures.is_declared() {
            unmodeled_ensures_call_sites.push(site.clone());
        }
    }

    if !has_called_extern {
        // An uncalled extern declaration, or a contract on an unrelated Mimi
        // callable, must not enter an FFI route or manufacture a proof status.
        return Ok(Vec::new());
    }

    // A checker-owned declaration whose only call-site obligations are
    // boolean literals has no caller state to encode.  Keep this narrow
    // declaration-root slice before the scalar ABI and compatibility routes:
    // a non-scalar declaration with `requires: true`/`false` is still fully
    // decidable from the typed declaration and must not reopen the retained
    // FFI body merely because its ABI is outside ScalarFfi.
    if !called_contract_names.is_empty() {
        if let Some(results) = verify_ffi_literal_contracts(program, &source_hash) {
            return results;
        }
    }

    let profile = crate::core::mir::CanonicalMirRouteProfile::ScalarFfi;
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    let actor_compatibility = is_actor_proof_compatibility(program);
    if profile.is_admitted(admission) && !actor_compatibility {
        // Once Checker admits the closed scalar profile, every construction
        // or consumer failure is hard. In particular, do not retry through
        // the compatibility owner if canonical materialization fails or Z3
        // is unavailable.
        let route = crate::core::mir::materialize_canonical_mir_route(program, None).map_err(
            |error| match error.diagnostic_code() {
                Some(code) => format!("{code}: {error}"),
                None => format!(
                    "{}: complete ScalarFfi admission entered compatibility materialization: {error}",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            },
        )?;
        if !profile.is_materialized(&route) {
            return Err(format!(
                "{}: complete ScalarFfi admission did not materialize a canonical verifier graph",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        if called_contract_names.is_empty() {
            // Route validation still owns every real call, even when there is
            // no contract root to prove. Do not ask either proof engine to
            // manufacture a result for an obligation-free declaration.
            return Ok(Vec::new());
        }
        if is_z3_available() {
            let canonical = route.program;
            let receipt = canonical.route_receipt("verify-ffi-v1");
            let mut results =
                verify_ffi_mir_with_route_receipt(&canonical, &receipt, source_hash.clone())?;
            // Declaration defaults are distinct source roots, not body-call
            // facts. Replay only the locally supported checked Math profile;
            // preserve all other default/context roots as their own NITS
            // results so a successful FFI call cannot mask them.
            results.extend(ffi_checked::verify_default_math_roots(
                program,
                &called_contract_names,
                &source_hash,
            )?);
            for deferred in ffi_checked::deferred_context_results(program, &called_contract_names) {
                if let Some(existing) = results
                    .iter_mut()
                    .find(|existing| existing.func_name == deferred.func_name)
                {
                    *existing = deferred;
                } else {
                    results.push(deferred);
                }
            }
            return Ok(results);
        }
        return Ok(called_contract_names
            .into_iter()
            .map(|name| ffi_infrastructure_error(format!("extern {name}")))
            .collect());
    }

    // Map-owner profiles have independent canonical ownership receipts. A
    // program that combines one with called externs cannot choose the AST
    // verifier for the FFI portion and a canonical owner for the rest.
    // Incomplete Map attempts also remain visible here so they cannot turn a
    // mixed program into a compatibility route.
    let map_owner_candidate = admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal();
    if map_owner_candidate {
        if actor_compatibility {
            if called_contract_names.is_empty() {
                return Err(format!(
                    "{}: called externs cannot borrow an independent canonical Map owner receipt from the Actor compatibility route",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ));
            }
            // The Actor compatibility route cannot borrow a separate Map
            // receipt. Preserve each called FFI obligation as explicitly
            // deferred rather than allowing the Actor selector to bypass the
            // same cross-owner guard enforced by canonical Map consumers.
            let mut results = program
                .call_sites_sorted()
                .into_iter()
                .filter(|site| {
                    site.kind == crate::core::ResolvedCallKind::Extern
                        && called_requires_names.contains(&site.callee)
                })
                .map(|site| ffi_mixed_map_requires_result(program, site))
                .collect::<Vec<_>>();
            results.extend(
                unmodeled_ensures_call_sites
                    .iter()
                    .map(|site| ffi_unmodeled_ensures_result(program, site)),
            );
            return Ok(results);
        }
        return Err(format!(
            "{}: called externs conflict with a separate canonical Map owner profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }

    if called_contract_names.is_empty() {
        // A real but obligation-free extern still has to cross the checked
        // source/ABI and route boundary. Keep the no-obligation result only
        // for a pure compatibility input with no canonical candidate; every
        // incomplete or mixed candidate must remain a hard route error.
        let route = match crate::core::mir::materialize_canonical_mir_route(program, None) {
            Ok(route) => route,
            Err(error) => {
                if let crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility {
                    admission: compatibility_admission,
                    ..
                } = &error
                {
                    if !compatibility_admission.has_candidate() {
                        return Ok(Vec::new());
                    }
                }
                return Err(match error.diagnostic_code() {
                    Some(code) => format!("{code}: {error}"),
                    None => format!(
                        "{}: called extern has no complete canonical ScalarFfi route: {error}",
                        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ),
                });
            }
        };
        if !profile.is_admitted(admission) || !profile.is_materialized(&route) {
            return Err(format!(
                "{}: called extern did not receive a complete canonical ScalarFfi admission and materialization",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        return Ok(Vec::new());
    }

    if !is_z3_available() {
        let mut results = called_requires_names
            .into_iter()
            .map(|name| ffi_infrastructure_error(format!("extern {name}")))
            .collect::<Vec<_>>();
        results.extend(
            unmodeled_ensures_call_sites
                .iter()
                .map(|site| ffi_unmodeled_ensures_result(program, site)),
        );
        return Ok(results);
    }

    let deferred_context = ffi_checked::deferred_context_results(program, &called_contract_names);
    let typed_results = ffi_checked::verify(program, &called_requires_names, &source_hash)?;
    let mut results = if let Some(results) = typed_results {
        results
    } else {
        // The checker has identified an FFI declaration/call-site obligation,
        // but the finite typed encoder did not admit its ABI or context shape.
        // This is an explicit coverage result.  It must never reopen the
        // retained source body: doing so would make the FFI owner depend on
        // the legacy AST verifier after its production edge has been retired.
        program
            .call_sites_sorted()
            .into_iter()
            .filter(|site| {
                site.kind == crate::core::ResolvedCallKind::Extern
                    && called_requires_names.contains(&site.callee)
            })
            .map(|site| ffi_coverage_result(program, &site))
            .collect()
    };
    if actor_compatibility {
        // Actor compatibility deliberately treats a foreign postcondition as
        // a checker-owned boundary.  The finite typed FFI encoder can still
        // derive a scalar Ensures verdict, but that verdict belongs to the
        // standalone ScalarFfi owner and would duplicate the one observation
        // emitted below for each call site.  Keep Requires results and let
        // the explicit deferred observation be the sole Actor Ensures row.
        results.retain(|result| {
            !(result.func_name.contains(" -> extern ") && result.func_name.contains(" Ensures"))
        });
    }
    results.extend(deferred_context);
    results.extend(
        unmodeled_ensures_call_sites
            .iter()
            .map(|site| ffi_unmodeled_ensures_result(program, site)),
    );
    Ok(results)
}

fn ffi_literal_argument_site(
    program: &crate::core::CheckedProgram,
    site: &crate::core::ResolvedCallSite,
) -> Result<bool, String> {
    fn literalish(expression: &crate::core::ir::ResolvedExpr) -> bool {
        match &expression.kind {
            crate::core::ir::ResolvedExprKind::Literal(_) => true,
            crate::core::ir::ResolvedExprKind::Cast { value, .. } => literalish(value),
            _ => false,
        }
    }

    for body in program.resolved_bodies().values() {
        let expressions = body
            .call_expressions(program.resolved_types(), program.resolved_type_targets())
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        if let Some(expression) = expressions.get(&site.node_id) {
            let crate::core::ir::ResolvedExprKind::Call(call) = &expression.kind else {
                continue;
            };
            if !matches!(call.callee, crate::core::ir::ResolvedCallee::Extern(_)) {
                continue;
            }
            return Ok(call
                .arguments
                .iter()
                .all(|argument| literalish(&argument.value)));
        }
    }
    for root in program.declaration_expression_bodies().values() {
        let Ok(body) = &root.body else {
            continue;
        };
        let expressions = body
            .call_expressions(program.resolved_types(), program.resolved_type_targets())
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        if let Some(expression) = expressions.get(&site.node_id) {
            let crate::core::ir::ResolvedExprKind::Call(call) = &expression.kind else {
                continue;
            };
            if !matches!(call.callee, crate::core::ir::ResolvedCallee::Extern(_)) {
                continue;
            }
            return Ok(call
                .arguments
                .iter()
                .all(|argument| literalish(&argument.value)));
        }
    }
    // The public call-site directory remains authoritative. If this optional
    // shortcut cannot bind its expression snapshot, defer to the full typed
    // encoder instead of manufacturing a declaration-root proof or a new
    // resolution failure.
    Ok(false)
}

/// Verify a declaration-root-only FFI request whose called contracts are all
/// boolean literals. No retained callable body is needed for this profile:
/// checker-owned declaration identity, arity, and literal roots determine the
/// result. Returning `None` is an explicit admission miss; any non-literal
/// contract proceeds through the typed call-site encoder or receives a
/// checker-owned `MIR-COVERAGE-001` boundary result.
fn verify_ffi_literal_contracts(
    program: &crate::core::CheckedProgram,
    source_hash: &str,
) -> Option<Result<Vec<VerificationResult>, String>> {
    use crate::ast::{Expr, Lit};

    // Scalar FFI has its own canonical MIR receipt. The declaration-root
    // shortcut is only for non-scalar ABI shapes whose call arguments and
    // roots are independently literal; it must never steal a MIR artifact.
    if crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_admitted(
        crate::core::mir::classify_canonical_mir_route_admission(program),
    ) {
        return None;
    }

    fn bool_literal(expression: Option<&Expr>) -> Option<bool> {
        let expression = expression?.unlocated();
        match expression {
            Expr::Literal(Lit::Bool(value)) => Some(*value),
            _ => None,
        }
    }

    let mut results = Vec::new();
    let mut saw_called_contract = false;
    let mut seen_contracts = std::collections::BTreeSet::new();
    let mut declared_symbols = std::collections::BTreeSet::new();
    let resolved_ir_hash = ctx::compute_resolved_ir_hash(program);

    // Keep the declaration identity gate even though this profile bypasses
    // the body adapter.  Duplicate symbols must remain a resolution error.
    for block in program.extern_blocks_sorted() {
        for signature in crate::core::CheckedProgram::extern_signatures_sorted(block) {
            if !declared_symbols.insert(signature.name.clone()) {
                return Some(Err(format!(
                    "TOOL-RESOLUTION-001: duplicate resolved extern symbol '{}'",
                    signature.name
                )));
            }
        }
    }

    for site in program.call_sites_sorted() {
        if site.kind != crate::core::ResolvedCallKind::Extern {
            continue;
        }
        let caller_owner = crate::core::NodeId(site.owner.clone());
        if program
            .callable(&caller_owner)
            .is_some_and(crate::core::ResolvedCallable::has_proof_obligations)
        {
            // Caller Requires/Ensures, nested roots, defaults, and other
            // effects need the typed call-site encoder. A declaration literal
            // cannot stand in for that context or erase argument definedness.
            return None;
        }
        match ffi_literal_argument_site(program, site) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => return Some(Err(error)),
        }
        let Ok(Some(signature)) = program.extern_func_signature_for_call(&site.callee, site.argc)
        else {
            return Some(Err(format!(
                "TOOL-RESOLUTION-001: missing checker-owned extern declaration for call '{}'",
                site.callee
            )));
        };
        if signature.requires.is_none() && signature.ensures.is_none() {
            continue;
        }
        // The compatibility verifier reports one declaration-level result,
        // even if the declaration is called at several sites.
        if !seen_contracts.insert(signature.name.clone()) {
            continue;
        }
        saw_called_contract = true;

        let requires = bool_literal(signature.requires.as_ref());
        let ensures = bool_literal(signature.ensures.as_ref());
        if (signature.requires.is_some() && requires.is_none())
            || (signature.ensures.is_some() && ensures.is_none())
        {
            // Whole-request admission is required.  Mixed literal and
            // stateful expressions must not split proof ownership.
            return None;
        }

        let status = if requires == Some(false) {
            VerifStatus::Failed
        } else if ensures == Some(false) {
            VerifStatus::Disproven
        } else {
            VerifStatus::Proven
        };
        let message = if status == VerifStatus::Failed {
            format!(
                "extern '{}' has an unsatisfiable literal requires contract",
                signature.name
            )
        } else if status == VerifStatus::Disproven {
            format!(
                "extern '{}' literal declaration ensures contract is disproven",
                signature.name
            )
        } else {
            format!(
                "extern '{}' literal declaration contract is proven from checker-owned roots",
                signature.name
            )
        };
        let diagnostic = (status != VerifStatus::Proven).then(|| {
            crate::diagnostic::Diagnostic::error(message.clone(), site.origin.user_span())
        });
        let mut artifact = ProofArtifact::new(
            "checker-literal-ffi-v1".to_string(),
            source_hash.to_string(),
        );
        artifact.engine = ProofArtifact::ENGINE_RESOLVED.to_string();
        artifact.resolved_ir_hash = resolved_ir_hash.clone();
        artifact.integer_model = "literal-boolean".to_string();
        artifact.float_model = "none".to_string();
        results.push(VerificationResult {
            func_name: format!("extern {}", signature.name),
            status,
            message,
            diagnostic,
            duration_us: 0,
            constraint_count: usize::from(signature.requires.is_some())
                + usize::from(signature.ensures.is_some()),
            artifact: Some(artifact),
            trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
        });
    }

    saw_called_contract.then_some(Ok(results))
}

/// Admit the literal FFI declaration-root profile at the public dual entry
/// point. This exact profile has no body-bearing semantic owner: any
/// callable proof root, Flow/Actor/Session/impl graph, or non-literal/mixed
/// FFI contract misses the profile and is reported through the canonical MIR
/// coverage boundary by the caller.
fn verify_dual_literal_ffi_declaration_root(
    program: &crate::core::CheckedProgram,
    source_hash: &str,
) -> Option<Result<Vec<VerificationResult>, String>> {
    // Scalar FFI already has an independent canonical MIR/dual route.  The
    // declaration-root slice only covers ABI shapes that still lack that
    // route; it must not replace a scalar MIR proof or steal a mixed Map
    // owner request.
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_admitted(admission)
        || admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission.ordinary_map_owned_any_v2.has_profile_signal()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal()
    {
        return None;
    }
    // `check_program` injects the RuntimeSystem `Main` flow for ordinary
    // scripts. It carries no user contract and is safe to ignore here, but
    // an explicit/user Flow (or a second generated flow) keeps the profile
    // deferred to the normal dual owner.
    let implicit_main_flow_nodes = program
        .flows()
        .values()
        .filter(|flow| {
            flow.id.0 == "Main"
                && matches!(
                    &flow.origin,
                    crate::core::Origin::RuntimeSystem { rule, .. }
                        if rule == "progressive.main"
                )
        })
        .map(|flow| flow.node_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let flows_are_only_implicit_main = !implicit_main_flow_nodes.is_empty()
        && program.flows().len() == implicit_main_flow_nodes.len()
        && program.transitions().values().all(|transition| {
            matches!(
                &transition.origin,
                crate::core::Origin::RuntimeSystem { parent, .. }
                    if implicit_main_flow_nodes.contains(parent)
            )
        });
    if (!program.flows().is_empty() && !flows_are_only_implicit_main)
        || !program.actors().is_empty()
        || !program.sessions().is_empty()
        || !program.impls().is_empty()
    {
        return None;
    }
    if program
        .callables()
        .values()
        .any(crate::core::ResolvedCallable::has_proof_obligations)
    {
        return None;
    }
    verify_ffi_literal_contracts(program, source_hash)
}

fn ffi_unmodeled_ensures_result(
    program: &crate::core::CheckedProgram,
    site: &crate::core::ResolvedCallSite,
) -> VerificationResult {
    let caller_owner = crate::core::NodeId(site.owner.clone());
    let caller = ffi_checked::caller_name(program, &caller_owner);
    let declaration_owner = program
        .ffi_declarations()
        .values()
        .find(|declaration| declaration.symbol == site.callee)
        .map(|declaration| declaration.declaration.0.as_str())
        .unwrap_or("<missing>");
    let message = format!(
        "{}: checker-owned FFI declaration '{}' (owner '{}') Ensures at call site '{}' (owner '{}', span {}) is outside canonical proof coverage; any Requires result is independent and does not prove the foreign postcondition",
        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
        site.callee,
        declaration_owner,
        site.node_id.0,
        caller_owner.0,
        site.origin.user_span()
    );
    VerificationResult {
        func_name: format!("{} -> extern {} Ensures", caller, site.callee),
        status: VerifStatus::NotInTrustedSubset,
        diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            message.clone(),
            site.origin.user_span(),
        )),
        message,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
    }
}

fn ffi_coverage_result(
    program: &crate::core::CheckedProgram,
    site: &crate::core::ResolvedCallSite,
) -> VerificationResult {
    let caller_owner = crate::core::NodeId(site.owner.clone());
    let caller = ffi_checked::caller_name(program, &caller_owner);
    let declaration = program
        .ffi_declarations()
        .values()
        .find(|declaration| declaration.symbol == site.callee);
    let shape = declaration
        .map(|declaration| {
            let mut flags = Vec::new();
            if declaration.abi != "C" {
                flags.push(format!("abi={}", declaration.abi));
            }
            if declaration.invocation.variadic {
                flags.push("variadic".to_string());
            }
            if declaration.invocation.block_returns_errno
                || declaration.invocation.declaration_returns_errno
            {
                flags.push("errno".to_string());
            }
            if declaration
                .fixed_parameter_modes
                .iter()
                .any(|mode| !matches!(mode, crate::core::ir::ResolvedFfiParameterMode::Value))
            {
                flags.push("parameter-mode".to_string());
            }
            if declaration.ensures.is_declared() {
                flags.push("ensures".to_string());
            }
            let has_non_scalar = declaration
                .signature
                .as_ref()
                .ok()
                .is_some_and(|signature| {
                    signature.parameters.iter().any(|parameter| {
                        !matches!(
                            program.resolved_types().get(&parameter.ty),
                            Some(crate::core::ir::ResolvedType::Primitive(
                                crate::core::ir::PrimitiveType::I32
                                    | crate::core::ir::PrimitiveType::I64
                                    | crate::core::ir::PrimitiveType::Bool
                                    | crate::core::ir::PrimitiveType::F32
                                    | crate::core::ir::PrimitiveType::F64
                            ))
                        )
                    }) || !matches!(
                        program.resolved_types().get(&signature.result),
                        Some(crate::core::ir::ResolvedType::Primitive(
                            crate::core::ir::PrimitiveType::Unit
                                | crate::core::ir::PrimitiveType::I32
                                | crate::core::ir::PrimitiveType::I64
                                | crate::core::ir::PrimitiveType::Bool
                                | crate::core::ir::PrimitiveType::F32
                                | crate::core::ir::PrimitiveType::F64
                        ))
                    )
                });
            if has_non_scalar {
                flags.push("non-scalar-abi".to_string());
            }
            if flags.is_empty() {
                "typed-call-context".to_string()
            } else {
                flags.join(",")
            }
        })
        .unwrap_or_else(|| "missing-declaration-descriptor".to_string());
    let declaration_owner = declaration
        .map(|declaration| declaration.declaration.0.as_str())
        .unwrap_or("<missing>");
    let message = format!(
        "{}: checker-owned FFI declaration '{}' (owner '{}') at call site '{}' (owner '{}', span {}) ({shape}) is outside canonical proof coverage",
        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
        site.callee,
        declaration_owner,
        site.node_id.0,
        caller_owner.0,
        site.origin.user_span(),
    );
    VerificationResult {
        func_name: format!("{caller} calls {}", site.callee),
        status: VerifStatus::NotInTrustedSubset,
        diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            message.clone(),
            site.origin.user_span(),
        )),
        message,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
    }
}

fn ffi_mixed_map_requires_result(
    program: &crate::core::CheckedProgram,
    site: &crate::core::ResolvedCallSite,
) -> VerificationResult {
    let caller = ffi_checked::caller_name(program, &crate::core::NodeId(site.owner.clone()));
    let message = format!(
        "{}: called extern '{}' Requires at call site '{}' is Deferred because the Actor compatibility request also contains an independently-owned Map profile without a combined verifier receipt",
        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
        site.callee,
        site.node_id.0
    );
    VerificationResult {
        func_name: format!("{caller} calls {}", site.callee),
        status: VerifStatus::NotInTrustedSubset,
        diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            message.clone(),
            site.origin.user_span(),
        )),
        message,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
    }
}

fn ffi_infrastructure_error(func_name: String) -> VerificationResult {
    VerificationResult {
        func_name,
        status: VerifStatus::InfrastructureError,
        message: "Z3 solver not available".into(),
        diagnostic: None,
        duration_us: 0,
        constraint_count: 0,
        artifact: None,
        trusted_subset_domain: None,
    }
}

/// Check whether the Z3 solver is available at runtime.
pub fn is_z3_available() -> bool {
    // The integration CLI needs a deterministic way to exercise the same
    // no-Z3 branch on hosts where libz3 is installed. Keep this hook confined
    // to debug builds; release users always receive the real runtime check.
    #[cfg(debug_assertions)]
    if std::env::var_os("MIMI_TEST_FORCE_NO_Z3").is_some() {
        return false;
    }
    Verifier::new().is_ok()
}

/// 0.34.44 (ADR-008 §3): dual-engine verification with fail-closed divergence.
///
/// Runs BOTH engines on the same checked program and merges their verdicts:
/// - primary = the Resolved engine (ADR-008 §1 main judgment);
/// - secondary = the canonical MIR engine on materialized typed requests;
///   deferred requests are either admitted by a closed canonical profile or
///   returned as an explicit coverage result;
/// - per-function verdict classes that disagree produce a fail-closed result
///   carrying the new E0439 divergence diagnostic — neither engine is trusted
///   alone when they disagree.
///
/// Used by `mimi verify` (the CLI main path). Library callers that need a
/// single engine keep using `verify_checked` / `Verifier::verify_checked`.
pub fn verify_checked_dual(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    program
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    // Keep the dual entry point on the same admission boundary before its
    // typed request fast path.  Otherwise a complete request envelope could
    // bypass the ordinary Map result-settlement check used by the single
    // verifier and canonical CLI route.
    validate_public_verifier_mir_boundary(program)?;
    let z3_available = is_z3_available();
    let actor_compatibility = is_actor_proof_compatibility(program);
    if !z3_available {
        if actor_compatibility {
            return Ok(actor_compatibility_no_z3_results(program, source_hash));
        }
        // The dual wrapper preserves the same no-solver result algebra as the
        // single checked entry. Never enter a closed MIR profile that would
        // attempt to initialize SolverSession without Z3.
        return Ok(ctx::mock_verify_checked(program));
    }

    // Owner-v2 is a checker-owned canonical profile, but CompleteCoverage is
    // still diagnostic-only while the shared production gate is closed. Keep
    // it out of the generic typed-request merge so a raw MIR proof cannot be
    // counted as production readiness; the internal profile verifier adds
    // the owner-scoped NITS result after collecting diagnostic observations.
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    match admission.ordinary_map_owned_any_v2 {
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::StagedGetValues => {
            return verify_staged_ordinary_map_owned_any_v2(program, source_hash);
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
            if !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY =>
        {
            return verify_closed_mir_program(program, source_hash)?.ok_or_else(|| {
                format!(
                    "{}: complete ordinary Map/Any owner-v2 diagnostic verifier did not materialize a route",
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )
            });
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage => {
            return Err(format!(
                "{}: Checker recognized an ordinary Map/Any owner-v2 candidate but withheld complete operation-attempt and result-receipt coverage",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::OutsideProfile
        | crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage => {}
    }
    let map_profile_complete = admission.map_root_complete()
        || admission.map_any_owner_complete()
        || admission.ordinary_map_owned_any_v2_complete()
        || admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        || admission.ordinary_map_owned_get_i32_string_v1_complete();
    if map_profile_complete {
        // Prefer the typed request envelope when it is available so the
        // public dual API preserves the same canonical-contract receipt as
        // `verify_checked`.  Some ownership-only profiles are deferred from
        // that envelope; those still use their exact profile receipt below.
        match mir_request::prepare(program) {
            mir_request::Preparation::Complete(request) => {
                return mir_request::verify(&request, program, source_hash.clone());
            }
            mir_request::Preparation::Deferred(_) => {
                if let Some(results) = verify_closed_mir_program(program, source_hash.clone())? {
                    return Ok(results);
                }
            }
        }
    }
    // A contract-bearing Actor without a Map/Any owner still has an explicit
    // compatibility verifier.  Keep that named boundary for proof-only
    // requests, while mixed Actor+Map candidates remain fail-closed below and
    // can never hide the ownership route behind this adapter.
    let map_owner_candidate = admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal();
    if actor_compatibility && !map_owner_candidate {
        let mut verifier = Verifier::new()?;
        verifier.set_source_hash(source_hash);
        return Ok(verifier.verify_checked(program));
    }
    let mut deferred_boundaries = Vec::new();
    if z3_available {
        match mir_request::prepare(program) {
            mir_request::Preparation::Complete(request) => {
                // Independent primary is still the Resolved expression/body
                // encoder. The secondary symbolically executes canonical MIR.
                // They share checker identities, never a solver traversal.
                let mut primary = Verifier::new()?;
                primary.set_source_hash(source_hash.clone());
                let mut resolved_results = primary.verify_checked(program);
                mir_request::normalize_primary_names(&mut resolved_results, &request);
                let mir_results = mir_request::verify(&request, program, source_hash)?;
                return Ok(merge_engine_verdicts_with_secondary_label(
                    resolved_results,
                    mir_results,
                    "canonical_mir",
                ));
            }
            mir_request::Preparation::Deferred(boundaries) => {
                deferred_boundaries = boundaries;
                // A literal FFI declaration root has a complete checker-owned
                // result without a callable body.  It cannot contribute a
                // second body-engine verdict, so the dual entry point treats
                // this exact declaration-only profile as an atomic canonical
                // obligation.  Any callable proof root, actor/flow/session,
                // impl, or non-literal/mixed FFI contract misses the profile
                // and is handled by the explicit canonical coverage boundary.
                if let Some(results) =
                    verify_dual_literal_ffi_declaration_root(program, &source_hash)
                {
                    return results;
                }
            }
        }
    }
    validate_public_verifier_mir_boundary(program)?;
    match verify_closed_mir_program(program, source_hash.clone()) {
        Ok(Some(results))
            if !deferred_boundaries.is_empty()
                && results.iter().any(|result| {
                    result
                        .message
                        .contains("proof worklist coverage gap: DeferredBoundary")
                }) =>
        {
            let mut primary = Verifier::new()?;
            primary.set_source_hash(source_hash.clone());
            let resolved_results = primary.verify_checked_without_actor_ffi(program);
            return Ok(dual_verifier_coverage_results(
                program,
                resolved_results,
                &deferred_boundaries,
                &source_hash,
            ));
        }
        Ok(Some(results)) => return Ok(results),
        Ok(None) => {}
        Err(error)
            if !deferred_boundaries.is_empty()
                && error.contains("proof worklist coverage gap: DeferredBoundary") =>
        {
            let mut primary = Verifier::new()?;
            primary.set_source_hash(source_hash.clone());
            let resolved_results = primary.verify_checked_without_actor_ffi(program);
            return Ok(dual_verifier_coverage_results(
                program,
                resolved_results,
                &deferred_boundaries,
                &source_hash,
            ));
        }
        Err(error) => return Err(error),
    }
    // Primary engine: resolved (verifies from Resolved IR).
    let mut primary = Verifier::new()?;
    primary.set_source_hash(source_hash.clone());
    let resolved_results = primary.verify_checked_without_actor_ffi(program);
    // Secondary engine: canonical MIR only. If the checker could not
    // materialize an admitted profile, retain the primary observations but
    // downgrade every proof-bearing result to an explicit coverage boundary;
    // reopening the retained AST here would make the dual API silently depend
    // on the retired Flow/VIR owner.
    if let Some(results) =
        verify_deferred_dual_mir(program, source_hash.clone(), resolved_results.clone())?
    {
        return Ok(results);
    }
    Ok(dual_verifier_coverage_results(
        program,
        resolved_results,
        &deferred_boundaries,
        &source_hash,
    ))
}

/// Consume a deferred dual request with the same checker-owned canonical MIR
/// route used by direct consumers. Closed profile adapters run before this
/// helper; this extra attempt covers a typed graph whose proof request is
/// deferred for a consumer-specific reason but whose complete MIR graph and
/// verifier capability gate are still available. Compatibility materializing
/// errors are an admission miss and are handled by
/// [`dual_verifier_coverage_results`]; complete-route errors are hard errors.
fn verify_deferred_dual_mir(
    program: &crate::core::CheckedProgram,
    source_hash: String,
    mut resolved_results: Vec<VerificationResult>,
) -> Result<Option<Vec<VerificationResult>>, String> {
    let route = match crate::core::mir::materialize_canonical_mir_route(program, None) {
        Ok(route) => route,
        Err(crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility { .. }) => {
            return Ok(None)
        }
        Err(error) => {
            let code = error
                .diagnostic_code()
                .unwrap_or(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE);
            return Err(format!("{code}: {error}"));
        }
    };
    let canonical = route.program;
    crate::verifier::validate_mir_capabilities(&canonical).map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: deferred dual canonical verifier consumer rejected the graph: {errors:?}"
        )
    })?;
    let receipt = canonical.route_receipt("dual-canonical-mir-v1");
    receipt.validate_against_program(&canonical)?;
    let mut mir_results =
        crate::verifier::verify_mir_with_route_receipt(&canonical, &receipt, source_hash)?;
    // The typed MIR request path normalizes checker owners before merge. The
    // route materializer does not carry that table, so use the same source
    // callable labels before comparing the independent engine observations.
    for result in &mut resolved_results {
        if let Some(name) = result.func_name.strip_prefix("function:") {
            result.func_name = name.to_string();
        }
    }
    for result in &mut mir_results {
        if let Some(name) = result.func_name.strip_prefix("function:") {
            result.func_name = name.to_string();
        }
    }
    let secondary_names = mir_results
        .iter()
        .map(|result| result.func_name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let missing = resolved_results
        .iter()
        .filter(|result| !matches!(result.status, VerifStatus::NoObligations))
        .filter(|result| !secondary_names.contains(result.func_name.as_str()))
        .map(|result| result.func_name.as_str())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        // A route that materializes only a subgraph cannot claim a dual
        // proof for the whole checked program. Return to the outer coverage
        // adapter so every primary owner is downgraded to one structured
        // NotInTrustedSubset result carrying the deferred boundary and
        // source/Resolved provenance; never merge a partial secondary batch.
        return Ok(None);
    }
    Ok(Some(merge_engine_verdicts_with_secondary_label(
        resolved_results,
        mir_results,
        "dual-canonical-mir",
    )))
}

/// Make a deferred dual request visibly fail closed without consuming the
/// retained source body. The primary engine may have produced a useful
/// observation, but the public dual contract cannot claim it without an
/// independent canonical secondary proof.
fn dual_verifier_coverage_results(
    program: &crate::core::CheckedProgram,
    primary: Vec<VerificationResult>,
    boundaries: &[crate::core::mir::verification::MirVerificationBoundary],
    source_hash: &str,
) -> Vec<VerificationResult> {
    let resolved_ir_hash = ctx::compute_resolved_ir_hash(program);
    let boundary_text = if boundaries.is_empty() {
        "canonical MIR secondary was not admitted".to_string()
    } else {
        boundaries
            .iter()
            .map(|boundary| {
                let point = boundary
                    .point
                    .as_ref()
                    .map(|point| format!("@{}", point.0))
                    .unwrap_or_default();
                format!("{}{}: {}", boundary.owner.0, point, boundary.reason)
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let provenance = if source_hash.is_empty() {
        format!("resolved_ir_hash={resolved_ir_hash}")
    } else {
        format!("source_hash={source_hash}, resolved_ir_hash={resolved_ir_hash}")
    };
    let message = format!(
        "{}: dual verifier has no checker-owned canonical MIR secondary ({boundary_text}; {provenance})",
        crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
    );
    if primary.is_empty() {
        return if boundaries.is_empty() {
            Vec::new()
        } else {
            vec![VerificationResult {
                func_name: "dual-canonical-mir".into(),
                status: VerifStatus::NotInTrustedSubset,
                message: message.clone(),
                diagnostic: Some(crate::diagnostic::Diagnostic::error_code(
                    crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                    message,
                    program.entry_span().unwrap_or(crate::span::Span::UNKNOWN),
                )),
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
            }]
        };
    }
    primary
        .into_iter()
        .map(|mut result| {
            if matches!(result.status, VerifStatus::NoObligations) {
                return result;
            }
            let span = result
                .diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.span)
                .unwrap_or_else(|| program.entry_span().unwrap_or(crate::span::Span::UNKNOWN));
            result.status = VerifStatus::NotInTrustedSubset;
            result.message = message.clone();
            result.diagnostic = Some(crate::diagnostic::Diagnostic::error_code(
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                message.clone(),
                span,
            ));
            result.constraint_count = 0;
            result.artifact = None;
            result.trusted_subset_domain = Some(TrustedSubsetDomain::Contract);
            result
        })
        .collect()
}

/// Try the already-closed verifier profile matrix from canonical MIR.
///
/// The public checked verifier is a program-level API. Once checker-owned
/// eligibility has established one of the exact closed profiles, its old
/// AST/Flow engine is no longer a valid consumer for this request. The profile
/// matrix below is owned by the shared MIR route module; construction,
/// materialized operation coverage, MIR capability, and the MIR verifier are
/// one hard-fail-closed chain. Programs outside every exact profile return
/// `None` so the caller can issue a checker-owned coverage result for the
/// unmigrated feature; they never re-enter a retained surface-body verifier.
fn verify_closed_mir_program(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    // Match default route precedence: a defined exported C body has no
    // canonical export-wrapper ABI receipt, so the entire request belongs to
    // the compatibility verifier even if another subgraph resembles a closed
    // MIR profile. Do not let a later verifier-side island probe turn that
    // explicit compatibility route into a construction failure.
    if crate::core::mir::exported_abi_body_boundary_reason(program).is_some()
        || crate::core::mir::scalar_actor_contract_boundary_reason(program).is_some()
    {
        return Ok(None);
    }
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if admission.ordinary_map_owned_any_v2
        == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map/Any owner-v2 candidate but withheld complete operation-attempt and result-receipt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission
        .ordinary_map_owned_get_i32_string_v1
        .has_profile_signal()
        && !admission.ordinary_map_owned_get_i32_string_v1_complete()
    {
        // This is a new typed API with no AST compatibility verifier. Keep
        // pre-existing untyped Map compatibility separate and fail closed for
        // a typed-get candidate whose independent receipt is incomplete.
        return Err(format!(
            "{}: Checker recognized an ordinary Map owned-get attempt without complete consumer coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    let ordinary_map_v1_exception = admission.map_any_owner_complete()
        && program
            .resource_analyses()
            .values()
            .filter(|analysis| analysis.has_ordinary_map_owner_lifecycle_v1_attempts())
            .all(|analysis| analysis.ordinary_map_attempts_fully_covered_by_map_any_owner_v1());
    if admission
        .ordinary_map_owner_lifecycle_i32_string_v1
        .has_profile_signal()
        && !admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        && !ordinary_map_v1_exception
    {
        // Default verify follows the same explicit compatibility disposition
        // as run/build. Partial ordinary Map attempts do not become a
        // canonical profile until Get/Values/escape shapes have receipts.
        return Ok(None);
    }

    const PROFILES: [crate::core::mir::CanonicalMirRouteProfile; 28] = [
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1,
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2,
        crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1,
        crate::core::mir::CanonicalMirRouteProfile::MapRoot,
        crate::core::mir::CanonicalMirRouteProfile::ScalarActor,
        crate::core::mir::CanonicalMirRouteProfile::ScalarFfi,
        crate::core::mir::CanonicalMirRouteProfile::ScalarNested,
        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI32,
        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI64,
        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection,
        crate::core::mir::CanonicalMirRouteProfile::FlatCopyRecord,
        crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition,
        crate::core::mir::CanonicalMirRouteProfile::FlowFailureRetry,
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant,
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionNestedTupleVariant,
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate,
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjection,
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback,
        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjection,
        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback,
        crate::core::mir::CanonicalMirRouteProfile::ManagedResultCall,
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI32Variant,
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionBoolVariant,
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI64Variant,
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionF64Variant,
        crate::core::mir::CanonicalMirRouteProfile::CopyResultI32Variant,
        crate::core::mir::CanonicalMirRouteProfile::SessionChannel,
    ];
    for profile in PROFILES {
        if let Some(results) = verify_closed_mir_profile(program, profile, source_hash.clone())? {
            return Ok(Some(results));
        }
    }
    Ok(None)
}

/// Run the staged V2 consumer through canonical MIR for diagnostic evidence.
/// Its owner-scoped coverage NITS are appended by `verify_closed_mir_profile`;
/// callers must not fall through to a compatibility proof engine.
fn verify_staged_ordinary_map_owned_any_v2(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    verify_closed_mir_program(program, source_hash)?.ok_or_else(|| {
        format!(
            "{}: staged ordinary Map/Any owner-v2 consumer did not materialize a verifier profile",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )
    })
}

pub(crate) fn verify_scalar_actor_checked(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if !admission.scalar_actor {
        return Ok(None);
    }
    validate_public_verifier_mir_boundary(program)?;
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::ScalarActor,
        source_hash,
    )
}

/// Verify one already-closed profile from the shared canonical route.
///
/// This function is the verifier-side consumer adapter only. Admission and
/// materialization are checker-owned by `core::mir::route`; profile-specific
/// TypeDesc/island validators remain explicit here because they are the
/// verifier's final consumer gate.
fn verify_closed_mir_profile(
    program: &crate::core::CheckedProgram,
    profile: crate::core::mir::CanonicalMirRouteProfile,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    let Some(canonical) = materialize_closed_mir_island(program, profile)? else {
        return Ok(None);
    };
    match profile {
        crate::core::mir::CanonicalMirRouteProfile::ScalarActor => {
            crate::core::mir::validate_scalar_actor_island(&canonical).map_err(|errors| {
                format!("MIR-CAPABILITY-001: Actor receipt rejected: {errors:?}")
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(&canonical, &receipt)
                .map_err(|errors| {
                format!("MIR-CAPABILITY-001: Actor bytecode consumer rejected: {errors:?}")
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!("MIR-CAPABILITY-001: Actor native consumer rejected: {errors:?}")
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1 => {
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map owned-get bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map owned-get native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1 => {
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map owner lifecycle bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map owner lifecycle native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1 => {
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapAnyOwner v1 bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapAnyOwner v1 native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::MapRoot => {
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapRoot bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the MapRoot native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2 => {
            crate::verifier::validate_mir_capabilities(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map/Any owner-v2 capability: {errors:?}"
                )
            })?;
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map/Any owner-v2 bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the ordinary Map/Any owner-v2 native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::ScalarFfi => {}
        crate::core::mir::CanonicalMirRouteProfile::ScalarExport => {
            if canonical.export_declarations().is_empty() {
                return Err(
                    "MIR-CAPABILITY-001: scalar export route has no export declaration receipt"
                        .into(),
                );
            }
            let receipt = canonical.route_receipt(profile.as_str());
            crate::interp::bytecode::compile_mir_program_with_route_receipt(
                &canonical,
                &receipt,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the scalar export bytecode consumer: {errors:?}"
                )
            })?;
            crate::codegen::mir::validate_mir_native(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the scalar export native consumer: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::ScalarNested => {}
        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI32 => {
            crate::core::mir::validate_scalar_generic_identity_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the generic scalar identity island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::ScalarGenericIdentityI64 => {
            crate::core::mir::validate_scalar_generic_identity_i64_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the generic scalar identity i64 island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection => {
            crate::core::mir::validate_scalar_collection_graph(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the scalar collection island: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::FlatCopyRecord
        | crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition
        | crate::core::mir::CanonicalMirRouteProfile::FlowFailureRetry => {}
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant => {
            crate::core::mir::validate_option_string_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Option<string> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionNestedTupleVariant => {
            crate::core::mir::validate_option_nested_tuple_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the nested Option tuple variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI32Variant => {
            crate::core::mir::validate_copy_option_i32_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Copy Option<i32> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionBoolVariant => {
            crate::core::mir::validate_copy_option_variant_island(
                &canonical,
                crate::core::PrimitiveType::Bool,
                crate::core::mir::COPY_OPTION_BOOL_VARIANT_ISLAND,
            )
            .map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the Copy Option<bool> variant island: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI64Variant => {
            crate::core::mir::validate_copy_option_i64_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Copy Option<i64> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionF64Variant => {
            crate::core::mir::validate_copy_option_f64_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Copy Option<f64> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::CopyResultI32Variant => {
            crate::core::mir::validate_copy_result_i32_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Copy Result<i32, i32> variant island: {errors:?}"
                    )
                },
            )?;
        }
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate => {}
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjection => {}
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback => {}
        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjection => {}
        crate::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback => {}
        crate::core::mir::CanonicalMirRouteProfile::ManagedResultCall => {
            crate::core::mir::validate_managed_result_call_island(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the managed Result direct-call island: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::SessionChannel => {
            crate::core::mir::validate_session_channel_island(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the session-channel island: {errors:?}"
                )
            })?;
        }
    }
    crate::verifier::validate_mir_capabilities(&canonical).map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: canonical verifier TypeDesc/capability gate rejected the {}: {errors:?}",
            profile.as_str()
        )
    })?;
    let mut results = if matches!(
        profile,
        crate::core::mir::CanonicalMirRouteProfile::MapRoot
            | crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1
            | crate::core::mir::CanonicalMirRouteProfile::ScalarActor
            | crate::core::mir::CanonicalMirRouteProfile::ScalarExport
            | crate::core::mir::CanonicalMirRouteProfile::ScalarNested
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1
            | crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
    ) {
        let receipt = canonical.route_receipt(profile.as_str());
        verify_mir_diagnostic_with_route_receipt(&canonical, &receipt, source_hash)?
    } else {
        crate::verifier::verify_mir(&canonical, source_hash)?
    };
    // Public checked-verifier APIs historically expose source-level callable
    // names. Keep that display contract at the adapter boundary while proof
    // artifacts retain the canonical `function:` owner and MIR hash.
    for result in &mut results {
        if let Some(diagnostic) = result
            .diagnostic
            .as_mut()
            .filter(|diagnostic| diagnostic.span == crate::span::Span::UNKNOWN)
        {
            let subject = diagnostic
                .origin
                .as_ref()
                .and_then(|origin| origin.parent_node_id.as_ref());
            if let Some(meta) = subject
                .and_then(|subject| {
                    program
                        .node_meta()
                        .get(&crate::core::NodeId(subject.clone()))
                })
                .or_else(|| {
                    program
                        .node_meta()
                        .get(&crate::core::NodeId(result.func_name.clone()))
                })
            {
                diagnostic.span = meta.origin.user_span();
            }
        }
        if let Some(name) = result.func_name.strip_prefix("function:") {
            result.func_name = name.to_string();
        }
    }
    if profile == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
        && !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
    {
        results = enforce_ordinary_map_owned_any_v2_coverage(program, results);
    }
    Ok(Some(results))
}

/// Materialize one of the already-closed verifier islands through the shared
/// route envelope.  This is deliberately profile-driven: a verifier helper
/// must not grow a private `is_exact -> construct -> contains_*` policy when a
/// new consumer route is added.  Checker admission is read before construction
/// so non-target profiles do not pay for or accidentally trigger materialization.
fn materialize_closed_mir_island(
    program: &crate::core::CheckedProgram,
    island: crate::core::mir::CanonicalMirRouteProfile,
) -> Result<Option<crate::core::mir::reference::MirProgram>, String> {
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
        && admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map/Any owner-v2 candidate but withheld complete operation-attempt and result-receipt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    let v2_verifier_diagnostic = island
        == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
        && (admission.ordinary_map_owned_any_v2
            == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::StagedGetValues
            || (admission.ordinary_map_owned_any_v2
                == crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
                && !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY));
    let ordinary_map_v1_exception = admission.map_any_owner_complete()
        && program
            .resource_analyses()
            .values()
            .filter(|analysis| analysis.has_ordinary_map_owner_lifecycle_v1_attempts())
            .all(|analysis| analysis.ordinary_map_attempts_fully_covered_by_map_any_owner_v1());
    // A complete map-owner profile cannot coexist with a scalar FFI island.
    // Resolve that shared-route conflict before the no-Z3 mock boundary too:
    // otherwise `verify_ffi_checked` could skip MIR materialization and report
    // a compatibility/mock result for a program the run/build route rejects.
    let conflicting_map_owner_ffi_profiles = (admission.map_root_complete()
        || admission.map_any_owner_complete()
        || admission.ordinary_map_owned_any_v2_complete()
        || admission.ordinary_map_owned_get_i32_string_v1_complete()
        || admission.ordinary_map_owner_lifecycle_i32_string_v1_complete())
        && admission.scalar_ffi;
    if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1
        && admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        && !admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        && !ordinary_map_v1_exception
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map owner lifecycle attempt without complete operation-attempt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1
        && admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal()
        && !admission.ordinary_map_owned_get_i32_string_v1_complete()
    {
        return Err(format!(
            "{}: Checker recognized an ordinary Map owned-get attempt without complete independent operation-attempt coverage",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if island == crate::core::mir::CanonicalMirRouteProfile::MapRoot
        && admission.map_root.is_candidate()
        && !admission.map_root_complete()
    {
        return Err(format!(
            "{}: Checker recognized a MapRoot candidate but withheld its complete local lifecycle receipt",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if island == crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1
        && admission.map_any_owner.is_candidate()
        && !admission.map_any_owner_complete()
    {
        return Err(format!(
            "{}: Checker recognized a MapAnyOwner v1 candidate but withheld its complete independent lifecycle receipt",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if !is_z3_available()
        && !conflicting_map_owner_ffi_profiles
        && island != crate::core::mir::CanonicalMirRouteProfile::ScalarActor
    {
        if island
            == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1
            && admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        {
            return Err(
                "MIR-VERIFY-001: ordinary Map owner lifecycle Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1
            && admission.ordinary_map_owned_get_i32_string_v1_complete()
        {
            return Err(
                "MIR-VERIFY-001: ordinary Map owned-get Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if island == crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2
            && admission.ordinary_map_owned_any_v2_complete()
        {
            return Err(
                "MIR-VERIFY-001: ordinary Map/Any owner-v2 Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if island == crate::core::mir::CanonicalMirRouteProfile::MapRoot
            && admission.map_root_complete()
        {
            return Err(
                "MIR-VERIFY-001: MapRoot Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        if island == crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1
            && admission.map_any_owner_complete()
        {
            return Err(
                "MIR-VERIFY-001: MapAnyOwner v1 Checker receipt profile requires the MIR contract verifier; compatibility fallback is forbidden".into(),
            );
        }
        // Preserve the existing CheckedProgram mock infrastructure boundary;
        // without Z3 the public API does not claim a MIR proof.
        return Ok(None);
    }
    if !island.is_admitted(admission) && !v2_verifier_diagnostic {
        return Ok(None);
    }
    let materialized_route = if v2_verifier_diagnostic {
        crate::core::mir::materialize_ordinary_map_owned_any_v2_for_verifier_diagnostics(
            program, None,
        )
    } else {
        crate::core::mir::materialize_canonical_mir_route(program, None)
    };
    let route = match materialized_route {
        Ok(route) => route,
        Err(crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility { .. }) => {
            return Ok(None)
        }
        Err(error) => {
            let code = error
                .diagnostic_code()
                .expect("complete route materialization errors carry a diagnostic code");
            return Err(format!("{code}: {error}"));
        }
    };
    let nonproduction_v2_has_checker_actions = v2_verifier_diagnostic
        && route.program.functions().values().any(|function| {
            function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_actions()
        });
    if !island.is_materialized(&route) && !nonproduction_v2_has_checker_actions {
        return Err(format!(
            "{}: {} admission did not materialize its canonical operation",
            crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
            island.as_str()
        ));
    }
    Ok(Some(route.program))
}

/// Verify the already-closed scalar collection island from canonical MIR.
#[cfg(test)]
fn verify_closed_scalar_collection_mir(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection,
        source_hash,
    )
}

/// Verify the already-closed S8 silent-local Flow island from canonical MIR.
#[cfg(test)]
fn verify_closed_s8_flow_mir(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    verify_closed_mir_profile(
        program,
        crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition,
        source_hash,
    )
}

/// Coarse verdict classes for divergence detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerdictClass {
    Proven,
    Disproven,
    Inconclusive,
    /// NoObligations / InfrastructureError — the engine attempted no proof;
    /// never counts as a divergence counterpart.
    NoOpinion,
}

fn verdict_class(status: &VerifStatus) -> VerdictClass {
    match status {
        VerifStatus::Proven => VerdictClass::Proven,
        VerifStatus::Disproven => VerdictClass::Disproven,
        VerifStatus::NoObligations | VerifStatus::InfrastructureError => VerdictClass::NoOpinion,
        _ => VerdictClass::Inconclusive,
    }
}

fn source_provenance_conflict(
    primary: &VerificationResult,
    secondary: &VerificationResult,
) -> bool {
    match (&primary.artifact, &secondary.artifact) {
        (Some(primary), Some(secondary)) => {
            !primary.source_hash.is_empty()
                && !secondary.source_hash.is_empty()
                && primary.source_hash != secondary.source_hash
        }
        _ => false,
    }
}

/// Return true when a merged batch carries proofs from more than one bound
/// source snapshot.  A per-function comparison is insufficient here because
/// the Flow engine can emit obligations (for example call-site obligations)
/// that have no same-named result in the resolved batch.  Those unmatched
/// results still become part of the merged proof observation and therefore
/// must obey the same single-snapshot provenance invariant.
fn batch_source_provenance_conflict(
    primary: &[VerificationResult],
    secondary: &[VerificationResult],
) -> bool {
    let mut source_hash: Option<&str> = None;
    for result in primary.iter().chain(secondary.iter()) {
        let Some(artifact) = result.artifact.as_ref() else {
            continue;
        };
        if artifact.source_hash.is_empty() {
            continue;
        }
        match source_hash {
            None => source_hash = Some(artifact.source_hash.as_str()),
            Some(existing) if existing != artifact.source_hash => return true,
            Some(_) => {}
        }
    }
    false
}

/// Convert a proof-bearing result into an explicit no-proof observation after
/// a batch-level provenance conflict.  Results without an artifact are left
/// untouched: they already carry no proof that could cross a source boundary.
fn discard_batch_provenance_artifact(mut result: VerificationResult) -> VerificationResult {
    if result.artifact.is_none() {
        return result;
    }
    let span = result
        .diagnostic
        .as_ref()
        .map(|diagnostic| diagnostic.span)
        .unwrap_or_else(|| crate::span::Span::new(1, 1, 1, 1));
    result.status = VerifStatus::InfrastructureError;
    result.trusted_subset_domain = None;
    result.constraint_count = 0;
    result.message = format!(
        "[{}] mixed source provenance mismatch in merged batch for '{}'; merged proof discarded",
        crate::diagnostic::codes::E0439,
        result.func_name
    );
    result.diagnostic = Some(crate::diagnostic::Diagnostic::error(
        format!(
            "{}: merged verification results for '{}' came from different source snapshots",
            crate::diagnostic::codes::E0439,
            result.func_name
        ),
        span,
    ));
    result.artifact = None;
    result
}

/// Merge per-function verdicts from the two engines (ADR-008 §3).
///
/// Rules:
/// - both agree (same class) → primary (resolved) result wins;
/// - one side is NoOpinion → the other side's result wins silently (no proof
///   was attempted on the silent side, so there is nothing to disagree with);
/// - both sides carry different non-empty source hashes → the merged result
///   is InfrastructureError with no artifact, even when verdict classes agree;
/// - classes disagree → fail-closed: Disproven beats Inconclusive beats
///   Proven, and the merged result carries the E0439 divergence diagnostic.
fn merge_engine_verdicts(
    primary: Vec<VerificationResult>,
    secondary: Vec<VerificationResult>,
) -> Vec<VerificationResult> {
    merge_engine_verdicts_with_secondary_label(primary, secondary, "flow_ast")
}

fn merge_engine_verdicts_with_secondary_label(
    primary: Vec<VerificationResult>,
    secondary: Vec<VerificationResult>,
    secondary_label: &str,
) -> Vec<VerificationResult> {
    // Normalize the inputs before matching names.  This closes the gap where
    // an unmatched Flow-only result could otherwise smuggle a proof from a
    // different source snapshot into the merged batch.
    let batch_source_conflict = batch_source_provenance_conflict(&primary, &secondary);
    let primary = if batch_source_conflict {
        primary
            .into_iter()
            .map(discard_batch_provenance_artifact)
            .collect()
    } else {
        primary
    };
    let secondary = if batch_source_conflict {
        secondary
            .into_iter()
            .map(discard_batch_provenance_artifact)
            .collect()
    } else {
        secondary
    };
    let mut merged: Vec<VerificationResult> = Vec::with_capacity(primary.len());
    let mut secondary_by_name: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (index, result) in secondary.iter().enumerate() {
        secondary_by_name
            .entry(result.func_name.clone())
            .or_insert(index);
    }
    let mut consumed: Vec<bool> = vec![false; secondary.len()];
    for mut result in primary {
        let Some(&sec_index) = secondary_by_name.get(&result.func_name) else {
            merged.push(result);
            continue;
        };
        consumed[sec_index] = true;
        let flow_result = &secondary[sec_index];
        let primary_class = verdict_class(&result.status);
        let flow_class = verdict_class(&flow_result.status);
        if source_provenance_conflict(&result, flow_result) {
            // A matching verdict is still unusable when the engines proved
            // different source snapshots. Drop the artifact and expose a
            // no-proof status instead of allowing agreement to hide the
            // provenance conflict.
            result.status = VerifStatus::InfrastructureError;
            result.trusted_subset_domain = None;
            result.constraint_count = 0;
            result.message = format!(
                "[{}] source provenance mismatch for '{}'; merged proof discarded",
                crate::diagnostic::codes::E0439,
                result.func_name
            );
            result.diagnostic = Some(crate::diagnostic::Diagnostic::error(
                format!(
                    "{}: verification results for '{}' came from different source snapshots",
                    crate::diagnostic::codes::E0439,
                    result.func_name
                ),
                result
                    .diagnostic
                    .as_ref()
                    .or(flow_result.diagnostic.as_ref())
                    .map(|diagnostic| diagnostic.span)
                    .unwrap_or_else(|| crate::span::Span::new(1, 1, 1, 1)),
            ));
            result.artifact = None;
            merged.push(result);
            continue;
        }
        if primary_class == VerdictClass::NoOpinion {
            // Resolved engine attempted no proof — take the flow verdict.
            merged.push(flow_result.clone());
            continue;
        }
        if flow_class == VerdictClass::NoOpinion || flow_class == primary_class {
            merged.push(result);
            continue;
        }
        // Divergence: fail-closed to the weaker conclusion.
        let weaker_is_flow = matches!(
            (primary_class, flow_class),
            (VerdictClass::Proven, VerdictClass::Disproven)
                | (VerdictClass::Proven, VerdictClass::Inconclusive)
                | (VerdictClass::Inconclusive, VerdictClass::Disproven)
        );
        let weaker = if weaker_is_flow {
            flow_result.clone()
        } else {
            result.clone()
        };
        result.status = weaker.status.clone();
        result.trusted_subset_domain = weaker.trusted_subset_domain;
        result.constraint_count = weaker.constraint_count;
        result.message = format!(
            "[{}] engine divergence for '{}': resolved={:?} vs {secondary_label}={:?}; \
             fail-closed to {:?} ({})",
            crate::diagnostic::codes::E0439,
            result.func_name,
            primary_class,
            flow_class,
            result.status,
            weaker.message
        );
        let divergence = crate::diagnostic::Diagnostic::error(
            format!(
                "{}: verification engines disagree on '{}' (resolved={:?}, \
                 {secondary_label}={:?}); the weaker conclusion wins",
                crate::diagnostic::codes::E0439,
                result.func_name,
                primary_class,
                flow_class
            ),
            weaker
                .diagnostic
                .as_ref()
                .or(result.diagnostic.as_ref())
                .map(|d| d.span)
                .unwrap_or_else(|| crate::span::Span::new(1, 1, 1, 1)),
        );
        result.diagnostic = Some(divergence);
        result.artifact = None; // a divergent verdict is no proof
        merged.push(result);
    }
    // Functions verified ONLY by the flow engine (e.g. call-site obligations
    // the resolved engine does not model yet) pass through untouched.
    for (index, result) in secondary.iter().enumerate() {
        if !consumed[index] {
            merged.push(result.clone());
        }
    }
    merged
}

fn format_check_errors(diagnostics: Vec<crate::diagnostic::Diagnostic>) -> String {
    diagnostics
        .into_iter()
        .map(|diagnostic| {
            format!(
                "{}:{}: {}",
                diagnostic.span.start_line, diagnostic.span.start_col, diagnostic.message
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests;
