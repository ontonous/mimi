mod ctx;
mod expr;
mod flow;
mod func;
mod helpers;

pub mod ffi;

pub(crate) use ctx::Z3VarMap;
pub use ctx::{Counterexample, VerifStatus, VerificationResult, Verifier};
pub use flow::{
    flow_verify_ffi_call_sites, flow_verify_ffi_call_sites_or_mock, flow_verify_file,
    flow_verify_file_or_mock, flow_verify_source, FlowAcc, FlowEvent, VerifierState,
};

use crate::ast::File;

/// Verify contracts in source text.
pub fn verify_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    flow::flow_verify_source(source)
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
    Ok(verifier.verify_file(&file))
}

/// Verify contracts in a parsed file (supports pre-merged imports).
pub fn verify_file(file: &File) -> Result<Vec<VerificationResult>, String> {
    flow::flow_verify_file_or_mock(file)
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

#[cfg(test)]
mod tests;
