mod ctx;
mod expr;
mod flow;
mod func;
mod helpers;
mod mir;
mod mir_capability;
pub(crate) mod resolved_expr;
pub mod vir;

pub mod ffi;

pub(crate) use ctx::Z3VarMap;
pub use ctx::{
    Counterexample, ProofArtifact, TrustedSubsetDomain, VerifStatus, VerificationResult, Verifier,
};
pub(crate) use ctx::{SolverSession, VerifierCtx};
#[cfg(test)] // §11-#48 regression tests (audit_fix_verifier.rs)
pub(crate) use expr::encode_match_bool;
pub use flow::{
    flow_verify_ffi_call_sites, flow_verify_ffi_call_sites_or_mock, FlowAcc, FlowEvent,
    VerifierState,
};

/// Verify a previously validated canonical MIR program with the experimental
/// MIR-only scalar contract engine.  The input boundary intentionally has no
/// AST/ResolvedBody parameter and never falls back to another verifier.
pub fn verify_mir(
    program: &crate::core::mir::reference::MirProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    mir::verify_program(program, source_hash)
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
    verifier.set_source_hash(source_hash);
    Ok(verifier.verify_checked(&program))
}

/// Verify contracts in a type-checked program (supports pre-merged imports).
///
/// `source_hash` is the BLAKE3 hash of the source text (for ProofArtifact
/// tamper detection). Pass an empty string if source text is unavailable.
///
/// Closed scalar collection, flat Copy-record, S8 Flow, and exact non-Copy
/// `Option<string>` programs are verified from one canonical MIR graph. Other
/// programs remain on the explicit compatibility
/// boundary: when Z3 is available, they delegate to the Flow verifier state
/// machine (which still uses `legacy_body_file()` for AST-based function body
/// encoding); when Z3 is unavailable, they use CheckedProgram-based mock
/// verification, bypassing `legacy_body_file()` entirely.
///
/// Note: The Resolved IR contract path (verify_checked_contracts) is used
/// by verify_source_with / VerifierCtx::verify_checked for direct contract
/// verification without the state machine wrapper. Full migration of the
/// Flow verifier to CheckedProgram-based initialization (C4, including
/// the Z3 path) is planned for 0.32.28+.
pub fn verify_checked(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    program
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    if let Some(results) = verify_closed_mir_program(program, source_hash.clone())? {
        return Ok(results);
    }
    // P1-24: compute Resolved IR hash from CheckedProgram signatures.
    let resolved_ir_hash = ctx::compute_resolved_ir_hash(program);
    if is_z3_available() {
        // C4 Z3 path (permanent): the Flow verifier encodes transition invariants
        // from surface AST body expressions. The explicitly tagged legacy body
        // boundary is required here because
        // the Z3 encoding is defined over AST Expr nodes, not ResolvedExpr.
        // The resolved_ir_hash is embedded in ProofArtifact by the flow verifier.
        flow::flow_verify_file_with_hashes(
            program.legacy_body_file(crate::core::LegacyBodyConsumer::FlowVerifierCompatibility),
            source_hash,
            resolved_ir_hash,
        )
    } else {
        // C4 mock path: from CheckedProgram, no retained surface body needed.
        Ok(ctx::mock_verify_checked(program))
    }
}

/// Parse source and verify extern call sites using Z3.
pub fn verify_ffi_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let file = parse_memory_source(source, "ffi-call-sites")?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    verify_ffi_checked(&program)
}

