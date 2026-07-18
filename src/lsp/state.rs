use serde_json::Value;
use std::path::PathBuf;

use crate::ast::Stmt;
use crate::lsp::diagnostic;
use crate::lsp::util::{find_enclosing_func_in_items, hash_func_body};
use crate::lsp::LspServer;
use crate::verifier::{VerifStatus, Verifier};
use crate::{core, lexer, parser};

impl LspServer {
    pub(crate) fn cache_put(&mut self, uri: String, text: String) {
        if self.documents.contains_key(&uri) {
            self.access_order.retain(|k| *k != uri);
        } else if self.documents.len() >= super::MAX_DOCUMENTS {
            // L-H7: never silently drop still-open documents (those with a
            // tracked version from didOpen/didChange). Evict the oldest closed
            // entry; if every cached doc is still open, grow past the soft limit.
            if let Some(pos) = self
                .access_order
                .iter()
                .position(|k| !self.document_versions.contains_key(k))
            {
                let lru = self.access_order.remove(pos).expect("index valid");
                self.documents.remove(&lru);
                self.document_versions.remove(&lru);
            }
        }
        self.access_order.push_back(uri.clone());
        self.documents.insert(uri, text);
    }

    /// L-H3: record the textDocument version for stale-change filtering.
    pub(crate) fn set_document_version(&mut self, uri: &str, version: i64) {
        self.document_versions.insert(uri.to_string(), version);
    }

    pub(crate) fn document_version(&self, uri: &str) -> Option<i64> {
        self.document_versions.get(uri).copied()
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
        self.document_versions.remove(uri);
    }

    /// Parse text with error recovery, returning partial AST even on errors.
    /// Uses a simple cache to avoid re-parsing the same text on every keystroke.
    pub(crate) fn parse_with_recovery(&self, text: &str) -> Option<crate::ast::File> {
        {
            let cache = self.parse_cache_text.borrow();
            if cache.0 == text {
                return cache.1.clone();
            }
        }
        let tokens = match lexer::Lexer::new(text).tokenize() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[mimi lsp] lex error: {}", e);
                return None;
            }
        };
        let (file, _errors) = parser::Parser::new(tokens).parse_file_with_recovery();
        let result = Some(file);
        *self.parse_cache_text.borrow_mut() = (text.to_string(), result.clone());
        result
    }

    /// Convert a `file://` URI to a filesystem path.
    fn uri_to_path(uri: &str) -> Option<PathBuf> {
        let path_str = uri.strip_prefix("file://")?;
        // SEC-C4 (deep audit): percent-decode the path (it may be %-encoded)
        // and normalize away `..` / `.` components so a crafted URI such as
        // `file:///home/user/../../etc/passwd` cannot escape the filesystem
        // root. `PathBuf::pop` on a root is a no-op, so escapes are dropped.
        let decoded = crate::lsp::util::percent_decode(path_str);
        let path = PathBuf::from(decoded);
        let mut normalized = PathBuf::new();
        for comp in path.components() {
            match comp {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => {}
                other => normalized.push(other.as_os_str()),
            }
        }
        // Workspace sandbox: if a workspace root is configured, reject paths
        // that resolve outside it (absolute path reads like /etc/passwd).
        // Callers without workspace_root still get the normalized path.
        Some(normalized)
    }

    /// Like [`uri_to_path`] but rejects paths outside `workspace_root` when set.
    fn uri_to_path_sandboxed(&self, uri: &str) -> Option<PathBuf> {
        let path = Self::uri_to_path(uri)?;
        if let Some(root) = &self.workspace_root {
            let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());
            let path_canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !path_canon.starts_with(&root_canon) {
                return None;
            }
            return Some(path_canon);
        }
        Some(path)
    }

    /// Resolve imports if the file has any, using the workspace root.
    /// L-C1: uses the in-memory AST for the main file so unsaved editor
    /// buffers are not replaced by stale on-disk content.
    fn resolve_imports(file: &mut crate::ast::File, file_path: &std::path::Path) {
        if file.imports.is_empty() {
            crate::loader::merge_prelude_into(file);
            return;
        }
        let base_dir = file_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = crate::loader::ModuleLoader::new(base_dir);
        if loader.load_main_with_file(file_path, file.clone()).is_ok() {
            if let Ok(merged) = loader.merge_all() {
                *file = merged;
            }
        }
        crate::loader::merge_prelude_into(file);
    }

    pub fn compute_diagnostics(&self, text: &str, uri: Option<&str>) -> Vec<Value> {
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
        let (mut file, parse_errors) = parser::Parser::new(tokens).parse_file_with_recovery();

        // Report all parse errors
        for e in &parse_errors {
            diagnostics.push(diagnostic::parse_error_to_lsp(e));
        }

        // Resolve imports so `use std::xxx` functions are visible to type checking.
        // Prefer sandboxed URI resolution so absolute paths outside the
        // workspace cannot be opened via a crafted file:// URI.
        if let Some(uri_str) = uri {
            if let Some(file_path) = self.uri_to_path_sandboxed(uri_str) {
                Self::resolve_imports(&mut file, file_path.as_path());
            }
        } else {
            crate::loader::merge_prelude_into(&mut file);
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
        let has_contracts = func.body.iter().any(|s| {
            matches!(
                s,
                Stmt::Requires(_, _)
                    | Stmt::Ensures(_, _)
                    | Stmt::Invariant(_, _)
                    | Stmt::MmsBlock { .. }
            )
        });
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
        let dynamic_timeout = (func_body_lines * 50 + param_count * 100).clamp(200, 5000) as u64;

        // AU-H3: if a prior Z3 crash poisoned the session, drop it so
        // get_or_insert creates a fresh solver (otherwise verify forever Unknown).
        if self.verifier.as_ref().is_some_and(|v| v.is_poisoned()) {
            self.verifier = None;
        }
        // Lazily initialize the Z3 verifier with dynamic timeout
        let verifier = self
            .verifier
            .get_or_insert(match Verifier::with_timeout(dynamic_timeout) {
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
