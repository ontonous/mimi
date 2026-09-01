//! Native MIR admission and symbol eligibility.
//! This module owns only pre-emission admission plumbing; shape rules live in
//! the validator and TypeDesc ABI modules.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeMirError {
    pub(super) subject: String,
    pub(super) message: String,
}

impl NativeMirError {
    pub(super) fn new(subject: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            message: message.into(),
        }
    }

    pub(super) fn diagnostic(self) -> Diagnostic {
        Diagnostic::error(
            format!(
                "canonical MIR native backend rejected {}: {}",
                self.subject, self.message
            ),
            Span::UNKNOWN,
        )
    }
}

/// Validate a Canonical MIR program against the native shape contract without
/// creating LLVM declarations.  The CLI uses this as part of the atomic
/// default-route capability gate so run/build/verify make the same route
/// decision before any production backend starts.
pub fn validate_mir_native(program: &MirProgram) -> Result<(), Vec<Diagnostic>> {
    NativeMirValidator::new(program)
        .validate()
        .map_err(|errors| errors.into_iter().map(NativeMirError::diagnostic).collect())
}

/// Compile a validated scalar/flat-aggregate MIR program directly to LLVM.
///
/// This is an explicit migration entry point. It is not used by the default
/// `build` path until the wider MIR shape and differential gates are closed.
impl<'ctx> CodeGenerator<'ctx> {
    pub fn compile_mir_native(&mut self, program: &MirProgram) -> Result<(), Vec<Diagnostic>> {
        validate_mir_native(program)?;

        NativeMirEmitter::new(self, program)
            .compile()
            .map_err(|error| vec![error.diagnostic()])
    }
}

pub(super) fn instruction_kind(
    instruction: &crate::core::mir::MirInstruction,
) -> &MirInstructionKind {
    &instruction.kind
}

pub(super) fn mir_symbol(owner: &crate::core::NodeId) -> Result<&str, String> {
    let symbol = owner
        .0
        .strip_prefix("function:")
        .ok_or_else(|| "callable identity is not a function owner".to_string())?;
    if symbol.trim().is_empty() || symbol.contains("::") {
        return Err("only simple function symbols are in the native MIR slice".into());
    }
    if symbol.starts_with("mimi_") {
        return Err("function symbol collides with reserved runtime namespace".into());
    }
    Ok(symbol)
}

pub(super) fn native_symbol_fragment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}
