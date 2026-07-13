#![allow(dead_code)]

use serde_json::Value;

use crate::span::Span;

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
