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
    flow::flow_verify_file_or_mock(program.file())
}

/// Parse source and verify extern call sites using Z3.
pub fn verify_ffi_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| e.message)?;
    flow::flow_verify_ffi_call_sites_or_mock(&file)
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
