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
    // wave1-review §6.4: the old assertion here was ALWAYS true — the rename
    // WorkspaceEdit JSON can never contain the source text, so the first
    // disjunct was vacuously true and CJK-literal integrity was never
    // actually tested. Apply the edits to the source and demand the exact
    // expected document: every code `msg` replaced, the CJK literal intact.
    let applied = audit2_apply_lsp_edits(src, changes);
    let expected = "func main() -> i32 {\n    let text = \"你好\"\n    println(text)\n    println(text)\n    0\n}";
    assert_eq!(
        applied, expected,
        "applying the rename edits must preserve the CJK literal and rewrite only the code occurrences: {changes:?}"
    );
}

/// Convert a UTF-16 character offset to a byte offset within one line.
fn audit2_utf16_to_byte(line: &str, utf16_offset: usize) -> usize {
    let mut units = 0usize;
    for (b, ch) in line.char_indices() {
        if units >= utf16_offset {
            return b;
        }
        units += ch.len_utf16();
    }
    line.len()
}

/// Apply single-document LSP TextEdits back onto `src` so tests can assert on
/// the resulting document (mirrors what an editor would do with the ranges).
fn audit2_apply_lsp_edits(src: &str, edits: &[serde_json::Value]) -> String {
    let mut sorted: Vec<(usize, usize, usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            let r = &e["range"];
            (
                r["start"]["line"].as_u64().unwrap() as usize,
                r["start"]["character"].as_u64().unwrap() as usize,
                r["end"]["line"].as_u64().unwrap() as usize,
                r["end"]["character"].as_u64().unwrap() as usize,
                e["newText"].as_str().unwrap(),
            )
        })
        .collect();
    // Apply from the end of the document so earlier offsets stay valid.
    sorted.sort_by(|a, b| b.cmp(a));
    let mut lines: Vec<String> = src.lines().map(String::from).collect();
    for (sl, sc, el, ec, new_text) in sorted {
        let total = lines.len();
        // Full-document replacement (whole-doc formatting): the edit spans
        // from the start of the document to the end (including the line past
        // a trailing newline). The result is exactly newText — splicing it
        // line-wise would keep the original tail (observed "formatted +
        // original" garbage in the test).
        if sl == 0 && sc == 0 && el >= total.saturating_sub(1) {
            let mut out = new_text.to_string();
            if src.ends_with('\n') && !out.ends_with('\n') {
                out.push('\n');
            }
            return out;
        }
        // Same-line window replacement.
        let l = &mut lines[sl];
        let sb = audit2_utf16_to_byte(l, sc);
        let eb = audit2_utf16_to_byte(l, ec);
        l.replace_range(sb..eb, new_text);
    }
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Drive the protocol far enough to serve document requests:
/// initialize → initialized → didOpen.
fn audit2_lsp_open(uri: &str, text: &str) -> crate::lsp::LspServer {
    let server = crate::lsp::LspServer::new();
    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "rootUri": null, "capabilities": {} }
    });
    let (server, resp) = crate::lsp::flow::transition(server, &init);
    assert!(resp.is_some(), "initialize must respond");
    let initialized = serde_json::json!({
        "jsonrpc": "2.0", "method": "initialized", "params": {}
    });
    let (server, _) = crate::lsp::flow::transition(server, &initialized);
    let open = serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": uri, "languageId": "mimi", "version": 1, "text": text } }
    });
    let (server, _) = crate::lsp::flow::transition(server, &open);
    server
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

// ============================================================
// Wave-2 audit fixes (devdocs/full-audit-2026-08-05-0656.md §2.9/§3.10)
// ============================================================

// --- X-6 (MED): parameter-hint slice panics on multi-line calls ---

