mod ctx;
mod expr;
mod flow;
mod func;
mod helpers;

pub mod ffi;

pub(crate) use ctx::Z3VarMap;
pub use ctx::{Counterexample, VerifStatus, VerificationResult, Verifier};
pub(crate) use ctx::{SolverSession, VerifierCtx};
pub use flow::{
    flow_verify_ffi_call_sites, flow_verify_ffi_call_sites_or_mock, FlowAcc, FlowEvent,
    VerifierState,
};

/// Verify contracts in source text.
pub fn verify_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| e.message)?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    verify_checked(&program)
}

/// Verify contracts using a caller-provided verifier (for timeout/config tests).
pub fn verify_source_with(
    source: &str,
    verifier: &mut Verifier,
) -> Result<Vec<VerificationResult>, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| e.message)?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    Ok(verifier.verify_checked(&program))
}

/// Verify contracts in a type-checked program (supports pre-merged imports).
pub fn verify_checked(
    program: &crate::core::CheckedProgram<'_>,
) -> Result<Vec<VerificationResult>, String> {
    program
        .validate_backend(crate::core::BackendProfile::Verifier)
        .map_err(format_check_errors)?;
    flow::flow_verify_file_or_mock(program.legacy_body_file())
}

/// Parse source and verify extern call sites using Z3.
pub fn verify_ffi_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| e.message)?;
    let program = crate::core::check_program(&file).map_err(format_check_errors)?;
    verify_ffi_checked(&program)
}

/// Verify extern call sites from a checked program.
///
/// Contract expressions still use the explicit legacy body adapter until
/// typed Verification IR lands, but declaration identity and arity are
/// authoritative from CheckedProgram and fail closed before that adapter.
pub fn verify_ffi_checked(
    program: &crate::core::CheckedProgram<'_>,
) -> Result<Vec<VerificationResult>, String> {
    let mut externs = std::collections::HashMap::new();
    for block in program.extern_blocks().values() {
        for signature in &block.signatures {
            let declaration = crate::ast::ExternFunc {
                name: signature.name.clone(),
                params: signature
                    .typed_params
                    .iter()
                    .map(|(name, ty, cap_mode)| crate::ast::ExternParam {
                        name: name.clone(),
                        ty: ty.clone(),
                        cap_mode: *cap_mode,
                    })
                    .collect(),
                ret: signature.ret_type.clone(),
                requires: signature.requires.clone(),
                ensures: signature.ensures.clone(),
                variadic: signature.variadic,
                no_panic: signature.no_panic || block.no_panic,
            };
            if externs.insert(signature.name.clone(), declaration).is_some() {
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
    flow::flow_verify_ffi_call_sites_with_externs_or_mock(program.legacy_body_file(), &externs)
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
