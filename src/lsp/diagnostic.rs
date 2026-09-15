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

/// Normalize one LSP diagnostic batch for deterministic clients and cache
/// snapshots.  The key keeps source position first, then severity/code/text
/// and serialized provenance; exact duplicate payloads collapse only after
/// sorting, so diagnostics at distinct spans or with distinct origins remain.
pub(crate) fn normalize_diagnostics(mut diagnostics: Vec<Value>) -> Vec<Value> {
    diagnostics.sort_by_key(lsp_diagnostic_sort_key);
    diagnostics.dedup();
    diagnostics
}

fn lsp_diagnostic_sort_key(
    value: &Value,
) -> (u64, u64, u64, u64, u64, String, String, String, String) {
    let range = &value["range"];
    let start = &range["start"];
    let end = &range["end"];
    (
        start["line"].as_u64().unwrap_or(u64::MAX),
        start["character"].as_u64().unwrap_or(u64::MAX),
        end["line"].as_u64().unwrap_or(u64::MAX),
        end["character"].as_u64().unwrap_or(u64::MAX),
        value["severity"].as_u64().unwrap_or(u64::MAX),
        value["code"].as_str().unwrap_or_default().to_owned(),
        value["message"].as_str().unwrap_or_default().to_owned(),
        value["source"].as_str().unwrap_or_default().to_owned(),
        value
            .get("data")
            .map(ToString::to_string)
            .unwrap_or_default(),
    )
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
    fn lsp_serialization_preserves_shared_route_factory_source_span() {
        let diagnostic = crate::diagnostic::mir_route_error_diagnostic(
            format!("native admission wrapper: {MIR_ROUTE_MANIFEST_ERROR_CODE}: future field"),
            Span::new(2, 3, 2, 9),
        );
        let serialized = diagnostic_to_lsp(&diagnostic, Some("first\nprofile"));

        assert_eq!(serialized["code"], MIR_ROUTE_MANIFEST_ERROR_CODE);
        assert_eq!(serialized["range"]["start"]["line"], 1);
        assert_eq!(serialized["range"]["start"]["character"], 2);
        assert_eq!(serialized["range"]["end"]["character"], 7);
        assert_eq!(serialized["data"]["origin"]["kind"], "runtime_system");
        assert_eq!(serialized["data"]["origin"]["rule"], "mir.route");
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

    #[test]
    fn lsp_serialization_preserves_unknown_future_code() {
        let diagnostic =
            Diagnostic::error_code("MIR-FUTURE-999", "future route diagnostic", Span::UNKNOWN);
        let serialized = diagnostic_to_lsp(&diagnostic, None);

        assert_eq!(serialized["code"], "MIR-FUTURE-999");
        assert_eq!(serialized["message"], "future route diagnostic");
        assert!(serialized.get("data").is_none());
    }

    #[test]
    fn lsp_normalization_sorts_and_folds_exact_duplicates() {
        let first = diagnostic_to_lsp(
            &Diagnostic::error("ordinary failure", Span::new(1, 1, 1, 2)),
            Some("a\nb"),
        );
        let route = diagnostic_to_lsp(
            &crate::diagnostic::mir_route_error_diagnostic(
                format!("wrapper: {MIR_ROUTE_MANIFEST_ERROR_CODE}: stale receipt"),
                Span::new(2, 1, 2, 4),
            ),
            Some("a\nb"),
        );
        let mut normalized = normalize_diagnostics(vec![route.clone(), first, route]);

        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized.remove(0)["message"], "ordinary failure");
        assert_eq!(normalized.remove(0)["code"], MIR_ROUTE_MANIFEST_ERROR_CODE);
    }
}
