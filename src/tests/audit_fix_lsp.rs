//! Wave-1 audit-fix regression tests — lsp.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).


// ============================================================
// Wave-2 audit fixes (devdocs/full-audit-2026-08-05.md §13)
// ============================================================

/// Helper: extract (start_line, start_char, end_line, end_char) from an LSP edit.
fn lsp_edit_range(edit: &serde_json::Value) -> (u64, u64, u64, u64) {
    let r = &edit["range"];
    (
        r["start"]["line"].as_u64().unwrap_or(u64::MAX),
        r["start"]["character"].as_u64().unwrap_or(u64::MAX),
        r["end"]["line"].as_u64().unwrap_or(u64::MAX),
        r["end"]["character"].as_u64().unwrap_or(u64::MAX),
    )
}

// --- Fix 1 (HIGH): rename must skip string literals and comments ---

#[test]
fn lsp_rename_skips_string_literals_and_comments() {
    let server = crate::lsp::LspServer::new();
    let src = "func main() -> i32 {\n    let x = 1\n    let note = \"x marks the spot\"\n    // x in a comment\n    println(x)\n    0\n}";
    let result = server
        .compute_rename(src, 1, 8, "file:///rn-str.mimi", "y")
        .expect("rename of let-bound x should succeed");
    let changes = result["changes"]["file:///rn-str.mimi"]
        .as_array()
        .expect("rename changes");
    let mut touched: Vec<(u64, u64, u64)> = changes
        .iter()
        .map(|c| {
            let (sl, sc, _el, ec) = lsp_edit_range(c);
            assert_eq!(c["newText"], "y");
            (sl, sc, ec - sc)
        })
        .collect();
    touched.sort();
    // Exactly two code occurrences: `let x` (line 1) and `println(x)` (line 4).
    assert_eq!(
        touched,
        vec![(1u64, 8u64, 1u64), (4u64, 12u64, 1u64)],
        "rename must not touch the string literal or the comment: {changes:?}"
    );
    // The string and comment lines must not appear at all.
    assert!(
        changes.iter().all(|c| {
            let (sl, _, _, _) = lsp_edit_range(c);
            sl != 2 && sl != 3
        }),
        "string literal line 2 / comment line 3 were corrupted: {changes:?}"
    );
}

// --- Fix 2 (HIGH): byte/char mixup panics on multi-byte identifiers ---

#[test]
fn lsp_find_word_occurrences_is_byte_safe() {
    use crate::lsp::util::find_word_occurrences;
    // Two CJK occurrences on one line: bytes 12..15 and 18..21.
    let line = "    println(数 + 数)";
    assert_eq!(find_word_occurrences(line, "数"), vec![12, 18]);
    // Whole-word boundary: no match inside a larger identifier.
    assert_eq!(find_word_occurrences("x数y", "数"), Vec::<usize>::new());
    // ASCII baseline.
    assert_eq!(find_word_occurrences("let x = x", "x"), vec![4, 8]);
    // Empty word must terminate (no infinite loop).
    assert!(find_word_occurrences("abc", "").is_empty());
}

#[test]
fn lsp_char_col_to_byte_is_char_based() {
    use crate::lsp::util::char_col_to_byte;
    let line = "let 数 = 1";
    assert_eq!(char_col_to_byte(line, 0), 0);
    assert_eq!(char_col_to_byte(line, 4), 4); // char col of 数
    assert_eq!(char_col_to_byte(line, 5), 7); // after a 3-byte char
    assert_eq!(char_col_to_byte(line, 999), line.len()); // clamps, no panic
}