/// Verify extern call sites from a checked program.
///
/// Contract expressions still use the explicit legacy body adapter until
/// typed Verification IR lands, but declaration identity and arity are
/// authoritative from CheckedProgram and fail closed before that adapter.
pub fn verify_ffi_checked(
    program: &crate::core::CheckedProgram,
) -> Result<Vec<VerificationResult>, String> {
    let mut externs = std::collections::HashMap::new();
    for block in program.extern_blocks().values() {
        for signature in &block.signatures {
            let func_span = signature.span;
            let adapter_origin = crate::ast::AstOrigin::Desugared("verifier.extern_adapter");
            let func_meta = crate::ast::AstNodeMeta::inherited(func_span, adapter_origin);
            let declaration = crate::ast::ExternFunc {
                meta: func_meta,
                name: signature.name.clone(),
                params: signature
                    .typed_params
                    .iter()
                    .map(|(name, ty, cap_mode)| crate::ast::ExternParam {
                        meta: func_meta,
                        name: name.clone(),
                        ty: ty.clone().deep_reorigin(func_meta),
                        cap_mode: *cap_mode,
                    })
                    .collect(),
                ret: signature
                    .ret_type
                    .clone()
                    .map(|ty| ty.deep_reorigin(func_meta)),
                requires: signature.requires.clone(),
                ensures: signature.ensures.clone(),
                variadic: signature.variadic,
                no_panic: signature.no_panic || block.no_panic,
                returns_errno: false,
            };
            if externs
                .insert(signature.name.clone(), declaration)
                .is_some()
            {
                return Err(format!(
                    "TOOL-RESOLUTION-001: duplicate resolved extern symbol '{}'",
                    signature.name
                ));
            }
        }
    }
    for site in program.call_sites().values() {
        if site.kind != crate::core::ResolvedCallKind::Extern {
            continue;
        }
        let signature = program.extern_func_signature(&site.callee).ok_or_else(|| {
            format!(
                "TOOL-RESOLUTION-001: missing resolved extern signature for call '{}'",
                site.callee
            )
        })?;
        if site.argc != signature.params.len() {
            return Err(format!(
                "TOOL-RESOLUTION-001: extern call '{}' expects {} arguments, got {}",
                site.callee,
                signature.params.len(),
                site.argc
            ));
        }
    }
    if is_z3_available() {
        // C4 Z3 path (permanent): FFI call-site verification encodes extern
        // contract expressions from surface AST. The explicitly tagged legacy
        // body boundary is required because
        // the Z3 encoding is defined over AST Expr nodes.
        flow::flow_verify_ffi_call_sites_with_externs_or_mock(
            program.legacy_body_file(crate::core::LegacyBodyConsumer::FfiVerifierCompatibility),
            &externs,
        )
    } else {
        // C4 mock path: from CheckedProgram's extern signatures, no retained
        // surface body needed.
        let mut results: Vec<VerificationResult> = Vec::new();
        for block in program.extern_blocks().values() {
            for signature in &block.signatures {
                if signature.requires.is_some() || signature.ensures.is_some() {
                    results.push(VerificationResult {
                        func_name: format!("extern {}", signature.name),
                        status: VerifStatus::InfrastructureError,
                        message: "Z3 solver not available".into(),
                        diagnostic: None,
                        duration_us: 0,
                        constraint_count: 0,
                        artifact: None,
                        trusted_subset_domain: None,
                    });
                }
            }
        }
        Ok(results)
    }
}

/// Check whether the Z3 solver is available at runtime.
pub fn is_z3_available() -> bool {
    Verifier::new().is_ok()
}

/// 0.34.44 (ADR-008 §3): dual-engine verification with fail-closed divergence.
///
/// Runs BOTH engines on the same checked program and merges their verdicts:
/// - primary = the Resolved engine (ADR-008 §1 main judgment);
/// - secondary = the flow/VIR engine (demoted `math:` channel, retirement on
///   the 0.2 track);
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
    if let Some(results) = verify_closed_mir_program(program, source_hash.clone())? {
        return Ok(results);
    }
    let resolved_ir_hash = ctx::compute_resolved_ir_hash(program);
    if !is_z3_available() {
        // C4 mock path: from CheckedProgram, no retained surface body needed.
        return Ok(ctx::mock_verify_checked(program));
    }
    // Primary engine: resolved (verifies from Resolved IR).
    let mut primary = Verifier::new()?;
    primary.set_source_hash(source_hash.clone());
    let resolved_results = primary.verify_checked(program);
    // Secondary engine: flow/VIR (encodes surface AST bodies).
    let flow_results = flow::flow_verify_file_with_hashes(
        program.legacy_body_file(crate::core::LegacyBodyConsumer::DualVerifierCompatibility),
        source_hash,
        resolved_ir_hash,
    )?;
    Ok(merge_engine_verdicts(resolved_results, flow_results))
}

