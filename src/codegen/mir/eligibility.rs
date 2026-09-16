//! Native MIR admission and symbol eligibility.
//! This module owns only pre-emission admission plumbing; shape rules live in
//! the validator and TypeDesc ABI modules.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeMirError {
    pub(super) subject: String,
    pub(super) message: String,
    pub(super) span: Span,
    /// Optional cross-layer diagnostic code.  Most native shape failures are
    /// ordinary backend diagnostics; route receipt failures additionally
    /// expose their stable code structurally instead of hiding it in text.
    pub(super) code: Option<&'static str>,
}

impl NativeMirError {
    pub(super) fn new(subject: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            message: message.into(),
            span: Span::UNKNOWN,
            code: None,
        }
    }

    pub(super) fn with_span(mut self, span: Span) -> Self {
        self.span = span;
        self
    }

    pub(super) fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub(super) fn diagnostic(self) -> Diagnostic {
        let message = format!(
            "canonical MIR native backend rejected {}: {}",
            self.subject, self.message
        );
        match self.code {
            Some(code) => Diagnostic::error_code(code, message, self.span).with_origin(
                crate::diagnostic::DiagnosticOrigin::runtime_system("mir.route"),
            ),
            None => Diagnostic::error(message, self.span),
        }
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
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                return Ok(());
            }
            return Err(vec![NativeMirError::new(
                "mir-program",
                "native generator already contains a different canonical MIR program",
            )
            .diagnostic()]);
        }
        validate_mir_native(program)?;

        NativeMirEmitter::new(self, program)
            .compile()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        Ok(())
    }

    /// Compile canonical MIR after checking the route receipt supplied by the
    /// caller. This keeps native emission on the exact immutable graph whose
    /// receipt admitted the route, matching the bytecode consumer boundary.
    pub fn compile_mir_native_with_route_receipt(
        &mut self,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        self.compile_mir_native(program)
    }

    /// Compile canonical MIR after parsing and checking a CLI route manifest.
    /// Keep this manifest boundary beside the typed receipt entry point so
    /// native emission cannot accept an unvalidated field extension.
    pub fn compile_mir_native_with_route_manifest(
        &mut self,
        program: &MirProgram,
        manifest: &str,
    ) -> Result<(), Vec<Diagnostic>> {
        let receipt = crate::core::mir::CanonicalMirRouteReceipt::from_manifest(manifest).map_err(
            |message| {
                vec![NativeMirError::new(
                    "mir-program",
                    format!(
                        "{}: canonical route manifest rejected: {message}",
                        crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE)
                .diagnostic()]
            },
        )?;
        self.compile_mir_native_with_route_receipt(program, &receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::NativeMirError;
    use crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE;

    #[test]
    fn route_errors_keep_structured_code_when_rendered() {
        let diagnostic = NativeMirError::new("mir-program", "future field")
            .with_code(MIR_ROUTE_MANIFEST_ERROR_CODE)
            .diagnostic();

        assert_eq!(
            diagnostic.code.as_deref(),
            Some(MIR_ROUTE_MANIFEST_ERROR_CODE)
        );
        assert_eq!(
            diagnostic.to_string(),
            format!(
                "[{}] canonical MIR native backend rejected mir-program: future field",
                MIR_ROUTE_MANIFEST_ERROR_CODE
            )
        );
        let origin = diagnostic.origin.expect("route error has provenance");
        assert_eq!(
            origin.kind,
            crate::diagnostic::DiagnosticOriginKind::RuntimeSystem
        );
        assert_eq!(origin.rule.as_deref(), Some("mir.route"));
    }
}

pub(super) fn instruction_kind(
    instruction: &crate::core::mir::MirInstruction,
) -> &MirInstructionKind {
    &instruction.kind
}

pub(super) fn mir_symbol(owner: &crate::core::NodeId) -> Result<String, String> {
    let Some(symbol) = owner.0.strip_prefix("function:") else {
        let transition = owner
            .0
            .strip_prefix("transition:")
            .ok_or_else(|| "callable identity is not a function or transition owner".to_string())?;
        if transition.trim().is_empty() {
            return Err("transition symbol must not be empty".into());
        }
        return Ok(format!(
            "__mimi_transition_{}",
            native_symbol_fragment(transition)
        ));
    };
    if symbol.trim().is_empty() {
        return Err("only simple function symbols are in the native MIR slice".into());
    }
    if symbol.starts_with("mimi_") {
        return Err("function symbol collides with reserved runtime namespace".into());
    }
    if symbol.contains("::") {
        // Protocol method owners are checker-canonical identities such as
        // `Read:for:Counter::read:<hash>`.  They are not surface names and
        // therefore need a deterministic LLVM-safe spelling, shared by the
        // declaration and call maps.  Keep ordinary function symbols
        // unchanged for compatibility with existing native callers.
        Ok(format!("__mimi_method_{}", native_symbol_fragment(symbol)))
    } else {
        Ok(symbol.to_owned())
    }
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
