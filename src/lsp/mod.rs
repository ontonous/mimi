use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

use crate::verifier::{VerifStatus, Verifier};

pub(crate) mod code_actions;
pub(crate) mod completion;
pub(crate) mod diagnostic;
pub(crate) mod folding;
pub(crate) mod handlers;
pub(crate) mod hierarchy;
pub(crate) mod hover;
pub(crate) mod inlay;
pub(crate) mod json_rpc;
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
        self.verification_cache.insert(key, (body_hash, status, message));
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
            self.verification_cache
                .insert(key.clone(), (entry.body_hash, status, entry.message.clone()));
        }
    }

    pub(crate) fn save_cache(&self) {
        let Some(ref path) = self.cache_path else { return };
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
            reader
                .read_exact(&mut body)
                .map_err(|e| format!("read error: {}", e))?;
            let body = String::from_utf8(body).map_err(|e| format!("utf8 error: {}", e))?;

            // Skip empty line after body
            let mut newline = [0u8; 1];
            let _ = io::stdin().read(&mut newline);

            // Parse and handle (with panic catch to prevent server crash)
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&body) {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.handle_message(&msg)
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
            }
        }
    }

    pub(crate) fn handle_message(
        &mut self,
        msg: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        handlers::handle_message(self, msg)
    }
}