/// Try the already-closed verifier profile matrix from canonical MIR.
///
/// The public checked verifier is a program-level API. Once checker-owned
/// eligibility has established one of the exact closed profiles, its old
/// AST/Flow engine is no longer a valid consumer for this request. The profile
/// matrix below is owned by the shared MIR route module; construction,
/// materialized operation coverage, MIR capability, and the MIR verifier are
/// one hard-fail-closed chain. Programs outside every exact profile return
/// `None` so the existing compatibility verifier remains an explicit boundary
/// for unmigrated features.
fn verify_closed_mir_program(
    program: &crate::core::CheckedProgram,
    source_hash: String,
) -> Result<Option<Vec<VerificationResult>>, String> {
    const PROFILES: [crate::core::mir::CanonicalMirRouteProfile; 6] = [
        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection,
        crate::core::mir::CanonicalMirRouteProfile::FlatCopyRecord,
        crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition,
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant,
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate,
        crate::core::mir::CanonicalMirRouteProfile::CopyOptionI32Variant,
    ];
    for profile in PROFILES {
        if let Some(results) = verify_closed_mir_profile(program, profile, source_hash.clone())? {
            return Ok(Some(results));
        }
    }
    Ok(None)
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
        crate::core::mir::CanonicalMirRouteProfile::ScalarCollection => {
            crate::core::mir::validate_scalar_collection_island(&canonical).map_err(|errors| {
                format!(
                    "MIR-CAPABILITY-001: canonical verifier rejected the scalar collection island: {errors:?}"
                )
            })?;
        }
        crate::core::mir::CanonicalMirRouteProfile::FlatCopyRecord
        | crate::core::mir::CanonicalMirRouteProfile::S8FlowTransition => {}
        crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant => {
            crate::core::mir::validate_option_string_variant_island(&canonical).map_err(
                |errors| {
                    format!(
                        "MIR-CAPABILITY-001: canonical verifier rejected the Option<string> variant island: {errors:?}"
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
        crate::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate => {}
    }
    crate::verifier::validate_mir_capabilities(&canonical).map_err(|errors| {
        format!(
            "MIR-CAPABILITY-001: canonical verifier TypeDesc/capability gate rejected the {}: {errors:?}",
            profile.as_str()
        )
    })?;
    let mut results = crate::verifier::verify_mir(&canonical, source_hash)?;
    // Public checked-verifier APIs historically expose source-level callable
    // names. Keep that display contract at the adapter boundary while proof
    // artifacts retain the canonical `function:` owner and MIR hash.
    for result in &mut results {
        if let Some(name) = result.func_name.strip_prefix("function:") {
            result.func_name = name.to_string();
        }
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
    if !is_z3_available() {
        // Preserve the existing CheckedProgram mock infrastructure boundary;
        // without Z3 the public API does not claim a MIR proof.
        return Ok(None);
    }
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if !island.is_admitted(admission) {
        return Ok(None);
    }
    let route = match crate::core::mir::materialize_canonical_mir_route(program, None) {
        Ok(route) => route,
        Err(crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility { .. }) => {
            return Ok(None)
        }
        Err(error) => {
            let code = match error {
                crate::core::mir::CanonicalMirRouteMaterializationError::Complete {
                    stage, ..
                } => match stage {
                    crate::core::mir::CanonicalMirRouteFailureStage::Construction => {
                        "MIR-MATERIALIZATION-001"
                    }
                    crate::core::mir::CanonicalMirRouteFailureStage::Coverage => "MIR-COVERAGE-001",
                },
                crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility {
                    ..
                } => {
                    unreachable!("compatibility errors are returned above")
                }
            };
            return Err(format!("{code}: {error}"));
        }
    };
    if !island.is_materialized(&route) {
        return Err(format!(
            "MIR-COVERAGE-001: {} admission did not materialize its canonical operation",
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

/// Merge per-function verdicts from the two engines (ADR-008 §3).
///
/// Rules:
/// - both agree (same class) → primary (resolved) result wins;
/// - one side is NoOpinion → the other side's result wins silently (no proof
///   was attempted on the silent side, so there is nothing to disagree with);
/// - classes disagree → fail-closed: Disproven beats Inconclusive beats
///   Proven, and the merged result carries the E0439 divergence diagnostic.
fn merge_engine_verdicts(
    primary: Vec<VerificationResult>,
    secondary: Vec<VerificationResult>,
) -> Vec<VerificationResult> {
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
            "[{}] engine divergence for '{}': resolved={:?} vs flow_ast={:?}; \
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
                 flow_ast={:?}); the weaker conclusion wins",
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
