pub mod codes;
pub mod format;

use crate::span::Span;

/// Owned provenance attached to a runtime diagnostic.
///
/// Unlike AST provenance, every field here owns its text so diagnostics may
/// safely cross cache/serialization boundaries without leaking strings into
/// `&'static str`. `parent_node_id`, when present, is the canonical resolved
/// parent NodeId rather than a source-order/index surrogate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticOrigin {
    pub kind: DiagnosticOriginKind,
    pub rule: Option<String>,
    pub parent_node_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticOriginKind {
    User,
    Desugared,
    PrototypeFallback,
    RuntimeSystem,
}

impl DiagnosticOrigin {
    pub const fn user() -> Self {
        Self {
            kind: DiagnosticOriginKind::User,
            rule: None,
            parent_node_id: None,
        }
    }

    /// Mark a diagnostic emitted by a compiler/runtime admission boundary.
    /// The rule is owned text so the provenance survives serde/cache hops.
    pub fn runtime_system(rule: impl Into<String>) -> Self {
        Self {
            kind: DiagnosticOriginKind::RuntimeSystem,
            rule: Some(rule.into()),
            parent_node_id: None,
        }
    }
}

/// Severity level for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Note => write!(f, "note"),
            Severity::Help => write!(f, "help"),
        }
    }
}

/// An attached note with its own span (e.g., "previous definition here").
#[derive(Debug, Clone)]
pub struct DiagnosticNote {
    pub message: String,
    pub span: Span,
}

/// A rich diagnostic message with span, severity, error code, notes, and help.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
    pub severity: Severity,
    pub code: Option<String>,
    pub notes: Vec<DiagnosticNote>,
    pub help: Option<String>,
    pub origin: Option<DiagnosticOrigin>,
}

impl Diagnostic {
    /// Create a new error diagnostic at a given span.
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            severity: Severity::Error,
            code: None,
            notes: Vec::new(),
            help: None,
            origin: None,
        }
    }

    /// Create a new error diagnostic with an error code.
    pub fn error_code(code: &str, message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            severity: Severity::Error,
            code: Some(code.to_string()),
            notes: Vec::new(),
            help: None,
            origin: None,
        }
    }

    /// Create a new warning diagnostic.
    pub fn warning(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            severity: Severity::Warning,
            code: None,
            notes: Vec::new(),
            help: None,
            origin: None,
        }
    }

    /// Create a new warning diagnostic with an error code.
    pub fn warning_code(code: &str, message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            severity: Severity::Warning,
            code: Some(code.to_string()),
            notes: Vec::new(),
            help: None,
            origin: None,
        }
    }

    /// Attach a note to this diagnostic.
    pub fn with_note(mut self, message: impl Into<String>, span: Span) -> Self {
        self.notes.push(DiagnosticNote {
            message: message.into(),
            span,
        });
        self
    }

    /// Attach a help message to this diagnostic.
    pub fn with_help(mut self, message: impl Into<String>) -> Self {
        self.help = Some(message.into());
        self
    }

    /// Attach an error code.
    pub fn with_code(mut self, code: &str) -> Self {
        self.code = Some(code.to_string());
        self
    }

    /// Replace the primary span of this diagnostic.
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = span;
        self
    }

    /// Attach owned semantic provenance.
    pub fn with_origin(mut self, origin: DiagnosticOrigin) -> Self {
        self.origin = Some(origin);
        self
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(code) = &self.code {
            write!(f, "[{}] {}", code, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for Diagnostic {}

/// Build a diagnostic for a Canonical MIR route failure while preserving the
/// shared machine-readable code and runtime provenance.  Adapter-specific
/// wrappers may prepend context to the historical message; classification is
/// therefore token-boundary aware and deliberately leaves ordinary errors
/// without a code.
pub fn mir_route_error_diagnostic(message: impl Into<String>, span: Span) -> Diagnostic {
    let message = message.into();
    match codes::canonical_mir_route_code_in_message(&message) {
        Some(code) => Diagnostic::error_code(code, message, span)
            .with_origin(DiagnosticOrigin::runtime_system("mir.route")),
        None => Diagnostic::error(message, span),
    }
}

/// Legacy bridge for genuinely global diagnostics.  `Span::UNKNOWN` is an
/// explicit lack of location; it must not be presented as a real `(0, 0)`
/// source coordinate by diagnostic consumers.
impl From<&str> for Diagnostic {
    fn from(msg: &str) -> Self {
        Self::error(msg, Span::UNKNOWN)
    }
}

impl From<String> for Diagnostic {
    fn from(msg: String) -> Self {
        Self::error(msg, Span::UNKNOWN)
    }
}

#[cfg(test)]
mod tests {
    use super::{mir_route_error_diagnostic, DiagnosticOriginKind};
    use crate::diagnostic::codes::{
        MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE, MIR_ROUTE_MANIFEST_ERROR_CODE,
        MIR_ROUTE_RECEIPT_ERROR_CODE,
    };
    use crate::span::{SourceId, Span};

    #[test]
    fn mir_route_error_diagnostic_has_stable_code_origin_and_span() {
        let span = Span::new(3, 11, 3, 11).with_source(SourceId::new(7));
        let diagnostic = mir_route_error_diagnostic(
            format!("adapter context: {MIR_ROUTE_RECEIPT_ERROR_CODE}: stale receipt"),
            span,
        );
        assert_eq!(
            diagnostic.code.as_deref(),
            Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        );
        assert_eq!(diagnostic.span, span);
        let origin = diagnostic.origin.expect("route diagnostic provenance");
        assert_eq!(origin.kind, DiagnosticOriginKind::RuntimeSystem);
        assert_eq!(origin.rule.as_deref(), Some("mir.route"));

        let declaration_boundary = mir_route_error_diagnostic(
            format!(
                "default Canonical MIR route rejected: {MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE}: canonical scalar FFI declaration boundary: extern declaration 'foreign' result type is outside canonical scalar FFI"
            ),
            span,
        );
        assert_eq!(
            declaration_boundary.code.as_deref(),
            Some(MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE)
        );

        let ordinary = mir_route_error_diagnostic(
            format!("{MIR_ROUTE_MANIFEST_ERROR_CODE}-extra: ordinary failure"),
            span,
        );
        assert!(ordinary.code.is_none());
        assert!(ordinary.origin.is_none());
        assert_eq!(ordinary.span, span);
    }
}
