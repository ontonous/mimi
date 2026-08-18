use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

use crate::diagnostic::{Diagnostic, DiagnosticNote, DiagnosticOrigin, Severity};
use crate::loader::stdlib_dir;
use crate::span::{SourceKey, SourceRegistry, Span};
use crate::verifier::{VerifStatus, Verifier};

pub(crate) mod code_actions;
pub(crate) mod completion;
pub(crate) mod diagnostic;
pub(crate) mod flow;
pub(crate) mod folding;
pub(crate) mod hierarchy;
pub(crate) mod hover;
pub(crate) mod inlay;
pub(crate) mod lens;
pub(crate) mod position;
pub(crate) mod position_map;
pub(crate) mod references;
pub(crate) mod state;
pub(crate) mod symbols;
pub(crate) mod tokens;
pub(crate) mod util;

const MAX_CONTENT_LENGTH: usize = 16 * 1024 * 1024; // 16MB
/// AU-LSP-6 (full audit 2026-08-05): hard cap for draining oversized bodies.
/// Messages between MAX_CONTENT_LENGTH and this limit are discarded in
/// bounded chunks (never buffered whole — memory bomb) so the stream stays
/// in sync; anything above this limit closes the connection cleanly.
const HARD_MAX_CONTENT_LENGTH: usize = 64 * 1024 * 1024; // 64MB
const MAX_DOCUMENTS: usize = 256;
/// Hard cap on the number of per-line vectors built by LSP text utilities.
/// A 16MB document cannot safely produce millions of line entries.
pub(crate) const MAX_LSP_DOCUMENT_LINES: usize = 200_000;
/// Hard cap on total cached open-document bytes. Prevents unbounded cache
/// growth even when all LRU entries are still open.
pub(crate) const MAX_DOCUMENT_BYTES_TOTAL: usize = 256 * 1024 * 1024;
/// Hard cap on session-global SourceRegistry records. Each parsed File keeps
/// its own registry snapshot, so clearing the session pool when it exceeds
/// this cap is safe and prevents long-running didOpen/didClose churn from
/// leaking records indefinitely (batch4 P2-8).
pub(crate) const MAX_SOURCE_RECORDS: usize = 4096;
/// Hard cap for a single LSP header line. Applied before body-size limits so
/// an adversarial client cannot grow a header String without bound.
const MAX_HEADER_LINE: usize = 4096;
/// Maximum number of verification cache entries before LRU eviction.
/// Prevents unbounded memory growth in long-running LSP sessions.
pub(crate) const MAX_VERIFICATION_CACHE: usize = 4096;
/// Persistent verification-cache schema. Version 4 preserves owned semantic
/// origin alongside the stable SourceKey/span. Earlier schemas cannot prove
/// provenance and are rejected wholesale.
const VERIFICATION_CACHE_VERSION: u32 = 4;

/// Consume the separator between the header block and the body. The protocol
/// requires `\r\n`; tolerate a bare `\n` sent by some clients. `read_line`
/// already consumed the header line's terminating `\n`, so at most two
/// further bytes remain before the body.
fn consume_body_separator<R: Read>(reader: &mut R) -> io::Result<()> {
    let mut single = [0u8; 1];
    let n = reader.read(&mut single)?;
    if n == 0 {
        // Header already ended the stream — nothing to consume.
        return Ok(());
    }
    if single[0] == b'\r' {
        // \r\n — consume the trailing \n too
        let mut nl = [0u8; 1];
        let m = reader.read(&mut nl)?;
        if m == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "body separator: stream ended after CR",
            ));
        }
    }
    Ok(())
}

/// Read a single line with a hard length cap.
///
/// Returns `Ok(Some(line))` when a line (including its trailing `\n`) was
/// read, `Ok(None)` at EOF, and `Err` when the line exceeds `limit` before a
/// newline is encountered.
fn read_limited_line<R: BufRead>(
    reader: &mut R,
    limit: usize,
    out: &mut String,
) -> io::Result<Option<()>> {
    let mut bytes = Vec::new();
    loop {
        if bytes.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP header line exceeds hard limit",
            ));
        }
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            bytes.extend_from_slice(&buf[..=pos]);
            reader.consume(pos + 1);
            break;
        }
        bytes.extend_from_slice(buf);
        let len = buf.len();
        reader.consume(len);
    }
    *out = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Some(()))
}

