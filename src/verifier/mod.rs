mod ctx;
mod expr;
mod flow;
mod func;
mod helpers;
pub(crate) mod resolved_expr;
pub mod vir;

pub mod ffi;

pub(crate) use ctx::Z3VarMap;
pub use ctx::{
    Counterexample, ProofArtifact, TrustedSubsetDomain, VerifStatus, VerificationResult, Verifier,
};
pub(crate) use ctx::{SolverSession, VerifierCtx};
pub use flow::{
    flow_verify_ffi_call_sites, flow_verify_ffi_call_sites_or_mock, FlowAcc, FlowEvent,
    VerifierState,
};

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
        // C4 Z3 path: still uses AST body encoding via flow verifier.
        // The resolved_ir_hash is embedded in ProofArtifact by the flow verifier.
        flow::flow_verify_file_with_hashes(
            program.legacy_body_file(),
            source_hash,
            resolved_ir_hash,
        )
    } else {
        // C4 mock path: from CheckedProgram, no legacy_body_file needed.
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
        // C4 Z3 path: still uses AST body encoding via flow verifier.
        // The _or_mock variant uses the Z3 path internally since we checked availability.
        flow::flow_verify_ffi_call_sites_with_externs_or_mock(
            program.legacy_body_file(),
            &externs,
        )
    } else {
        // C4 mock path: from CheckedProgram's extern signatures, no legacy_body_file.
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