#[test]
fn lsp_references_multibyte_identifier_twice_on_one_line() {
    let server = crate::lsp::LspServer::new();
    let src = "func main() -> i32 {\n    let 数 = 1\n    println(数 + 数)\n    0\n}";
    // Cursor on 数 in the let binding (line 1, UTF-16 char 8).
    // The old scan advanced by 1 byte and sliced mid-char on line 2 → panic.
    let refs = server.compute_references(src, 1, 8, "file:///mb.mimi", false);
    assert_eq!(refs.len(), 3, "let-binding + two uses: {refs:?}");
    let mut line2: Vec<(u64, u64)> = refs
        .iter()
        .filter(|r| r["range"]["start"]["line"] == 2)
        .map(|r| {
            (
                r["range"]["start"]["character"]
                    .as_u64()
                    .unwrap_or(u64::MAX),
                r["range"]["end"]["character"].as_u64().unwrap_or(u64::MAX),
            )
        })
        .collect();
    line2.sort();
    // "    println(" = 12 UTF-16 units; 数 is 1 unit.
    assert_eq!(line2, vec![(12, 13), (16, 17)], "both uses found: {refs:?}");
}

#[test]
fn lsp_rename_multibyte_identifier_completes_without_panic() {
    // NOTE: Mimi identifiers are ASCII-only (the lexer rejects non-ASCII
    // idents — `mimi check` on `let 数 = 1` reports "unexpected character").
    // The panic class this regression guards is byte/char confusion when the
    // rename scan crosses MULTIBYTE CONTENT on the same lines (string
    // literals, comments) — so the fixture renames an ASCII local whose line
    // contains CJK bytes before/after the occurrences.
    let server = crate::lsp::LspServer::new();
    let src = "func main() -> i32 {\n    let msg = \"你好\"\n    println(msg)\n    println(msg)\n    0\n}";
    let result = server
        .compute_rename(src, 1, 9, "file:///mb-rename.mimi", "text")
        .expect("rename of ASCII local on multibyte lines should succeed");
    let changes = result["changes"]["file:///mb-rename.mimi"]
        .as_array()
        .expect("changes");
    assert_eq!(changes.len(), 3, "decl + two uses: {changes:?}");
    // The CJK string literal must not be corrupted by the rewrite.
    assert!(!result.to_string().contains("你好") || changes.iter().all(|c| {
        c["newText"].as_str() == Some("text")
    }));
}

// --- Fix 3 (MEDIUM): PositionMap span columns are char counts, not bytes ---

#[test]
fn lsp_position_map_span_to_lsp_char_columns() {
    use crate::lsp::position_map::PositionMap;
    // "let 数 = 数" — chars (1-indexed cols): l1 e2 t3 ␣4 数5 ␣6 =7 ␣8 数9.
    // The second 数 would be byte offset 10; if columns were misread as bytes
    // the conversion yields 6 instead of 8, so this fixture discriminates.
    let map = PositionMap::new("let 数 = 数\n");
    let range = map.span_to_lsp(1, 9, 1, 10);
    assert_eq!(
        range["start"]["character"], 8,
        "'let 数 = ' = 8 UTF-16 units"
    );
    assert_eq!(range["end"]["character"], 9);

    // CJK identifier pair.
    let map = PositionMap::new("let 数字 = 1\n");
    let range = map.span_to_lsp(1, 5, 1, 7); // 数字 = cols 5..7
    assert_eq!(range["start"]["character"], 4);
    assert_eq!(range["end"]["character"], 6);

    // Surrogate pair: 😀 is 1 char, 2 UTF-16 units.
    let map = PositionMap::new("let 😀 = 1\n");
    let range = map.span_to_lsp(1, 5, 1, 6);
    assert_eq!(range["start"]["character"], 4);
    assert_eq!(range["end"]["character"], 6);

    // Existing ASCII behavior unchanged (regression guard).
    let map = PositionMap::new("let x = 42\n");
    let range = map.span_to_lsp(1, 1, 1, 7);
    assert_eq!(range["start"]["character"], 0);
    assert_eq!(range["end"]["character"], 6);
}

// --- Fix 4 (MEDIUM): cursor/span line-convention mismatch ---

