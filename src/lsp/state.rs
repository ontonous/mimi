use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::ast::Stmt;
use crate::diagnostic::{Diagnostic, DiagnosticOrigin, DiagnosticOriginKind};
use crate::lsp::diagnostic;
use crate::lsp::util::{find_enclosing_func_in_items, hash_func_body};
use crate::lsp::{LspServer, VerificationCacheEntry};
use crate::span::{
    SourceContext, SourceId, SourceKey, SourceRecord, SourceRegistry, SourceTextOrigin, Span,
};
use crate::verifier::{VerifStatus, Verifier};
use crate::{core, lexer, parser};

#[derive(Debug, Clone)]
pub(crate) struct DiagnosticBatch {
    pub(crate) uri: Option<String>,
    pub(crate) diagnostics: Vec<Value>,
}

fn owned_diagnostic_origin(origin: &crate::core::Origin) -> DiagnosticOrigin {
    match origin {
        crate::core::Origin::User(_) => DiagnosticOrigin::user(),
        crate::core::Origin::Desugared { parent, rule, .. } => DiagnosticOrigin {
            kind: DiagnosticOriginKind::Desugared,
            rule: Some(rule.clone()),
            parent_node_id: Some(parent.0.clone()),
        },
        crate::core::Origin::PrototypeFallback { parent, rule, .. } => DiagnosticOrigin {
            kind: DiagnosticOriginKind::PrototypeFallback,
            rule: Some(rule.clone()),
            parent_node_id: Some(parent.0.clone()),
        },
        crate::core::Origin::RuntimeSystem { parent, rule, .. } => DiagnosticOrigin {
            kind: DiagnosticOriginKind::RuntimeSystem,
            rule: Some(rule.clone()),
            parent_node_id: Some(parent.0.clone()),
        },
    }
}

/// Resolve provenance only when the checked catalog has one unambiguous
/// semantic origin at this exact source span inside the selected function.
/// HashMap iteration order is never used as a tie-breaker: conflicting exact
/// candidates deliberately produce no origin.
fn verification_diagnostic_origin(
    program: &crate::core::CheckedProgram,
    function_name: &str,
    function_span: crate::span::Span,
    diagnostic_span: crate::span::Span,
) -> Option<DiagnosticOrigin> {
    let mut functions = program.functions().values().filter(|function| {
        function
            .qualified_name
            .rsplit("::")
            .next()
            .is_some_and(|name| name == function_name)
            && function.origin.user_span() == function_span
    });
    let function = functions.next()?;
    if functions.next().is_some() {
        return None;
    }

    let owner = &function.node_id.0;
    let child_prefix = format!("{owner}/");
    let mut candidates = program
        .node_meta()
        .values()
        .filter(|meta| {
            (meta.node_id.0 == *owner || meta.node_id.0.starts_with(&child_prefix))
                && meta.origin.user_span() == diagnostic_span
        })
        .map(|meta| owned_diagnostic_origin(&meta.origin))
        .collect::<Vec<_>>();
    if function.origin.user_span() == diagnostic_span {
        candidates.push(owned_diagnostic_origin(&function.origin));
    }
    let first = candidates.first()?.clone();
    candidates
        .iter()
        .all(|candidate| candidate == &first)
        .then_some(first)
}

impl LspServer {
    fn register_uri_source(&self, uri: &str) -> Result<(SourceId, SourceRegistry), String> {
        let disk_path = Self::uri_to_path(uri).map(|path| path.canonicalize().unwrap_or(path));
        let key = match disk_path.as_deref() {
            Some(path) => {
                let context = crate::loader::source_context_for_path(path)?;
                context
                    .registry()
                    .key(context.source_id())
                    .cloned()
                    .ok_or_else(|| "loader returned an unregistered URI source".to_string())?
            }
            None => SourceKey::external_uri(uri),
        };
        let mut record = SourceRecord::new(key, SourceTextOrigin::Memory).with_uri(uri);
        if let Some(path) = disk_path {
            record = record.with_disk_path(path);
        }
        let mut registry = self.source_registry.borrow_mut();
        let source_id = registry
            .register(record)
            .map_err(|error| error.to_string())?;
        Ok((source_id, registry.clone()))
    }

