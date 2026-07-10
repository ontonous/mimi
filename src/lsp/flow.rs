#![allow(unused_mut)]

//! Flow-based LSP message handler — v0.29.5 prototype.
//!
//! Replaces `handle_message(&mut self, msg)` with
//! `transition(server: LspServer, msg: &Value) -> (LspServer, Option<Value>)`.
//!
//! This is an outer "strict Flow" shell: `transition` takes ownership of
//! `LspServer` and returns it.  Internal helpers still use `&mut self` on
//! the local `server` (which is owned, so `&mut self` methods are callable).

use std::path::PathBuf;

use serde_json::Value;

use crate::fmt;
use crate::lsp::util::percent_decode;
use crate::lsp::LspServer;

/// Extract the `method` string from a JSON-RPC message.
fn get_method(msg: &Value) -> Option<&str> {
    msg.get("method").and_then(|v| v.as_str())
}

/// Extract the textDocument URI from params.
fn get_uri(msg: &Value) -> Option<&str> {
    msg.get("params")
        .and_then(|p| p.get("textDocument"))
        .and_then(|td| td.get("uri"))
        .and_then(|u| u.as_str())
}

/// Extract a line/character position from params.
fn get_line_char(msg: &Value) -> Option<(usize, usize)> {
    let pos = msg.get("params").and_then(|p| p.get("position"))?;
    Some((
        pos.get("line").and_then(|l| l.as_u64())? as usize,
        pos.get("character").and_then(|c| c.as_u64())? as usize,
    ))
}

/// Transition the LSP server state in response to a JSON-RPC message.
///
/// Takes ownership of `server` and returns it alongside any response.
pub(crate) fn transition(mut server: LspServer, msg: &Value) -> (LspServer, Option<Value>) {
    let method = match get_method(msg) {
        Some(m) => m,
        None => return (server, None),
    };
    let id = msg.get("id");

    match method {
        "initialize" => initialize(server, msg, id),
        "initialized" => (server, None),
        "textDocument/didOpen" => did_open(server, msg),
        "textDocument/didChange" => did_change(server, msg),
        "textDocument/didClose" => did_close(server, msg),
        "textDocument/didSave" => did_save(server, msg),
        "textDocument/completion" => completion(server, msg, id),
        "textDocument/hover" => hover(server, msg, id),
        "textDocument/definition" => definition(server, msg, id),
        "textDocument/implementation" => implementation(server, msg, id),
        "textDocument/documentSymbol" => document_symbol(server, msg, id),
        "textDocument/references" => references(server, msg, id),
        "textDocument/prepareRename" => prepare_rename(server, msg, id),
        "textDocument/rename" => rename(server, msg, id),
        "textDocument/signatureHelp" => signature_help(server, msg, id),
        "textDocument/semanticTokens/full" => semantic_tokens(server, msg, id),
        "textDocument/foldingRange" => folding_range(server, msg, id),
        "textDocument/formatting" => formatting(server, msg, id),
        "textDocument/documentHighlight" => document_highlight(server, msg, id),
        "textDocument/inlayHint" => inlay_hint(server, msg, id),
        "textDocument/codeAction" => code_action(server, msg, id),
        "workspace/symbol" => workspace_symbol(server, msg, id),
        "textDocument/codeLens" => code_lens(server, msg, id),
        "textDocument/prepareCallHierarchy" => prepare_call_hierarchy(server, msg, id),
        "callHierarchy/incomingCalls" => incoming_calls(server, msg, id),
        "callHierarchy/outgoingCalls" => outgoing_calls(server, msg, id),
        "shutdown" => (
            server,
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": null
            })),
        ),
        "exit" => {
            server.should_exit = true;
            (server, None)
        }
        _ => (server, None),
    }
}

// ── Handler functions ─────────────────────────────────────────────────

