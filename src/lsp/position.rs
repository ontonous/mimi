#![allow(dead_code)]

use serde_json::Value;

use crate::span::Span;

/// LOSSY no-text fallback for Span → LSP range.
///
/// X-11 (full audit 2026-08-05 §3.10, closed 0.36.82 by design): Mimi span
/// columns are 1-indexed CHAR counts (lexer advances per char); LSP
/// `character` is UTF-16 code units. The exact conversion walks the line's
/// chars summing `len_utf16` (AU-LSP-3, `PositionMap::span_to_lsp`) and
/// REQUIRES the document text. Without text this fallback can only subtract
/// the 1-based bias, which is exact on pure-ASCII lines and drifts when
/// supplementary-plane chars (e.g. emoji, 1 char = 2 UTF-16 units) precede
/// the span on its line.
///
/// The residual lossiness is inherent to the no-text case: no algorithm can
/// recover UTF-16 units from a bare Span without the source line. Production
/// callers already prefer the text-based path (`diagnostic_to_lsp` with
/// `Some(text)`); this exists solely for diagnostics whose source text
/// cannot be recovered, and its documented lossy behavior is the bounded
/// fallback rather than a silent miscompile.
pub(crate) fn span_to_range(span: &Span) -> Value {
    serde_json::json!({
        "start": {
            "line": span.start_line.saturating_sub(1),
            "character": span.start_col.saturating_sub(1)
        },
        "end": {
            "line": span.end_line.saturating_sub(1),
            "character": span.end_col.saturating_sub(1)
        }
    })
}

/// B2: Convert LSP position (line, UTF-16 character) to byte offset.
/// Uses PositionMap for correct UTF-16 ↔ byte conversion.
pub(crate) fn position_to_offset(text: &str, line: usize, character: usize) -> usize {
    let map = super::position_map::PositionMap::new(text);
    map.lsp_to_byte(line, character)
}

/// B2: Convert byte offset to LSP position (line, UTF-16 character).
/// Uses PositionMap for correct byte ↔ UTF-16 conversion.
pub(crate) fn offset_to_position(text: &str, offset: usize) -> (usize, usize) {
    let map = super::position_map::PositionMap::new(text);
    map.byte_to_lsp(offset)
}
