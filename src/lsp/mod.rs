use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

use crate::loader::stdlib_dir;
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
const MAX_DOCUMENTS: usize = 256;
/// Maximum number of verification cache entries before LRU eviction.
/// Prevents unbounded memory growth in long-running LSP sessions.
pub(crate) const MAX_VERIFICATION_CACHE: usize = 4096;

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    body_hash: u64,
    status: String,
    message: String,
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
    /// L-H3: last seen textDocument version per URI (None = unknown).
    pub(crate) document_versions: HashMap<String, i64>,
    access_order: VecDeque<String>,
    workspace_root: Option<PathBuf>,
    last_cursor_line: usize,
    verification_cache: HashMap<String, (u64, VerifStatus, String)>,
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
    /// Simple parse cache: stores the last parsed text and its AST.
    /// Avoids re-parsing the same text multiple times per keystroke.
    /// Cleared on textDocument/didChange.
    parse_cache_text: std::cell::RefCell<(String, Option<crate::ast::File>)>,
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
            parse_cache_text: std::cell::RefCell::new((String::new(), None)),
        }
    }

    fn cache_file_path(&self) -> Option<PathBuf> {
        self.workspace_root
            .as_ref()
            .map(|root| root.join(".mimi").join("verify_cache.json"))
    }

    /// Insert a verification result into the cache. Used by tests.
    #[cfg(test)]
    /// Insert a verification result into the cache. Used by tests.
    #[cfg(test)]
    pub(crate) fn insert_verification_cache(
        &mut self,
        key: String,
        body_hash: u64,
        status: VerifStatus,
        message: String,
    ) {
        self.verification_cache
            .insert(key, (body_hash, status, message));
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
        if cache.version != 1 {
            return;
        }
        for (key, entry) in &cache.entries {
            let status = match entry.status.as_str() {
                "Verified" => VerifStatus::Verified,
                "Failed" => VerifStatus::Failed,
                _ => VerifStatus::Unknown,
            };
            self.verification_cache.insert(
                key.clone(),
                (entry.body_hash, status, entry.message.clone()),
            );
        }
    }

    pub(crate) fn save_cache(&self) {
        let Some(ref path) = self.cache_path else {
            return;
        };
        let entries: HashMap<String, CacheEntry> = self
            .verification_cache
            .iter()
            .map(|(key, (hash, status, msg))| {
                let status_str = match status {
                    VerifStatus::Verified => "Verified",
                    VerifStatus::Failed => "Failed",
                    VerifStatus::Unknown => "Unknown",
                };
                (
                    key.clone(),
                    CacheEntry {
                        body_hash: *hash,
                        status: status_str.to_string(),
                        message: msg.clone(),
                    },
                )
            })
            .collect();
        let cache = PersistentCache {
            version: 1,
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
            let (file, _parse_errors) =
                crate::parser::Parser::new(tokens).parse_file_with_recovery();

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
                if reader.read_line(&mut header).is_err() || header.is_empty() {
                    return Ok(());
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

            if len == 0 || len > MAX_CONTENT_LENGTH {
                continue;
            }

            // Read JSON body
            let mut body = vec![0u8; len];

            // CL-C1: consume the separator between Content-Length header and JSON body.
            // read_line includes the trailing \n. Protocol requires \r\n before body,
            // but some clients send only \n. Handle both by reading one byte and
            // optionally discarding a \r:
            let mut single = [0u8; 1];
            if let Err(e) = reader.read(&mut single) {
                eprintln!("[mimi lsp] failed to read separator byte: {}", e);
                continue;
            }
            if single[0] == b'\r' {
                // \r\n — consume the trailing \n too
                let mut nl = [0u8; 1];
                if let Err(e) = reader.read(&mut nl) {
                    eprintln!("[mimi lsp] failed to read newline byte: {}", e);
                    continue;
                }
            }

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
                let saved = std::mem::take(self);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    flow::transition(saved, &msg)
                }));
                match result {
                    Ok((new_self, Some(response))) => {
                        *self = new_self;
                        let resp_str = serde_json::to_string(&response).unwrap_or_default();
                        print!("Content-Length: {}\r\n\r\n{}", resp_str.len(), resp_str);
                        // M11-fix: log flush failure instead of silently ignoring
                        if let Err(e) = io::stdout().flush() {
                            eprintln!("[mimi lsp] failed to flush stdout: {}", e);
                        }
                    }
                    Ok((new_self, None)) => {
                        *self = new_self;
                    }
                    Err(_) => {
                        // AU-H2: restore caches + stdlib; drop verifier so AU-H3
                        // can recreate a fresh Z3 session on next verify.
                        self.documents = backup_docs;
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
                        self.verifier = None;
                        eprintln!(
                            "[lsp] handler panicked for method {:?}, state preserved (verifier reset)",
                            msg.get("method").and_then(|v| v.as_str())
                        );
                    }
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
}