fn initialize(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    server.workspace_root = msg
        .get("params")
        .and_then(|p| p.get("rootUri"))
        .and_then(|u| u.as_str())
        .and_then(|u| u.strip_prefix("file://"))
        .map(|p| PathBuf::from(percent_decode(p)));
    if server.workspace_root.is_none() {
        server.workspace_root = msg
            .get("params")
            .and_then(|p| p.get("rootPath"))
            .and_then(|p| p.as_str())
            .map(PathBuf::from);
    }
    server.load_cache();
    let result = serde_json::json!({
        "capabilities": {
            "textDocumentSync": {
                "openClose": true,
                "change": 1,
                "save": { "includeText": true }
            },
            "completionProvider": { "triggerCharacters": [".", ":"] },
            "hoverProvider": true,
            "definitionProvider": true,
            "implementationProvider": true,
            "referencesProvider": true,
            "renameProvider": { "prepareProvider": true },
            "signatureHelpProvider": { "triggerCharacters": ["("] },
            "semanticTokensProvider": {
                "legend": {
                    "tokenTypes": ["keyword", "function", "type", "variable", "number", "string", "comment", "operator"],
                    "tokenModifiers": ["declaration", "definition"]
                },
                "full": true
            },
            "codeActionProvider": true,
            "workspaceSymbolProvider": true,
            "codeLensProvider": { "resolveProvider": false },
            "foldingRangeProvider": true,
            "documentFormattingProvider": true,
            "documentHighlightProvider": true,
            "inlayHintProvider": true,
            "callHierarchyProvider": true
        },
        "serverInfo": { "name": "mimi", "version": "0.27.0" }
    });
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        })),
    )
}

fn did_open(mut server: LspServer, msg: &Value) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let text = match msg
        .get("params")
        .and_then(|p| p.get("textDocument"))
        .and_then(|td| td.get("text"))
        .and_then(|t| t.as_str())
    {
        Some(t) => t,
        None => return (server, None),
    };
    server.cache_put(uri.to_string(), text.to_string());
    let diagnostics = server.compute_diagnostics(text, Some(uri));
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics }
        })),
    )
}

fn did_change(mut server: LspServer, msg: &Value) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let changes = match msg
        .get("params")
        .and_then(|p| p.get("contentChanges"))
        .and_then(|c| c.as_array())
    {
        Some(c) => c,
        None => return (server, None),
    };
    if changes.is_empty() {
        return (server, None);
    }
    let text = match changes
        .first()
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
    {
        Some(t) => t,
        None => return (server, None),
    };
    server.cache_put(uri.to_string(), text.to_string());
    *server.parse_cache_text.borrow_mut() = (String::new(), None);
    let mut diagnostics = server.compute_diagnostics(text, Some(uri));
    let verif_diags = server.compute_verification_diagnostics(text, server.last_cursor_line, uri);
    diagnostics.extend(verif_diags);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics }
        })),
    )
}

fn did_close(mut server: LspServer, msg: &Value) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    server.cache_remove(uri);
    (server, None)
}

fn did_save(mut server: LspServer, msg: &Value) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let text = msg
        .get("params")
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .or_else(|| server.documents.get(uri).map(|s| s.as_str()))
        .unwrap_or("");
    let diagnostics = server.compute_diagnostics(text, Some(uri));
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics }
        })),
    )
}

fn completion(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = msg
        .get("params")
        .and_then(|p| p.get("position"))
        .map(|pos| {
            (
                pos.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as usize,
                pos.get("character").and_then(|c| c.as_u64()).unwrap_or(0) as usize,
            )
        })
        .unwrap_or((0, 0));
    server.last_cursor_line = line;
    let text = match server.documents.get(uri) {
        Some(t) => t.clone(),
        None => return (server, None),
    };
    let items = server.compute_completion(&text, line, character);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "isIncomplete": false, "items": items }
        })),
    )
}

fn hover(mut server: LspServer, msg: &Value, id: Option<&Value>) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = match get_line_char(msg) {
        Some(lc) => lc,
        None => return (server, None),
    };
    server.last_cursor_line = line;
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let hover = server.compute_hover(text, line, character);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": hover
        })),
    )
}

fn definition(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = match get_line_char(msg) {
        Some(lc) => lc,
        None => return (server, None),
    };
    server.last_cursor_line = line;
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let def = server.compute_definition(text, line, character, uri);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": def
        })),
    )
}

fn implementation(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = match get_line_char(msg) {
        Some(lc) => lc,
        None => return (server, None),
    };
    server.last_cursor_line = line;
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let impls = server.compute_go_to_implementation(text, line, character, uri);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": impls
        })),
    )
}

fn document_symbol(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let symbols = server.compute_document_symbols(text);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": symbols
        })),
    )
}

fn references(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = match get_line_char(msg) {
        Some(lc) => lc,
        None => return (server, None),
    };
    server.last_cursor_line = line;
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let include_decl = msg
        .get("params")
        .and_then(|p| p.get("context"))
        .and_then(|c| c.get("includeDeclaration"))
        .and_then(|b| b.as_bool())
        .unwrap_or(true);
    let refs = server.compute_references(text, line, character, uri, include_decl);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": refs
        })),
    )
}