/// Discard exactly `len` bytes from `reader`, reading in bounded chunks so
/// the body is never buffered whole. AU-LSP-6 (full audit 2026-08-05): an
/// oversized message must still have its body consumed; skipping it desyncs
/// the stream and every subsequent message is misparsed until EOF.
pub(crate) fn drain_discard<R: Read>(reader: &mut R, mut remaining: usize) -> io::Result<()> {
    let mut sink = [0u8; 8192];
    while remaining > 0 {
        let want = remaining.min(sink.len());
        match reader.read(&mut sink[..want]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "LSP body truncated before Content-Length bytes",
                ))
            }
            Ok(read) => remaining -= read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    body_hash: u64,
    status: String,
    message: String,
    #[serde(default)]
    diagnostic: Option<PersistedDiagnostic>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PersistedDiagnostic {
    source_key: String,
    start_line: usize,
    start_col: usize,
    end_line: usize,
    end_col: usize,
    severity: u8,
    code: Option<String>,
    message: String,
    notes: Vec<PersistedDiagnosticNote>,
    help: Option<String>,
    origin: DiagnosticOrigin,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PersistedDiagnosticNote {
    source_key: String,
    start_line: usize,
    start_col: usize,
    end_line: usize,
    end_col: usize,
    message: String,
}

#[derive(Clone)]
pub(crate) struct VerificationCacheEntry {
    pub(crate) body_hash: u64,
    pub(crate) status: VerifStatus,
    pub(crate) message: String,
    diagnostic: Option<Diagnostic>,
    persisted_diagnostic: Option<PersistedDiagnostic>,
}

/// 0.34.44 (ADR-008 §2): the ONLY cache-key shape for verification verdicts.
///
/// The key carries the ENGINE identity (`resolved` — the LSP never runs or
/// caches the flow/VIR engine) and the semantics version, so:
/// - pre-0.34.44 persisted entries (no engine segment) never match → the old
///   on-disk cache auto-invalidates on upgrade (fail-loud, never silent reuse);
/// - any future engine switch or semantics bump invalidates every entry.
pub(crate) fn verification_cache_key(uri: &str, func_name: &str) -> String {
    format!(
        "{}:{}:{}:v{}",
        uri,
        func_name,
        crate::verifier::ProofArtifact::ENGINE_RESOLVED,
        crate::verifier::ProofArtifact::SEMANTICS_VERSION
    )
}

#[derive(Clone, Default)]
struct ParseCacheEntry {
    source_key: String,
    text: String,
    file: Option<crate::ast::File>,
}

impl PersistedDiagnostic {
    fn from_runtime(diagnostic: &Diagnostic, registry: &SourceRegistry) -> Option<Self> {
        let origin = diagnostic.origin.clone()?;
        let source_key = registry
            .key(diagnostic.span.source_id)?
            .as_str()
            .to_string();
        let notes = diagnostic
            .notes
            .iter()
            .map(|note| {
                Some(PersistedDiagnosticNote {
                    source_key: registry.key(note.span.source_id)?.as_str().to_string(),
                    start_line: note.span.start_line,
                    start_col: note.span.start_col,
                    end_line: note.span.end_line,
                    end_col: note.span.end_col,
                    message: note.message.clone(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            source_key,
            start_line: diagnostic.span.start_line,
            start_col: diagnostic.span.start_col,
            end_line: diagnostic.span.end_line,
            end_col: diagnostic.span.end_col,
            severity: match diagnostic.severity {
                Severity::Error => 1,
                Severity::Warning => 2,
                Severity::Note => 3,
                Severity::Help => 4,
            },
            code: diagnostic.code.clone(),
            message: diagnostic.message.clone(),
            notes,
            help: diagnostic.help.clone(),
            origin,
        })
    }

    fn to_runtime(&self, registry: &SourceRegistry) -> Option<Diagnostic> {
        let source_key = SourceKey::new(self.source_key.clone()).ok()?;
        let source_id = registry.id_for_key(&source_key)?;
        let span = Span::new(self.start_line, self.start_col, self.end_line, self.end_col)
            .with_source(source_id);
        let notes = self
            .notes
            .iter()
            .map(|note| {
                let key = SourceKey::new(note.source_key.clone()).ok()?;
                let source_id = registry.id_for_key(&key)?;
                Some(DiagnosticNote {
                    message: note.message.clone(),
                    span: Span::new(note.start_line, note.start_col, note.end_line, note.end_col)
                        .with_source(source_id),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Diagnostic {
            message: self.message.clone(),
            span,
            severity: match self.severity {
                1 => Severity::Error,
                2 => Severity::Warning,
                3 => Severity::Note,
                4 => Severity::Help,
                _ => return None,
            },
            code: self.code.clone(),
            notes,
            help: self.help.clone(),
            origin: Some(self.origin.clone()),
        })
    }
}

impl VerificationCacheEntry {
    pub(crate) fn new(
        body_hash: u64,
        status: VerifStatus,
        message: String,
        diagnostic: Option<Diagnostic>,
    ) -> Self {
        Self {
            body_hash,
            status,
            message,
            diagnostic,
            persisted_diagnostic: None,
        }
    }

    pub(crate) fn diagnostic(&self, registry: &SourceRegistry) -> Option<Diagnostic> {
        self.diagnostic
            .as_ref()
            .filter(|diagnostic| registry.record(diagnostic.span.source_id).is_some())
            .cloned()
            .or_else(|| {
                self.persisted_diagnostic
                    .as_ref()
                    .and_then(|diagnostic| diagnostic.to_runtime(registry))
            })
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct PersistentCache {
    version: u32,
    entries: HashMap<String, CacheEntry>,
}

/// L-H6: JSON-RPC / LSP session lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleState {
    /// Before successful `initialize`.
    Uninitialized,
    /// After `initialize`, before `initialized` notification (requests limited).
    Initializing,
    /// Fully running — all methods allowed.
    Running,
    /// After `shutdown` request; only `exit` is valid.
    Shutdown,
    /// After `exit` notification.
    Exited,
}

/// LSP server for Mimi language
pub struct LspServer {
    pub(crate) documents: HashMap<String, String>,
    /// Total bytes currently stored in `documents`; used to enforce a hard
    /// memory cap even when all cached documents are still open.
    pub(crate) total_doc_bytes: usize,
    /// L-H3: last seen textDocument version per URI (None = unknown).
    pub(crate) document_versions: HashMap<String, i64>,
    access_order: VecDeque<String>,
    workspace_root: Option<PathBuf>,
    last_cursor_line: usize,
    pub(crate) verification_cache: HashMap<String, VerificationCacheEntry>,
    /// LRU tracking for verification cache eviction.
    cache_access_order: VecDeque<String>,
    verifier: Option<Verifier>,
    cache_path: Option<PathBuf>,
    /// Stdlib completions: module_name -> Vec<(func_name, detail, insert_text)>
    stdlib_funcs: HashMap<String, Vec<(String, String, String)>>,
    /// Flat list of all stdlib function completion items for "top" context
    stdlib_completions_raw: Vec<Value>,
    stdlib_loaded: bool,
    /// Set by the LSP `exit` notification. The real server run loop checks
    /// this flag after handling a message and terminates cleanly instead of
    /// calling `process::exit`, which would kill the test runner when
    /// `handle_message` is exercised directly in unit tests.
    should_exit: bool,
    /// L-H6: session lifecycle gate for method dispatch.
    pub(crate) lifecycle: LifecycleState,
    /// Last parsed document, keyed by both stable source identity and text.
    /// Text alone is insufficient because two documents may have identical
    /// contents while requiring distinct SourceId/URI ownership.
    parse_cache: std::cell::RefCell<ParseCacheEntry>,
    /// Session-local source interner shared by unsaved buffers and diagnostics.
    pub(crate) source_registry: std::cell::RefCell<crate::span::SourceRegistry>,
    /// Additional JSON-RPC notifications produced by one transition. The
    /// transition API retains its single primary response for unit tests; the
    /// real run loop drains this queue and writes every notification.
    pending_notifications: VecDeque<Value>,
    /// URI whose request is currently being served. Text-only helper APIs use
    /// this to select the correct source-aware parse cache entry.
    active_document_uri: Option<String>,
}

impl Default for LspServer {
    fn default() -> Self {
        Self::new()
    }
}

impl LspServer {
    pub fn new() -> Self {
        LspServer {
            documents: HashMap::new(),
            total_doc_bytes: 0,
            document_versions: HashMap::new(),
            access_order: VecDeque::new(),
            workspace_root: None,
            last_cursor_line: 0,
            verification_cache: HashMap::new(),
            cache_access_order: VecDeque::new(),
            verifier: None,
            cache_path: None,
            stdlib_funcs: HashMap::new(),
            stdlib_completions_raw: Vec::new(),
            stdlib_loaded: false,
            should_exit: false,
            lifecycle: LifecycleState::Uninitialized,
            parse_cache: std::cell::RefCell::new(ParseCacheEntry::default()),
            source_registry: std::cell::RefCell::new(crate::span::SourceRegistry::default()),
            pending_notifications: VecDeque::new(),
            active_document_uri: None,
        }
    }

    fn cache_file_path(&self) -> Option<PathBuf> {
        self.workspace_root
            .as_ref()
            .map(|root| root.join(".mimi").join("verify_cache.json"))
    }

    /// Insert a verification result into the cache. Used by tests.
    #[cfg(test)]
    pub(crate) fn insert_verification_cache(
        &mut self,
        key: String,
        body_hash: u64,
        status: VerifStatus,
        message: String,
    ) {
        self.verification_cache.insert(
            key,
            VerificationCacheEntry::new(body_hash, status, message, None),
        );
    }

    #[cfg(test)]
    pub(crate) fn insert_verification_cache_with_diagnostic(
        &mut self,
        key: String,
        body_hash: u64,
        status: VerifStatus,
        message: String,
        diagnostic: Diagnostic,
    ) {
        self.verification_cache.insert(
            key,
            VerificationCacheEntry::new(body_hash, status, message, Some(diagnostic)),
        );
    }

    pub(crate) fn load_cache(&mut self) {
        let path = self.cache_file_path();
        self.cache_path = path.clone();
        let Some(path) = path else { return };
        let data = match crate::path_safety::read_source_capped(&path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let cache: PersistentCache = match serde_json::from_str(&data) {
            Ok(c) => c,
            Err(_) => return,
        };
        if cache.version != VERIFICATION_CACHE_VERSION {
            return;
        }
        for (key, entry) in &cache.entries {
            let status = match entry.status.as_str() {
                "Verified" | "Proven" => VerifStatus::Proven,
                "Failed" | "Disproven" => VerifStatus::Disproven,
                "NotInTrustedSubset" => VerifStatus::NotInTrustedSubset,
                "SolverUnknown" => VerifStatus::SolverUnknown,
                "Timeout" => VerifStatus::Timeout,
                "InfrastructureError" => VerifStatus::InfrastructureError,
                "RuntimeOnlyContract" => VerifStatus::RuntimeOnlyContract,
                "NoObligations" => VerifStatus::NoObligations,
                _ => VerifStatus::SolverUnknown,
            };
            self.verification_cache.insert(
                key.clone(),
                VerificationCacheEntry {
                    body_hash: entry.body_hash,
                    status,
                    message: entry.message.clone(),
                    diagnostic: None,
                    persisted_diagnostic: entry.diagnostic.clone(),
                },
            );
        }
    }

    pub(crate) fn save_cache(&self) {
        let Some(ref path) = self.cache_path else {
            return;
        };
        let registry = self.source_registry.borrow();
        let entries: HashMap<String, CacheEntry> = self
            .verification_cache
            .iter()
            .map(|(key, entry)| {
                let status_str = match entry.status.clone() {
                    VerifStatus::Proven => "Verified",
                    VerifStatus::Disproven => "Failed",
                    VerifStatus::NotInTrustedSubset => "NotInTrustedSubset",
                    VerifStatus::SolverUnknown => "SolverUnknown",
                    VerifStatus::Timeout => "Timeout",
                    VerifStatus::InfrastructureError => "InfrastructureError",
                    VerifStatus::RuntimeOnlyContract => "RuntimeOnlyContract",
                    VerifStatus::NoObligations => "NoObligations",
                };
                (
                    key.clone(),
                    CacheEntry {
                        body_hash: entry.body_hash,
                        status: status_str.to_string(),
                        message: entry.message.clone(),
                        diagnostic: entry
                            .diagnostic
                            .as_ref()
                            .and_then(|diagnostic| {
                                PersistedDiagnostic::from_runtime(diagnostic, &registry)
                            })
                            .or_else(|| entry.persisted_diagnostic.clone()),
                    },
                )
            })
            .collect();
        let cache = PersistentCache {
            version: VERIFICATION_CACHE_VERSION,
            entries,
        };
        if let Some(parent) = path.parent() {
            // H11-fix: propagate directory creation failure instead of silently ignoring
            if let Err(e) = fs::create_dir_all(parent) {
                eprintln!("[mimi lsp] failed to create cache directory: {}", e);
            }
        }
        if let Ok(data) = serde_json::to_string(&cache) {
            // H11-fix: propagate write failure instead of silently ignoring
            if let Err(e) = fs::write(path, data) {
                eprintln!("[mimi lsp] failed to write cache file: {}", e);
            }
        }
    }

    /// Load stdlib function completions by scanning stdlib .mimi files.
    /// Populates stdlib_funcs and stdlib_completions_raw.
    pub(crate) fn load_stdlib_completions(&mut self) {
        if self.stdlib_loaded {
            return;
        }
        self.stdlib_loaded = true;
        let Some(std_dir) = stdlib_dir() else { return };
        let dir = match fs::read_dir(&std_dir) {
            Ok(d) => d,
            Err(_) => return,
        };
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e != "mimi").unwrap_or(true) {
                continue;
            }
            let module_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if module_name.is_empty() {
                continue;
            }
            let source = match crate::path_safety::read_source_capped(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let tokens = match crate::lexer::Lexer::new(&source).tokenize() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let Ok(parser) = crate::loader::parser_for_path(tokens, &path) else {
                continue;
            };
            let (file, _parse_errors) = parser.parse_file_with_recovery();

            let mut funcs = Vec::new();
            for item in &file.items {
                if let crate::ast::Item::Func(f) = item {
                    let params_str: Vec<String> = f
                        .params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, crate::core::fmt_type(&p.ty)))
                        .collect();
                    let ret_str = f
                        .ret
                        .as_ref()
                        .map(crate::core::fmt_type)
                        .unwrap_or_else(|| "unit".to_string());
                    let detail = format!("{}({}) -> {}", f.name, params_str.join(", "), ret_str);
                    let insert_text = format!("{}(${{1}})", f.name);
                    self.stdlib_completions_raw.push(serde_json::json!({
                        "label": f.name,
                        "kind": 3, // Function
                        "detail": detail,
                        "insertText": insert_text,
                        "insertTextFormat": 2,
                    }));
                    funcs.push((f.name.clone(), detail, format!("{}(${{1}})", f.name)));
                }
            }
            if !funcs.is_empty() {
                self.stdlib_funcs.insert(module_name, funcs);
            }
        }
    }

    /// Run the LSP server (stdin/stdout JSON-RPC)
    pub fn run(&mut self) -> Result<(), String> {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        let mut buffer = String::new();

        loop {
            buffer.clear();
            // Read Content-Length header
            let mut header = String::new();
            loop {
                header.clear();
                if read_limited_line(&mut reader, MAX_HEADER_LINE, &mut header)
                    .map_err(|e| e.to_string())?
                    .is_none()
                {
                    return Ok(());
                }
                if header.is_empty() {
                    continue;
                }
                // CL-H9 (deep audit): LSP headers are case-insensitive.
                // Also handle optional whitespace after colon.
                let header_lower = header.to_lowercase();
                if header_lower.starts_with("content-length:") {
                    break;
                }
            }

            // CL-H9: use case-insensitive strip + trim for robust parsing.
            let len: usize = header
                .trim()
                .to_lowercase()
                .strip_prefix("content-length:")
                .map(|s| s.trim())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);

            if len == 0 {
                continue;
            }

            // AU-LSP-6 (full audit 2026-08-05): an oversized message must still
            // consume its body. The old `continue` left the body in the stream,
            // desyncing it so subsequent messages were misparsed until EOF.
            // Between the soft and hard caps the body is discarded in bounded
            // chunks (never allocated whole); above the hard cap the connection
            // closes cleanly instead of draining an unbounded memory bomb.
            if len > MAX_CONTENT_LENGTH {
                if len > HARD_MAX_CONTENT_LENGTH {
                    eprintln!(
                        "[mimi lsp] Content-Length {} exceeds hard cap ({} bytes); closing",
                        len, HARD_MAX_CONTENT_LENGTH
                    );
                    return Ok(());
                }
                if let Err(e) = consume_body_separator(&mut reader) {
                    eprintln!("[mimi lsp] failed to read separator byte: {e}");
                    return Ok(());
                }
                if let Err(e) = drain_discard(&mut reader, len) {
                    eprintln!("[mimi lsp] failed to drain oversized body: {e}");
                    return Ok(());
                }
                continue;
            }

            // CL-C1: consume the separator between Content-Length header and JSON body.
            // read_line includes the trailing \n. Protocol requires \r\n before body,
            // but some clients send only \n. Handle both.
            if let Err(e) = consume_body_separator(&mut reader) {
                eprintln!("[mimi lsp] failed to read separator byte: {}", e);
                continue;
            }

            // Read JSON body
            let mut body = vec![0u8; len];
            reader
                .read_exact(&mut body)
                .map_err(|e| format!("read error: {}", e))?;
            let body = String::from_utf8(body).map_err(|e| format!("utf8 error: {}", e))?;

            // Trailing empty line after body is consumed by the header-read loop
            // below: `read_line` will return it as an empty line that doesn't
            // start with "Content-Length:", so the loop continues to the
            // actual Content-Length header of the next message.

            // Parse and handle (with panic catch to prevent server crash)
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&body) {
                // SEC-C5 / AU-H2: preserve document + caches across panic recovery.
                // mem::take moves the server into catch_unwind; on panic the moved
                // value is dropped and *self is Default — restore everything that
                // does not hold live Z3 state (verifier is intentionally cleared).
                let backup_docs = self.documents.clone();
                let backup_versions = self.document_versions.clone();
                let backup_access = self.access_order.clone();
                let backup_workspace = self.workspace_root.clone();
                let backup_cursor = self.last_cursor_line;
                let backup_verif_cache = self.verification_cache.clone();
                let backup_cache_order = self.cache_access_order.clone();
                let backup_cache_path = self.cache_path.clone();
                let backup_stdlib_funcs = self.stdlib_funcs.clone();
                let backup_stdlib_raw = self.stdlib_completions_raw.clone();
                let backup_stdlib_loaded = self.stdlib_loaded;
                let backup_should_exit = self.should_exit;
                let backup_lifecycle = self.lifecycle;
                let backup_parse_cache = self.parse_cache.borrow().clone();
                let backup_sources = self.source_registry.borrow().clone();
                let backup_pending = self.pending_notifications.clone();
                let backup_active_uri = self.active_document_uri.clone();
                let saved = std::mem::take(self);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    flow::transition(saved, &msg)
                }));
                let mut outbound = Vec::new();
                match result {
                    Ok((new_self, Some(response))) => {
                        *self = new_self;
                        outbound.push(response);
                        outbound.extend(self.pending_notifications.drain(..));
                    }
                    Ok((new_self, None)) => {
                        *self = new_self;
                        outbound.extend(self.pending_notifications.drain(..));
                    }
                    Err(_) => {
                        // AU-H2: restore caches + stdlib; drop verifier so AU-H3
                        // can recreate a fresh Z3 session on next verify.
                        self.documents = backup_docs;
                        self.document_versions = backup_versions;
                        self.access_order = backup_access;
                        self.workspace_root = backup_workspace;
                        self.last_cursor_line = backup_cursor;
                        self.verification_cache = backup_verif_cache;
                        self.cache_access_order = backup_cache_order;
                        self.cache_path = backup_cache_path;
                        self.stdlib_funcs = backup_stdlib_funcs;
                        self.stdlib_completions_raw = backup_stdlib_raw;
                        self.stdlib_loaded = backup_stdlib_loaded;
                        self.should_exit = backup_should_exit;
                        self.lifecycle = backup_lifecycle;
                        *self.parse_cache.borrow_mut() = backup_parse_cache;
                        *self.source_registry.borrow_mut() = backup_sources;
                        self.pending_notifications = backup_pending;
                        self.active_document_uri = backup_active_uri;
                        self.verifier = None;
                        eprintln!(
                            "[lsp] handler panicked for method {:?}, state preserved (verifier reset)",
                            msg.get("method").and_then(|v| v.as_str())
                        );
                    }
                }
                for response in outbound {
                    let resp_str = serde_json::to_string(&response).unwrap_or_default();
                    // P3: use write! and handle errors instead of print!, which
                    // can panic when the client disconnects (EPIPE).
                    if let Err(e) = io::stdout().write_all(
                        format!("Content-Length: {}\r\n\r\n{}", resp_str.len(), resp_str)
                            .as_bytes(),
                    ) {
                        eprintln!("[mimi lsp] failed to write response: {}", e);
                    }
                }
                if let Err(e) = io::stdout().flush() {
                    eprintln!("[mimi lsp] failed to flush stdout: {}", e);
                }
                if self.should_exit {
                    return Ok(());
                }
            }
        }
    }

    /// Convenience wrapper: process a single JSON-RPC message and return the response.
    /// Used by unit tests. Takes `&mut self` for API compat; internally calls
    /// `flow::transition` which takes ownership and returns the updated server.
    #[allow(dead_code)]
    pub(crate) fn handle_message(&mut self, msg: &serde_json::Value) -> Option<serde_json::Value> {
        let server = std::mem::take(self);
        let (updated, response) = flow::transition(server, msg);
        *self = updated;
        response
    }

    #[cfg(test)]
    pub(crate) fn drain_pending_notifications(&mut self) -> Vec<Value> {
        self.pending_notifications.drain(..).collect()
    }

    /// Test-only: configure the workspace root (M10 sandbox tests).
    #[cfg(test)]
    pub(crate) fn set_workspace_root_for_test(&mut self, root: PathBuf) {
        self.workspace_root = Some(root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn lsp_header_line_cap_rejects_oversize_line() {
        let mut cur = Cursor::new(vec![b'x'; 8192]);
        let mut line = String::new();
        let err = read_limited_line(&mut cur, 4096, &mut line)
            .expect_err("oversize header line must fail closed");
        assert!(err.kind() == io::ErrorKind::InvalidData);
    }

    #[test]
    fn lsp_header_line_cap_keeps_normal_line() {
        let mut cur = Cursor::new(b"Content-Length: 42\r\n\r\n{}".to_vec());
        let mut line = String::new();
        assert!(read_limited_line(&mut cur, 4096, &mut line)
            .unwrap()
            .is_some());
        assert_eq!(line, "Content-Length: 42\r\n");
    }
}
