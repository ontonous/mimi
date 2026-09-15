use serde_json::Value;

use crate::diagnostic::{Diagnostic, Severity};
use crate::lexer::LexerError;
use crate::lsp::position;

pub(crate) fn severity_to_lsp(severity: &Severity) -> i32 {
    match severity {
        Severity::Error => 1,
        Severity::Warning => 2,
        Severity::Note => 3,
        Severity::Help => 4,
    }
}

fn diagnostic_range(span: &crate::span::Span, text: Option<&str>) -> Value {
    let mut range = match text {
        Some(text) => crate::lsp::position_map::PositionMap::new(text).span_to_lsp(
            span.start_line,
            span.start_col,
            span.end_line,
            span.end_col,
        ),
        None => position::span_to_range(span),
    };
    if range["start"] == range["end"] {
        let start = range["start"]["character"].as_u64().unwrap_or(0);
        range["end"]["character"] = Value::from(start.saturating_add(1));
    }
    range
}

pub(crate) fn diagnostic_to_lsp(diagnostic: &Diagnostic, text: Option<&str>) -> Value {
    let code = diagnostic.code.clone().unwrap_or_default();
    let mut value = serde_json::json!({
        "range": diagnostic_range(&diagnostic.span, text),
        "severity": severity_to_lsp(&diagnostic.severity),
        "source": "mimi",
        "code": code,
        "message": diagnostic.message
    });
    if let Some(origin) = &diagnostic.origin {
        value["data"] = serde_json::json!({ "origin": origin });
    }
    value
}

pub(crate) fn lexer_error_to_lsp(err: &LexerError, text: Option<&str>) -> Value {
    let (line, col) = err.position();
    serde_json::json!({
        "range": diagnostic_range(&crate::span::Span::single(line, col), text),
        "severity": 1,
        "source": "mimi",
        "message": err.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::codes::{describe, MIR_ROUTE_MANIFEST_ERROR_CODE};
    use crate::span::Span;

    #[test]
    fn lsp_serialization_preserves_registered_mir_code_and_source_span() {
        assert_eq!(
            describe(MIR_ROUTE_MANIFEST_ERROR_CODE),
            "canonical MIR route manifest rejected"
        );
        let diagnostic = Diagnostic::error_code(
            MIR_ROUTE_MANIFEST_ERROR_CODE,
            "invalid MIR route manifest: future field",
            Span::new(2, 3, 2, 9),
        );
        let serialized = diagnostic_to_lsp(&diagnostic, Some("first\nprofile"));
        assert_eq!(serialized["source"], "mimi");
        assert_eq!(serialized["code"], MIR_ROUTE_MANIFEST_ERROR_CODE);
        assert_eq!(serialized["range"]["start"]["line"], 1);
        assert_eq!(serialized["range"]["start"]["character"], 2);
        assert_eq!(serialized["range"]["end"]["line"], 1);
        assert_eq!(serialized["range"]["end"]["character"], 7);
    }

    #[test]
    fn lsp_serialization_preserves_source_less_route_provenance() {
        let diagnostic = Diagnostic::error_code(
            MIR_ROUTE_MANIFEST_ERROR_CODE,
            "canonical route manifest rejected",
            Span::UNKNOWN,
        )
        .with_origin(crate::diagnostic::DiagnosticOrigin::runtime_system(
            "mir.route",
        ));
        let serialized = diagnostic_to_lsp(&diagnostic, None);

        assert_eq!(serialized["code"], MIR_ROUTE_MANIFEST_ERROR_CODE);
        assert_eq!(serialized["range"]["start"]["line"], 0);
        assert_eq!(serialized["range"]["end"]["character"], 1);
        assert_eq!(serialized["data"]["origin"]["kind"], "runtime_system");
        assert_eq!(serialized["data"]["origin"]["rule"], "mir.route");
    }
}
