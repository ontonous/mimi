use std::io::Write;
use std::path::PathBuf;

use serde_json::Value;

use crate::fmt;
use crate::lsp::LspServer;
use crate::lsp::util::percent_decode;

pub(crate) fn handle_message(
    server: &mut LspServer,
    msg: &Value,
) -> Option<Value> {
    let method = msg.get("method")?.as_str()?;
    let id = msg.get("id");

    match method {
        "initialize" => {
            // Store workspace root if provided
            server.workspace_root = msg
                .get("params")
                .and_then(|p| p.get("rootUri"))
                .and_then(|u| u.as_str())
                .and_then(|u| u.strip_prefix("file://"))
                .map(|p| PathBuf::from(percent_decode(p)));
            // Fall back to rootPath
            if server.workspace_root.is_none() {
                server.workspace_root = msg
                    .get("params")
                    .and_then(|p| p.get("rootPath"))
                    .and_then(|p| p.as_str())
                    .map(PathBuf::from);
            }
            // Load persistent verification cache after workspace root is set
            server.load_cache();
            let result = serde_json::json!({
                "capabilities": {
                    "textDocumentSync": {
                        "openClose": true,
                        "change": 1,
                        "save": {
                            "includeText": false
                        }
                    },
                    "completionProvider": {
                        "triggerCharacters": [".", ":"]
                    },
                    "hoverProvider": true,
                    "definitionProvider": true,
                    "implementationProvider": true,
                    "referencesProvider": true,
                    "renameProvider": {
                        "prepareProvider": true
                    },
                    "signatureHelpProvider": {
                        "triggerCharacters": ["("]
                    },
                    "semanticTokensProvider": {
                        "legend": {
                            "tokenTypes": ["keyword", "function", "type", "variable", "number", "string", "comment", "operator"],
                            "tokenModifiers": ["declaration", "definition"]
                        },
                        "full": true
                    },
                    "codeActionProvider": true,
                    "workspaceSymbolProvider": true,
                    "codeLensProvider": {
                        "resolveProvider": false
                    },
                    "foldingRangeProvider": true,
                    "documentFormattingProvider": true,
                    "documentHighlightProvider": true,
                    "inlayHintProvider": true,
                    "callHierarchyProvider": true
                }
            });
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }))
        }
        "initialized" => None,
        "textDocument/didOpen" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let text = msg
                .get("params")?
                .get("textDocument")?
                .get("text")?
                .as_str()?;
            server.cache_put(uri.to_string(), text.to_string());
            // Publish diagnostics
            let diagnostics = server.compute_diagnostics(text);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": {
                    "uri": uri,
                    "diagnostics": diagnostics
                }
            }))
        }
        "textDocument/didChange" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let text = msg
                .get("params")?
                .get("contentChanges")?
                .as_array()?
                .first()?
                .get("text")?
                .as_str()?;
            server.cache_put(uri.to_string(), text.to_string());
            let mut diagnostics = server.compute_diagnostics(text);
            let verif_diags = server.compute_verification_diagnostics(text, server.last_cursor_line, uri);
            diagnostics.extend(verif_diags);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": {
                    "uri": uri,
                    "diagnostics": diagnostics
                }
            }))
        }
        "textDocument/didClose" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            server.cache_remove(uri);
            None
        }
        "textDocument/didSave" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            if let Some(text) = server.documents.get(uri) {
                let diagnostics = server.compute_diagnostics(text);
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {
                        "uri": uri,
                        "diagnostics": diagnostics
                    }
                }))
            } else {
                None
            }
        }
        "textDocument/completion" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let (line, character) = msg
                .get("params")
                .and_then(|p| p.get("position"))
                .map(|pos| {
                    (
                        pos.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as usize,
                        pos.get("character")
                            .and_then(|c| c.as_u64())
                            .unwrap_or(0) as usize,
                    )
                })
                .unwrap_or((0, 0));
            server.last_cursor_line = line;
            let text = server.documents.get(uri)?;
            let items = server.compute_completion(text, line, character);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "isIncomplete": false,
                    "items": items
                }
            }))
        }
        "textDocument/hover" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let position = msg.get("params")?.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            server.last_cursor_line = line;
            let text = server.documents.get(uri)?;
            let hover = server.compute_hover(text, line, character);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": hover
            }))
        }
        "textDocument/definition" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let position = msg.get("params")?.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            server.last_cursor_line = line;
            let text = server.documents.get(uri)?;
            let definition = server.compute_definition(text, line, character, uri);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": definition
            }))
        }
        "textDocument/implementation" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let position = msg.get("params")?.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            server.last_cursor_line = line;
            let text = server.documents.get(uri)?;
            let impls = server.compute_go_to_implementation(text, line, character, uri);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": impls
            }))
        }
        "textDocument/documentSymbol" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let text = server.documents.get(uri)?;
            let symbols = server.compute_document_symbols(text);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": symbols
            }))
        }
        "textDocument/references" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let position = msg.get("params")?.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            server.last_cursor_line = line;
            let text = server.documents.get(uri)?;
            let include_decl = msg
                .get("params")?
                .get("context")?
                .get("includeDeclaration")?
                .as_bool()
                .unwrap_or(true);
            let references = server.compute_references(text, line, character, uri, include_decl);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": references
            }))
        }
        "textDocument/prepareRename" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let position = msg.get("params")?.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            server.last_cursor_line = line;
            let text = server.documents.get(uri)?;
            let word = server.get_word_at(text, line, character);
            if word.is_empty() {
                return None;
            }
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "start": { "line": line, "character": server.word_start_col(text, line, character) },
                    "end": { "line": line, "character": character + server.word_end_offset(text, line, character) }
                }
            }))
        }
        "textDocument/rename" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let position = msg.get("params")?.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            server.last_cursor_line = line;
            let new_name = msg.get("params")?.get("newName")?.as_str()?;
            let text = server.documents.get(uri)?;
            let workspace_edit = server.compute_rename(text, line, character, uri, new_name);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": workspace_edit
            }))
        }
        "textDocument/signatureHelp" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let position = msg.get("params")?.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            server.last_cursor_line = line;
            let text = server.documents.get(uri)?;
            let sig_help = server.compute_signature_help(text, line, character);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": sig_help
            }))
        }
        "textDocument/semanticTokens/full" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let text = server.documents.get(uri)?;
            let tokens = server.compute_semantic_tokens(text);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "data": tokens
                }
            }))
        }
        "textDocument/foldingRange" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let text = server.documents.get(uri)?;
            let ranges = server.compute_folding_ranges(text);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": ranges
            }))
        }
        "textDocument/formatting" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let text = server.documents.get(uri)?;
            let formatted = fmt::Formatter::new().format(text);
            let line_count = text.lines().count();
            let last_line_len = text.lines().last().map(|l| l.len()).unwrap_or(0);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": [{
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": (line_count - 1).max(0), "character": last_line_len }
                    },
                    "newText": formatted
                }]
            }))
        }
        "textDocument/documentHighlight" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let position = msg.get("params")?.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            let text = server.documents.get(uri)?;
            let highlights = server.compute_document_highlight(text, line, character);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": highlights
            }))
        }
        "textDocument/inlayHint" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let text = server.documents.get(uri)?;
            let hints = server.compute_inlay_hints(text);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": hints
            }))
        }
        "textDocument/codeAction" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let context = msg.get("params")?.get("context")?;
            let actions = server.compute_code_actions(uri, context);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": actions
            }))
        }
        "workspace/symbol" => {
            let query = msg
                .get("params")
                .and_then(|p| p.get("query"))
                .and_then(|q| q.as_str())
                .unwrap_or("");
            let symbols = server.compute_workspace_symbols(query);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": symbols
            }))
        }
        "textDocument/codeLens" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let text = server.documents.get(uri)?;
            let lenses = server.compute_code_lens(text, uri);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": lenses
            }))
        }
        "textDocument/prepareCallHierarchy" => {
            let uri = msg
                .get("params")?
                .get("textDocument")?
                .get("uri")?
                .as_str()?;
            let position = msg.get("params")?.get("position")?;
            let line = position.get("line")?.as_u64()? as usize;
            let character = position.get("character")?.as_u64()? as usize;
            let text = server.documents.get(uri)?;
            let items = server.compute_prepare_call_hierarchy(text, uri, line, character);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": items
            }))
        }
        "callHierarchy/incomingCalls" => {
            let uri = msg.get("params")?.get("item")?.get("uri")?.as_str()?;
            let name = msg.get("params")?.get("item")?.get("name")?.as_str()?;
            let text = server.documents.get(uri)?;
            let calls = server.compute_incoming_calls(text, uri, name);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": calls
            }))
        }
        "callHierarchy/outgoingCalls" => {
            let uri = msg.get("params")?.get("item")?.get("uri")?.as_str()?;
            let name = msg.get("params")?.get("item")?.get("name")?.as_str()?;
            let text = server.documents.get(uri)?;
            let calls = server.compute_outgoing_calls(text, uri, name);
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": calls
            }))
        }
        "shutdown" => {
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": null
            }))
        }
        "exit" => {
            // LSP spec: exit notification means server should terminate.
            // Flush buffers before exiting to ensure log output is written.
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(0)
        }
        _ => None,
    }
}
