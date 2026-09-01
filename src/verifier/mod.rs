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
/// When Z3 is available, delegates to the Flow verifier state machine
/// (which still uses `legacy_body_file()` for AST-based function body
/// encoding). When Z3 is unavailable, uses CheckedProgram-based mock
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
    // P1-24: compute Resolved IR hash from CheckedProgram signatures.
    let resolved_ir_hash = ctx::compute_resolved_ir_hash(program);
    if is_z3_available() {
        // C4 Z3 path (permanent): the Flow verifier encodes transition invariants
        // from surface AST body expressions. raw_ast() is required here because
        // the Z3 encoding is defined over AST Expr nodes, not ResolvedExpr.
        // The resolved_ir_hash is embedded in ProofArtifact by the flow verifier.
        flow::flow_verify_file_with_hashes(program.raw_ast(), source_hash, resolved_ir_hash)
    } else {
        // C4 mock path: from CheckedProgram, no raw_ast needed.
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
        // contract expressions from surface AST. raw_ast() is required because
        // the Z3 encoding is defined over AST Expr nodes.
        flow::flow_verify_ffi_call_sites_with_externs_or_mock(program.raw_ast(), &externs)
    } else {
        // C4 mock path: from CheckedProgram's extern signatures, no raw_ast needed.
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
    let resolved_ir_hash = ctx::compute_resolved_ir_hash(program);
    if !is_z3_available() {
        // C4 mock path: from CheckedProgram, no raw_ast needed.
        return Ok(ctx::mock_verify_checked(program));
    }
    // Primary engine: resolved (verifies from Resolved IR).
    let mut primary = Verifier::new()?;
    primary.set_source_hash(source_hash.clone());
    let resolved_results = primary.verify_checked(program);
    // Secondary engine: flow/VIR (encodes surface AST bodies).
    let flow_results =
        flow::flow_verify_file_with_hashes(program.raw_ast(), source_hash, resolved_ir_hash)?;
    Ok(merge_engine_verdicts(resolved_results, flow_results))
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