#[test]
fn audit2_tool_inlay_param_hints_no_panic_on_multiline_call() {
    // PoC: the hint scan picks the FIRST line containing the callee name and
    // '(' — here main's multi-line call `other(ack(1, 2),\n 3)`. On that line
    // the inner call's ')' sits LEFT of the last-argument start, so
    // rfind(')') < arg_start_byte and the old `line_content[arg_start_byte..
    // end_pos]` sliced an inverted range → panic (byte index out of bounds).
    let src = "func main() -> i64 {\n    other(ack(1, 2),\n        3)\n    0\n}\nfunc ack(m: i64, n: i64) -> i64 {\n    ack(ack(m - 1, 1), n)\n}\nfunc other(a: i64, b: i64) -> i64 { a + b }\n";
    let server = crate::lsp::LspServer::new();
    let hints = server.compute_inlay_hints(src); // pre-fix: panics
                                                 // The mechanism genuinely reached the guarded code path: the first
                                                 // argument `ack(1, 2)` is non-trivial and carries the `m:` label.
    assert!(
        hints.iter().any(|h| h["label"] == "m:"),
        "first-arg param hint must be emitted (proves the guarded path was reached): {:?}",
        hints
    );
    // No hint may point at the unextractable tail argument position.
    assert!(
        hints
            .iter()
            .filter(|h| h["label"] == "n:")
            .all(|h| h["position"]["line"].as_u64() != Some(1)),
        "line 1 has no extractable `n:` argument text: {:?}",
        hints
    );
}

// --- X-8 (LOW): prepareRename byte offsets and formatting range ---

#[test]
fn audit2_tool_prepare_rename_range_is_utf16_not_bytes() {
    // PoC: `msg` sits after CJK comment content on its line.
    // Char/UTF-16 column of `msg` = 17; BYTE offset = 21 (你/好 = 3 bytes
    // each but 1 UTF-16 unit each). The old code emitted the byte offset.
    let src = "func main() -> i32 {\n    /* 你好 */ let msg = 1\n    msg\n    0\n}\n";
    let server = audit2_lsp_open("file:///prep.mimi", src);
    let req = serde_json::json!({
        "jsonrpc": "2.0", "id": 7, "method": "textDocument/prepareRename",
        "params": {
            "textDocument": { "uri": "file:///prep.mimi" },
            "position": { "line": 1, "character": 18 }
        }
    });
    let (_, resp) = crate::lsp::flow::transition(server, &req);
    let resp = resp.expect("prepareRename must respond");
    let result = &resp["result"];
    assert_eq!(result["start"]["line"], 1);
    assert_eq!(
        result["start"]["character"], 17,
        "UTF-16 character expected, got byte offset? {result}"
    );
    assert_eq!(result["end"]["character"], 20, "end of `msg`: {result}");
}

#[test]
fn audit2_tool_formatting_range_covers_trailing_newline_and_utf16() {
    // PoC (two defects): document ends with '\n' → lines() drops the final
    // empty line, so the old end position stopped before the trailing newline
    // (applying the edit duplicated it); and the last-line length was BYTES.
    let src = "func main() -> i64 {\nlet x=1\nlet s=\"你好  世界\"\nx+len(s)\n}\n";
    let server = audit2_lsp_open("file:///fmt1.mimi", src);
    let req = serde_json::json!({
        "jsonrpc": "2.0", "id": 8, "method": "textDocument/formatting",
        "params": { "textDocument": { "uri": "file:///fmt1.mimi" }, "options": { "tabSize": 4, "insertSpaces": true } }
    });
    let (_, resp) = crate::lsp::flow::transition(server, &req);
    let resp = resp.expect("formatting must respond");
    let edit = &resp["result"][0];
    // newText is the Formatter output (H-29: string spacing preserved).
    let formatted = crate::fmt::Formatter::new().format(src);
    assert_eq!(
        edit["newText"], formatted,
        "newText must be formatter output"
    );
    assert!(
        formatted.contains("\"你好  世界\""),
        "formatter corrupted the CJK/multi-space literal:\n{}",
        formatted
    );
    // Trailing '\n' → the range must end at (line_count, 0), covering it.
    assert_eq!(edit["range"]["start"]["line"], 0);
    assert_eq!(edit["range"]["start"]["character"], 0);
    assert_eq!(
        edit["range"]["end"]["line"],
        src.lines().count() as u64,
        "end line must point past the trailing newline: {edit}"
    );
    assert_eq!(edit["range"]["end"]["character"], 0);
    // Applying the single edit must reproduce the formatted text exactly —
    // no leftover trailing newline, no duplicated blank line.
    let applied = audit2_apply_lsp_edits(src, std::slice::from_ref(edit));
    assert_eq!(applied, formatted, "whole-doc replacement must be exact");
}

