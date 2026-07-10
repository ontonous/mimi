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

/// LSP server for Mimi language
pub struct LspServer {
    pub(crate) documents: HashMap<String, String>,
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
        let data = match fs::read_to_string(&path) {
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
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string(&cache) {
            let _ = fs::write(path, data);
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
            let source = match fs::read_to_string(&path) {
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
                if header.starts_with("Content-Length:") {
                    break;
                }
            }

            let len: usize = header
                .trim()
                .strip_prefix("Content-Length: ")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);

            if len == 0 || len > MAX_CONTENT_LENGTH {
                continue;
            }

            // Read JSON body
            let mut body = vec![0u8; len];

            // Consume the separator between the Content-Length header
            // line and the JSON body.  read_line includes the trailing \n.
            // The protocol spec requires \r\n before the body, but some
            // clients send only \n.  Read one byte (the \n that was already
            // consumed by read_line) and optionally a \r byte before it.
            // Then discard the \r if present.
            let mut single = [0u8; 1];
            let _ = reader.read(&mut single);
            if single[0] == b'\r' {
                // \r\n — consume the trailing \n too
                let mut nl = [0u8; 1];
                let _ = reader.read(&mut nl);
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
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let (new_self, response) = flow::transition(std::mem::take(self), &msg);
                    *self = new_self;
                    response
                }));
                match result {
                    Ok(Some(response)) => {
                        let resp_str = serde_json::to_string(&response).unwrap_or_default();
                        print!("Content-Length: {}\r\n\r\n{}", resp_str.len(), resp_str);
                        io::stdout().flush().ok();
                    }
                    Ok(None) => {}
                    Err(_) => {
                        eprintln!(
                            "[lsp] handler panicked for method {:?}, continuing",
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

    }