fn prepare_rename(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = match get_line_char(msg) {
        Some(lc) => lc,
        None => return (server, None),
    };
    server.last_cursor_line = line;
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let (word_start, word_end) = match server.get_word_range(text, line, character) {
        Some(r) => r,
        None => return (server, None),
    };
    if word_start >= word_end {
        return (server, None);
    }
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "start": { "line": line, "character": word_start },
                "end": { "line": line, "character": word_end }
            }
        })),
    )
}

fn rename(mut server: LspServer, msg: &Value, id: Option<&Value>) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = match get_line_char(msg) {
        Some(lc) => lc,
        None => return (server, None),
    };
    server.last_cursor_line = line;
    let new_name = match msg
        .get("params")
        .and_then(|p| p.get("newName"))
        .and_then(|n| n.as_str())
    {
        Some(n) => n,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let workspace_edit = server.compute_rename(text, line, character, uri, new_name);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": workspace_edit
        })),
    )
}

fn signature_help(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = match get_line_char(msg) {
        Some(lc) => lc,
        None => return (server, None),
    };
    server.last_cursor_line = line;
    let text = match server.documents.get(uri) {
        Some(t) => t.clone(),
        None => return (server, None),
    };
    let sig_help = server.compute_signature_help(&text, line, character);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": sig_help
        })),
    )
}

fn semantic_tokens(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let tokens = server.compute_semantic_tokens(text);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "data": tokens }
        })),
    )
}

fn folding_range(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let ranges = server.compute_folding_ranges(text);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": ranges
        })),
    )
}

fn formatting(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t.clone(),
        None => return (server, None),
    };
    let formatted = fmt::Formatter::new().format(&text);
    let line_count = text.lines().count();
    let last_line_len = text.lines().last().map(|l| l.len()).unwrap_or(0);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": [{
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": (line_count - 1), "character": last_line_len }
                },
                "newText": formatted
            }]
        })),
    )
}

fn document_highlight(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = match get_line_char(msg) {
        Some(lc) => lc,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let highlights = server.compute_document_highlight(text, line, character);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": highlights
        })),
    )
}

fn inlay_hint(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let hints = server.compute_inlay_hints(text);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": hints
        })),
    )
}

fn code_action(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let context = match msg.get("params").and_then(|p| p.get("context")) {
        Some(c) => c,
        None => return (server, None),
    };
    let actions = server.compute_code_actions(uri, context);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": actions
        })),
    )
}

fn workspace_symbol(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let query = msg
        .get("params")
        .and_then(|p| p.get("query"))
        .and_then(|q| q.as_str())
        .unwrap_or("");
    let symbols = server.compute_workspace_symbols(query);
    let is_incomplete = symbols.len() >= 1000;
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "isIncomplete": is_incomplete,
                "symbols": symbols
            }
        })),
    )
}

fn code_lens(mut server: LspServer, msg: &Value, id: Option<&Value>) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let lenses = server.compute_code_lens(text, uri);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": lenses
        })),
    )
}

fn prepare_call_hierarchy(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match get_uri(msg) {
        Some(u) => u,
        None => return (server, None),
    };
    let (line, character) = match get_line_char(msg) {
        Some(lc) => lc,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let items = server.compute_prepare_call_hierarchy(text, uri, line, character);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": items
        })),
    )
}

fn incoming_calls(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match msg
        .get("params")
        .and_then(|p| p.get("item"))
        .and_then(|i| i.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(u) => u,
        None => return (server, None),
    };
    let name = match msg
        .get("params")
        .and_then(|p| p.get("item"))
        .and_then(|i| i.get("name"))
        .and_then(|n| n.as_str())
    {
        Some(n) => n,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let calls = server.compute_incoming_calls(text, uri, name);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": calls
        })),
    )
}

fn outgoing_calls(
    mut server: LspServer,
    msg: &Value,
    id: Option<&Value>,
) -> (LspServer, Option<Value>) {
    let uri = match msg
        .get("params")
        .and_then(|p| p.get("item"))
        .and_then(|i| i.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(u) => u,
        None => return (server, None),
    };
    let name = match msg
        .get("params")
        .and_then(|p| p.get("item"))
        .and_then(|i| i.get("name"))
        .and_then(|n| n.as_str())
    {
        Some(n) => n,
        None => return (server, None),
    };
    let text = match server.documents.get(uri) {
        Some(t) => t,
        None => return (server, None),
    };
    let calls = server.compute_outgoing_calls(text, uri, name);
    (
        server,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": calls
        })),
    )
}