#[test]
fn audit2_tool_formatting_range_end_character_is_utf16() {
    // No trailing newline + CJK on the last line: end character must be the
    // UTF-16 length (7), not the byte length (15).
    let src = "func main() -> i32 {\n    0\n}\n// 你好注释";
    let server = audit2_lsp_open("file:///fmt2.mimi", src);
    let req = serde_json::json!({
        "jsonrpc": "2.0", "id": 9, "method": "textDocument/formatting",
        "params": { "textDocument": { "uri": "file:///fmt2.mimi" }, "options": {} }
    });
    let (_, resp) = crate::lsp::flow::transition(server, &req);
    let resp = resp.expect("formatting must respond");
    let edit = &resp["result"][0];
    let last_line = src.lines().last().unwrap();
    let utf16_len: usize = last_line.chars().map(|c| c.len_utf16()).sum();
    let byte_len = last_line.len();
    assert!(utf16_len != byte_len, "fixture must discriminate");
    assert_eq!(
        edit["range"]["end"]["line"],
        (src.lines().count() - 1) as u64
    );
    assert_eq!(
        edit["range"]["end"]["character"], utf16_len as u64,
        "end character must be UTF-16 units, not bytes: {edit}"
    );
}

// --- X-7 (LOW): goto-definition 1-based char columns as LSP character ---

#[test]
fn audit2_tool_definition_func_range_is_zero_based_utf16() {
    // PoC: a CJK block comment before `func` on the SAME line separates char
    // columns from byte offsets; the old code emitted start_col verbatim
    // (1-based char column) — off by one AND drifted.
    let src = "/* 你好 */ func target() -> i32 { 0 }\nfunc main() -> i32 { target() }\n";
    let server = crate::lsp::LspServer::new();
    // Cursor inside the `target` call on line 1.
    let result = server
        .compute_definition(src, 1, 22, "file:///def.mimi")
        .expect("definition of `target`");
    let range = &result["range"];
    assert_eq!(range["start"]["line"], 0);
    // `func` starts at 0-based UTF-16 character 9 ("/* 你好 */ " = 9 units);
    // old code emitted start_col = 10.
    assert_eq!(range["start"]["character"], 9, "0-based UTF-16: {result}");
    // Range spans `func ` + `target` = 11 units → end character 20.
    assert_eq!(range["end"]["character"], 20, "0-based UTF-16: {result}");
}

#[test]
fn audit2_tool_definition_param_range_is_zero_based() {
    // Params arm of the same fix: first param of a column-1 func must start
    // at character 0, not at start_col (= 1).
    let src = "func id(n: i64) -> i64 { n }\nfunc main() -> i64 { id(5) }\n";
    let server = crate::lsp::LspServer::new();
    // Cursor on the use of `n` inside id's body (line 0, char 25).
    let result = server
        .compute_definition(src, 0, 25, "file:///defp.mimi")
        .expect("definition of param `n`");
    let range = &result["range"];
    assert_eq!(range["start"]["line"], 0);
    assert_eq!(range["start"]["character"], 0, "0-based column: {result}");
    assert_eq!(range["end"]["character"], 1, "single-char name: {result}");
}