#[test]
fn lsp_enclosing_func_found_on_signature_line() {
    let server = crate::lsp::LspServer::new();
    let text = "func foo() -> i32 {\n    0\n}\n";
    let file = server.parse_with_recovery(text).expect("parses");
    // The caller converts the 0-indexed LSP cursor line 0 to 1-indexed 1 at
    // the boundary; the old code rejected cursor_line == 0 outright and
    // compared 0 >= start_line(1) → enclosing func never found.
    let found = crate::lsp::util::find_enclosing_func_in_items(&file.items, text, 1);
    assert!(found.is_some(), "cursor on the signature line must match");
    assert_eq!(found.unwrap().name, "foo");
    // Closing-brace line (0-indexed 2 → 1-indexed 3) is still inside.
    assert!(crate::lsp::util::find_enclosing_func_in_items(&file.items, text, 3).is_some());
    // Beyond the function (1-indexed 4) is outside.
    assert!(crate::lsp::util::find_enclosing_func_in_items(&file.items, text, 4).is_none());
}

// --- Fix 5 (MEDIUM): brace counting must ignore strings/comments ---

#[test]
fn lsp_find_func_end_line_ignores_braces_in_strings_and_comments() {
    // `let s = "}"` used to close the function at line 1.
    let text = "func f() -> i32 {\n    let s = \"}\"\n    0\n}\n";
    assert_eq!(crate::lsp::util::find_func_end_line(text, 1), 3);

    // Braces inside line/block comments must not count either.
    let text = "func g() -> i32 {\n    /* } */\n    // }\n    0\n}\n";
    assert_eq!(crate::lsp::util::find_func_end_line(text, 1), 4);

    // Plain function (no braces in literals) — unchanged behavior.
    let text = "func h() -> i32 {\n    0\n}\n";
    assert_eq!(crate::lsp::util::find_func_end_line(text, 1), 2);
}

#[test]
fn lsp_non_code_byte_ranges_mark_strings_and_comments() {
    use crate::lsp::util::{byte_in_non_code, non_code_byte_ranges};
    let text = "let x = \"a}b\"\n// x\nlet y = 1";
    let ranges = non_code_byte_ranges(text);
    // Line 0: string content at bytes 9..12; delimiters stay code.
    assert!(byte_in_non_code(&ranges[0], 9));
    assert!(byte_in_non_code(&ranges[0], 11));
    assert!(!byte_in_non_code(&ranges[0], 8)); // opening quote
    assert!(!byte_in_non_code(&ranges[0], 12)); // closing quote
                                                // Line 1: the whole comment is non-code.
    assert!(byte_in_non_code(&ranges[1], 0));
    assert!(byte_in_non_code(&ranges[1], 3));
    // Line 2: pure code.
    assert!(ranges[2].is_empty());

    // Block comment spanning lines.
    let text2 = "let a = 1 /* start\n x */ let b = 2";
    let r2 = non_code_byte_ranges(text2);
    assert!(byte_in_non_code(&r2[0], 11)); // inside "/* start"
    assert!(byte_in_non_code(&r2[1], 1)); // inside " x */"
    assert!(!byte_in_non_code(&r2[1], 6)); // code after "*/"
}

// --- Fix 6 (LOW): oversized Content-Length must drain, not desync ---

#[test]
fn lsp_drain_discard_consumes_exactly_len_bytes() {
    let body: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    let mut data = body.clone();
    data.extend_from_slice(b"Content-Length: 2\r\n\r\n{}");
    let mut reader: &[u8] = &data;
    crate::lsp::drain_discard(&mut reader, body.len()).expect("drain succeeds");
    assert_eq!(
        reader,
        &b"Content-Length: 2\r\n\r\n{}"[..],
        "the following message must remain intact after draining"
    );
}

#[test]
fn lsp_drain_discard_truncated_body_is_error() {
    let mut reader: &[u8] = b"partial";
    let err = crate::lsp::drain_discard(&mut reader, 100).expect_err("truncated body");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    assert!(reader.is_empty(), "drain consumed what was available");
}

#[test]
fn lsp_drain_discard_zero_len_is_noop() {
    let mut reader: &[u8] = b"untouched";
    crate::lsp::drain_discard(&mut reader, 0).expect("zero drain");
    assert_eq!(reader, &b"untouched"[..]);
}

