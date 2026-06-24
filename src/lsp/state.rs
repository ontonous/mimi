use serde_json::Value;

use crate::{core, lexer, parser};
use crate::ast::Stmt;
use crate::lsp::diagnostic;
use crate::lsp::LspServer;
use crate::lsp::util::{find_enclosing_func_in_items, hash_func_body};
use crate::verifier::{VerifStatus, Verifier};

impl LspServer {
    pub(crate) fn cache_put(&mut self, uri: String, text: String) {
        if self.documents.contains_key(&uri) {
            self.access_order.retain(|k| *k != uri);
        } else if self.documents.len() >= super::MAX_DOCUMENTS {
            if let Some(lru) = self.access_order.pop_front() {
                self.documents.remove(&lru);
            }
        }
        self.access_order.push_back(uri.clone());
        self.documents.insert(uri, text);
    }

    /// Insert into verification cache with LRU eviction.
    pub(crate) fn cache_put_verification(
        &mut self,
        key: String,
        value: (u64, VerifStatus, String),
    ) {
        self.cache_access_order.retain(|k| *k != key);
        self.cache_access_order.push_back(key.clone());
        while self.cache_access_order.len() > crate::lsp::MAX_VERIFICATION_CACHE {
            if let Some(lru) = self.cache_access_order.pop_front() {
                self.verification_cache.remove(&lru);
            } else {
                break;
            }
        }
        self.verification_cache.insert(key, value);
    }

    pub(crate) fn cache_remove(&mut self, uri: &str) {
        self.access_order.retain(|k| k != uri);
        self.documents.remove(uri);
    }

    /// Parse text with error recovery, returning partial AST even on errors
    pub(crate) fn parse_with_recovery(&self, text: &str) -> Option<crate::ast::File> {
        let tokens = match lexer::Lexer::new(text).tokenize() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[mimi lsp] lex error: {}", e);
                return None;
            }
        };
        let (file, _errors) = parser::Parser::new(tokens).parse_file_with_recovery();
        Some(file)
    }

    pub fn compute_diagnostics(&self, text: &str) -> Vec<Value> {
        let mut diagnostics = Vec::new();

        // Parse
        let tokens = match lexer::Lexer::new(text).tokenize() {
            Ok(t) => t,
            Err(e) => {
                diagnostics.push(diagnostic::simple_error_diagnostic(&e.to_string()));
                return diagnostics;
            }
        };

        // Use recovery parser to get partial AST + all parse errors
        let (file, parse_errors) = parser::Parser::new(tokens).parse_file_with_recovery();

        // Report all parse errors
        for e in &parse_errors {
            diagnostics.push(diagnostic::parse_error_to_lsp(e));
        }

        // Type check the partial AST (even if parse had errors)
        if let Err(errs) = core::check(&file) {
            for err in &errs {
                diagnostics.push(diagnostic::diagnostic_to_lsp(err));
            }
        }

        diagnostics
    }

    /// Compute Z3 verification diagnostics for the function at the given cursor line.
    /// Returns verification errors/warnings as LSP diagnostics.
    /// Uses caching: if the function body hasn't changed, skips re-verification.
    /// Returns empty vec on timeout, parser failure, or when no function is at cursor.
    /// `uri` is included in the cache key to avoid collisions between identically
    /// named functions in different files (fixes P1.4).
    pub fn compute_verification_diagnostics(
        &mut self,
        text: &str,
        cursor_line: usize,
        uri: &str,
    ) -> Vec<Value> {
        let mut diagnostics = Vec::new();

        if cursor_line == 0 {
            return diagnostics;
        }

        // Parse
        let tokens = match lexer::Lexer::new(text).tokenize() {
            Ok(t) => t,
            Err(_) => return diagnostics,
        };
        let (file, _errors) = parser::Parser::new(tokens).parse_file_with_recovery();

        // Find the enclosing function at cursor line
        let func = match find_enclosing_func_in_items(&file.items, text, cursor_line) {
            Some(f) => f,
            None => return diagnostics,
        };

        // Only verify if function has contracts
        let has_contracts = func
            .body
            .iter()
            .any(|s| matches!(s, Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Invariant(_, _) | Stmt::MmsBlock { .. }));
        if !has_contracts {
            return diagnostics;
        }

        // Compute body hash for caching
        let body_hash = hash_func_body(text, func);
        // Include URI in cache key to prevent collisions between identically
        // named functions in different files.
        let cache_key = format!("{}:{}", uri, func.name);

        // Check cache
        if let Some((cached_hash, ref status, ref msg)) = self.verification_cache.get(&cache_key) {
            if *cached_hash == body_hash {
                if matches!(status, VerifStatus::Failed) {
                    diagnostics.push(serde_json::json!({
                        "range": {
                            "start": { "line": func.pos.0.saturating_sub(1), "character": 0 },
                            "end": { "line": func.pos.0.saturating_sub(1), "character": 100 }
                        },
                        "severity": 1,
                        "source": "mimi-verify",
                        "message": msg
                    }));
                }
                return diagnostics;
            }
        }

        // Dynamic timeout based on function complexity
        let func_body_lines = func.pos.1.saturating_sub(func.pos.0).max(1);
        let param_count = func.params.len();
        let dynamic_timeout = (func_body_lines * 50 + param_count * 100)
            .clamp(200, 5000) as u64;

        // Lazily initialize the Z3 verifier with dynamic timeout
        let verifier = self.verifier.get_or_insert(match Verifier::with_timeout(dynamic_timeout) {
            Ok(v) => v,
            Err(_) => return diagnostics, // Z3 not available
        });
        // Update timeout for this invocation (reuses existing verifier)
        verifier.set_timeout(dynamic_timeout);

        // Run verification
        let results = verifier.verify_file(&file);
        for result in &results {
            if result.func_name != func.name {
                continue;
            }
            // Update cache (with LRU eviction)
            self.cache_put_verification(
                cache_key.clone(),
                (body_hash, result.status.clone(), result.message.clone()),
            );

            if matches!(result.status, VerifStatus::Failed) {
                diagnostics.push(serde_json::json!({
                    "range": {
                        "start": { "line": func.pos.0.saturating_sub(1), "character": 0 },
                        "end": { "line": func.pos.0.saturating_sub(1), "character": 100 }
                    },
                    "severity": 1,
                    "source": "mimi-verify",
                    "message": result.message
                }));
            }
        }

        // Persist cache to disk
        self.save_cache();

        diagnostics
    }
}