// --- X-4 (MED): Z3 dynamic timeout formula ---

#[test]
fn audit2_tool_verification_timeout_scales_with_line_span() {
    // PoC (formula-level): the pre-fix formula was start_col - start_line —
    // column minus line — which saturating-subtracts to 0 for any function
    // declared below ~line 10, clamping nearly everything to the 200 ms
    // floor. The fix scales with the function's line span.
    use crate::lsp::state::verification_timeout_ms;
    assert_eq!(verification_timeout_ms(1, 0), 200, "floor clamp");
    assert_eq!(verification_timeout_ms(10, 2), 700, "10 lines, 2 params");
    assert_eq!(verification_timeout_ms(40, 1), 2100, "linear in lines");
    assert_eq!(verification_timeout_ms(200, 0), 5000, "ceiling clamp");
    // The discriminating shape: a 100-line function (start_col ~1) used to
    // compute ~(1 - 100).max(1) = 1 → 200 ms; now it gets the ceiling.
    assert_eq!(verification_timeout_ms(100, 1), 5000);
}

// --- X-10 (LOW): folding string-escape desync / cross-line flag / block comments ---

#[test]
fn audit2_tool_folding_survives_escaped_backslash_string() {
    // PoC: `"a\\"` ends the string (escaped backslash), but the old
    // chars[i-1] != '\\' check left in_string stuck across lines; the `}{`
    // literal on the next line then produced phantom/missing folds.
    let server = crate::lsp::LspServer::new();
    let src = "func f() -> i32 {\n    let s = \"a\\\\\"\n    let t = \"}{\"\n    0\n}\n";
    let ranges = server.compute_folding_ranges(src);
    assert_eq!(
        ranges.len(),
        1,
        "exactly the function body must fold (pre-fix: zero ranges — the desync swallowed the closing brace): {:?}",
        ranges
    );
    assert_eq!(ranges[0]["startLine"], 0);
    assert_eq!(ranges[0]["endLine"], 4);
}

#[test]
fn audit2_tool_folding_ignores_block_comment_braces() {
    // Pre-fix, block comments were not tracked at all, so the brace pair
    // inside `/* { ... } */` created a phantom fold.
    let server = crate::lsp::LspServer::new();
    let src = "func g() -> i32 {\n    /* {\n    } */\n    0\n}\n";
    let ranges = server.compute_folding_ranges(src);
    assert_eq!(
        ranges.len(),
        1,
        "no phantom fold from the comment: {:?}",
        ranges
    );
    assert_eq!(ranges[0]["startLine"], 0);
    assert_eq!(ranges[0]["endLine"], 4);
    // Line comments with braces stay ignored as before.
    let src = "func h() -> i32 {\n    // }\n    0\n}\n";
    let ranges = server.compute_folding_ranges(src);
    assert_eq!(ranges.len(), 1, "{:?}", ranges);
}

// --- X-11 (LOW): no-text span fallback must align with AU-LSP-3 on ASCII ---

#[test]
fn audit2_tool_span_to_range_fallback_aligns_with_text_path() {
    // Without document text the char→UTF-16 walk is impossible; the fallback
    // must at least agree with PositionMap::span_to_lsp exactly on ASCII
    // source (where char columns == UTF-16 units) and be 0-based.
    let span = crate::span::Span::new(2, 5, 2, 9);
    let fallback = crate::lsp::position::span_to_range(&span);
    let text = "let x = 1\nlet yyyy = 2\nlet z = 3\n";
    let via_text = crate::lsp::position_map::PositionMap::new(text).span_to_lsp(2, 5, 2, 9);
    assert_eq!(
        fallback, via_text,
        "fallback must match the text-based path on ASCII"
    );
    assert_eq!(fallback["start"]["line"], 1);
    assert_eq!(fallback["start"]["character"], 4);
    assert_eq!(fallback["end"]["character"], 8);
}