    fn register_memory_source(&self, text: &str) -> Result<(SourceId, SourceRegistry), String> {
        let context = SourceContext::memory("lsp", "anonymous-buffer", text)
            .map_err(|error| error.to_string())?;
        let record = context
            .registry()
            .record(context.source_id())
            .cloned()
            .ok_or_else(|| "memory source context is unregistered".to_string())?;
        let mut registry = self.source_registry.borrow_mut();
        let source_id = registry
            .register(record)
            .map_err(|error| error.to_string())?;
        Ok((source_id, registry.clone()))
    }

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
    pub(crate) fn cache_put_verification(&mut self, key: String, value: VerificationCacheEntry) {
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
        self.clear_parse_cache();
    }

    /// Parse text with error recovery, returning partial AST even on errors.
    /// Cache identity includes SourceKey: equal text in two URIs must never
    /// return an AST carrying the first document's SourceId.
    pub(crate) fn parse_with_recovery_for_uri(
        &self,
        text: &str,
        uri: Option<&str>,
    ) -> Option<crate::ast::File> {
        let (source_id, source_registry) = match uri {
            Some(uri) => self.register_uri_source(uri).ok()?,
            None => self.register_memory_source(text).ok()?,
        };
        let source_key = source_registry.key(source_id)?.as_str().to_string();
        let cache = self.parse_cache.borrow();
        if cache.source_key == source_key && cache.text == text {
            return cache.file.clone();
        }
        drop(cache);
        let tokens = match lexer::Lexer::new(text).tokenize() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[mimi lsp] lex error: {}", e);
                return None;
            }
        };
        let (file, _errors) =
            parser::Parser::new_with_source_registry(tokens, source_id, source_registry)
                .parse_file_with_recovery();
        let result = Some(file);
        *self.parse_cache.borrow_mut() = super::ParseCacheEntry {
            source_key,
            text: text.to_string(),
            file: result.clone(),
        };
        result
    }

    pub(crate) fn parse_with_recovery(&self, text: &str) -> Option<crate::ast::File> {
        let active_uri = self.active_document_uri.as_deref().filter(|uri| {
            self.documents
                .get(*uri)
                .is_some_and(|document| document == text)
        });
        let mut matches = self
            .documents
            .iter()
            .filter(|(_, document)| document.as_str() == text)
            .map(|(uri, _)| uri.as_str());
        let first = matches.next();
        let unique_match = first.filter(|_| matches.next().is_none());
        let matching_uri = active_uri.or(unique_match);
        self.parse_with_recovery_for_uri(text, matching_uri)
    }

    pub(crate) fn clear_parse_cache(&self) {
        *self.parse_cache.borrow_mut() = super::ParseCacheEntry::default();
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
    fn adopt_file_registry(&self, file: &mut crate::ast::File) -> Result<(), String> {
        let incoming = file.sources.clone();
        let mut session = self.source_registry.borrow_mut();
        let mut merged = session.clone();
        let remap = merged
            .merge_from(&incoming)
            .map_err(|error| format!("cannot merge LSP source registry: {error}"))?;
        for record in incoming.records() {
            let mapped = remap
                .remap(record.id)
                .map_err(|error| format!("cannot validate LSP source remap: {error}"))?;
            if mapped != record.id {
                return Err(format!(
                    "LSP source registry merge would renumber '{}' from {} to {}; refusing to attach an unremapped AST",
                    record.key,
                    record.id.raw(),
                    mapped.raw()
                ));
            }
        }
        *session = merged;
        file.sources = session.clone();
        Ok(())
    }

    fn resolve_imports(
        &self,
        file: &mut crate::ast::File,
        file_path: &std::path::Path,
    ) -> Result<(), crate::loader::LoadDiagnosticError> {
        if !file.imports.is_empty() {
            let base_dir = file_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf();
            let mut loader = crate::loader::ModuleLoader::new(base_dir);
            loader.load_main_with_file_diagnostic(file_path, file.clone())?;
            *file = loader.merge_all().map_err(|error| {
                crate::loader::LoadDiagnosticError::global(error, loader.source_registry().clone())
            })?;
        }
        crate::loader::merge_prelude_into(file);
        self.adopt_file_registry(file).map_err(|error| {
            crate::loader::LoadDiagnosticError::global(error, file.sources.clone())
        })
    }

    fn source_text_for_diagnostic(
        &self,
        record: Option<&crate::span::SourceRecord>,
        target_uri: Option<&str>,
        primary_uri: Option<&str>,
        primary_text: &str,
    ) -> Option<String> {
        if target_uri.is_some() && target_uri == primary_uri {
            return Some(primary_text.to_string());
        }
        if let Some(uri) = target_uri {
            if let Some(text) = self.documents.get(uri) {
                return Some(text.clone());
            }
        }
        record
            .and_then(|record| record.disk_path.as_deref())
            .and_then(|path| crate::path_safety::read_source_capped(path).ok())
    }

    pub(crate) fn compute_diagnostic_batches(
        &self,
        text: &str,
        uri: Option<&str>,
    ) -> Vec<DiagnosticBatch> {
        let tokens = match lexer::Lexer::new(text).tokenize() {
            Ok(tokens) => tokens,
            Err(error) => {
                return vec![DiagnosticBatch {
                    uri: uri.map(str::to_string),
                    diagnostics: vec![diagnostic::lexer_error_to_lsp(&error, Some(text))],
                }];
            }
        };

        let registration = match uri {
            Some(uri) => self.register_uri_source(uri),
            None => self.register_memory_source(text),
        };
        let (source_id, source_registry) = match registration {
            Ok(registration) => registration,
            Err(error) => {
                let diagnostic = Diagnostic::error(
                    format!("source registration failed: {error}"),
                    Span::UNKNOWN,
                );
                let mut batches = vec![DiagnosticBatch {
                    uri: None,
                    diagnostics: vec![diagnostic::diagnostic_to_lsp(&diagnostic, None)],
                }];
                if let Some(uri) = uri {
                    // Clear stale diagnostics for the active document while
                    // reporting the registration failure as a global message.
                    batches.push(DiagnosticBatch {
                        uri: Some(uri.to_string()),
                        diagnostics: Vec::new(),
                    });
                }
                return batches;
            }
        };
        let (mut file, parse_errors) =
            parser::Parser::new_with_source_registry(tokens, source_id, source_registry)
                .parse_file_with_recovery();
        let mut raw_diagnostics: Vec<Diagnostic> = parse_errors
            .iter()
            .map(|error| error.to_diagnostic())
            .collect();
        let import_result = if let Some(uri) = uri {
            self.uri_to_path_sandboxed(uri)
                .ok_or_else(|| {
                    crate::loader::LoadDiagnosticError::global(
                        format!("URI '{uri}' is outside the workspace"),
                        file.sources.clone(),
                    )
                })
                .and_then(|path| self.resolve_imports(&mut file, &path))
        } else {
            crate::loader::merge_prelude_into(&mut file);
            self.adopt_file_registry(&mut file).map_err(|error| {
                crate::loader::LoadDiagnosticError::global(error, file.sources.clone())
            })
        };
        if let Err(error) = import_result {
            file.sources = error.sources.clone();
            *self.source_registry.borrow_mut() = error.sources;
            raw_diagnostics.push(*error.diagnostic);
        }

        if let Err(errors) = core::check(&file) {
            raw_diagnostics.extend(errors);
        }

        let mut grouped: BTreeMap<Option<String>, Vec<Value>> = BTreeMap::new();
        for diagnostic in raw_diagnostics {
            let record = file.sources.record(diagnostic.span.source_id);
            // A registered source identifies ownership, but zero coordinates
            // still mean that no legal document range exists. Keep both
            // unknown-source and known-source/unknown-range failures in the
            // URI-less batch so they surface via window/showMessage.
            let has_known_range = diagnostic.span.start_line > 0 && diagnostic.span.start_col > 0;
            let target_uri = has_known_range
                .then(|| record.and_then(|record| record.canonical_uri.clone()))
                .flatten();
            let source_text = has_known_range
                .then(|| self.source_text_for_diagnostic(record, target_uri.as_deref(), uri, text))
                .flatten();
            grouped
                .entry(target_uri)
                .or_default()
                .push(diagnostic::diagnostic_to_lsp(
                    &diagnostic,
                    source_text.as_deref(),
                ));
        }
        if let Some(uri) = uri {
            grouped.entry(Some(uri.to_string())).or_default();
        }
        grouped
            .into_iter()
            .map(|(uri, diagnostics)| DiagnosticBatch { uri, diagnostics })
            .collect()
    }

    pub(crate) fn compute_diagnostic_notifications(&self, text: &str, uri: &str) -> Vec<Value> {
        self.compute_diagnostic_batches(text, Some(uri))
            .into_iter()
            .flat_map(|batch| {
                if let Some(uri) = batch.uri {
                    return vec![serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": { "uri": uri, "diagnostics": batch.diagnostics }
                    })];
                }
                // UNKNOWN-source diagnostics are explicitly global. LSP has no
                // legal publishDiagnostics URI for them, so surface them via
                // window/showMessage instead of dropping or blaming the active
                // document.
                batch
                    .diagnostics
                    .into_iter()
                    .filter_map(|diagnostic| {
                        let message = diagnostic.get("message")?.as_str()?;
                        Some(serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "window/showMessage",
                            "params": { "type": 1, "message": message }
                        }))
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub fn compute_diagnostics(&self, text: &str, uri: Option<&str>) -> Vec<Value> {
        let batches = self.compute_diagnostic_batches(text, uri);
        match uri {
            Some(uri) => batches
                .into_iter()
                .find(|batch| batch.uri.as_deref() == Some(uri))
                .map(|batch| batch.diagnostics)
                .unwrap_or_default(),
            None => batches
                .into_iter()
                .flat_map(|batch| batch.diagnostics)
                .collect(),
        }
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
        let (source_id, source_registry) = match self.register_uri_source(uri) {
            Ok(registration) => registration,
            Err(error) => {
                eprintln!("[mimi lsp] verification source registration failed: {error}");
                return diagnostics;
            }
        };
        let (file, _errors) =
            parser::Parser::new_with_source_registry(tokens, source_id, source_registry)
                .parse_file_with_recovery();

        // Find the enclosing function at cursor line
        let func = match find_enclosing_func_in_items(&file.items, text, cursor_line) {
            Some(f) => f,
            None => return diagnostics,
        };

        // Only verify if function has contracts
        let has_contracts = func.body.iter().any(|s| {
            matches!(
                s.unlocated(),
                Stmt::Requires(_, _)
                    | Stmt::Ensures(_, _)
                    | Stmt::Invariant(_, _)
                    | Stmt::MmsBlock { .. }
            )
        });
        if !has_contracts {
            return diagnostics;
        }

        // Verification and provenance must consume the same checked program;
        // re-checking after verification could observe a different lowering
        // catalog and attach a plausible but false Origin.
        let checked_program = match core::check_program(&file) {
            Ok(program) => program,
            Err(_) => return diagnostics,
        };

        // Compute body hash for caching
        let body_hash = hash_func_body(text, func);
        // Include URI in cache key to prevent collisions between identically
        // named functions in different files.
        let cache_key = format!("{}:{}", uri, func.name);

        // Check cache
        if let Some(cached) = self.verification_cache.get(&cache_key).cloned() {
            if cached.body_hash == body_hash {
                match &cached.status {
                    VerifStatus::Failed => {
                        let registry = self.source_registry.borrow();
                        if let Some(cached_diagnostic) = cached.diagnostic(&registry) {
                            diagnostics.push(diagnostic::diagnostic_to_lsp(
                                &cached_diagnostic,
                                Some(text),
                            ));
                            return diagnostics;
                        }
                        // A persistent v1 cache entry, or a v2 entry whose
                        // SourceKey cannot be remapped in this session, is not a
                        // safe location cache hit. Re-run verification instead
                        // of fabricating the function declaration range.
                    }
                    VerifStatus::Verified | VerifStatus::Unknown => return diagnostics,
                }
            }
        }

        // Dynamic timeout based on function complexity
        let func_body_lines = func
            .meta
            .span
            .start_col
            .saturating_sub(func.meta.span.start_line)
            .max(1);
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
        let results = verifier.verify_checked(&checked_program);
        for result in &results {
            if result.func_name != func.name {
                continue;
            }
            // Update cache (with LRU eviction)
            let mut structured_diagnostic =
                if matches!(result.status, VerifStatus::Failed) {
                    Some(result.diagnostic.clone().unwrap_or_else(|| {
                        Diagnostic::error(result.message.clone(), func.meta.span)
                    }))
                } else {
                    result.diagnostic.clone()
                };
            if let Some(diagnostic) = &mut structured_diagnostic {
                diagnostic.origin = verification_diagnostic_origin(
                    &checked_program,
                    &func.name,
                    func.meta.span,
                    diagnostic.span,
                );
            }
            self.cache_put_verification(
                cache_key.clone(),
                VerificationCacheEntry::new(
                    body_hash,
                    result.status.clone(),
                    result.message.clone(),
                    structured_diagnostic.clone(),
                ),
            );

            if matches!(result.status, VerifStatus::Failed) {
                if let Some(structured_diagnostic) = structured_diagnostic {
                    diagnostics.push(diagnostic::diagnostic_to_lsp(
                        &structured_diagnostic,
                        Some(text),
                    ));
                }
            }
        }

        // Persist cache to disk
        self.save_cache();

        diagnostics
    }
}
