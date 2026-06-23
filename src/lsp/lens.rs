use serde_json::Value;

use crate::ast::{Item, Stmt};
use crate::lsp::symbols::count_text_references;
use crate::lsp::LspServer;
use crate::verifier::VerifStatus;

impl LspServer {
    /// Compute code lenses for a document: reference counts and verification status
    pub fn compute_code_lens(&self, text: &str, uri: &str) -> Vec<Value> {
        let mut lenses = Vec::new();
        let file = match self.parse_with_recovery(text) {
            Some(f) => f,
            None => return lenses,
        };
        for item in &file.items {
            match item {
                Item::Func(f) => {
                    let def_line = text
                        .lines()
                        .position(|l| l.contains(&format!("func {}", f.name)))
                        .unwrap_or(0);
                    lenses.push(code_lens_value(def_line, count_text_references(text, &f.name)));

                    // Add verification status lens if function has contracts
                    let has_contracts = f.body.iter().any(|s| matches!(s,
                        Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Invariant(_, _)
                    ));
                    if has_contracts {
                        let cache_key = format!("{}:{}", uri, f.name);
                        let verify_title = if let Some((_, status, msg)) = self.verification_cache.get(&cache_key) {
                            match status {
                                VerifStatus::Verified => format!("✓ {}", msg),
                                VerifStatus::Failed => format!("✗ {}", msg),
                                VerifStatus::Unknown => format!("? {}", msg),
                            }
                        } else {
                            "verify".to_string()
                        };
                        lenses.push(serde_json::json!({
                            "range": {
                                "start": { "line": def_line, "character": 0 },
                                "end": { "line": def_line, "character": 0 }
                            },
                            "command": {
                                "title": verify_title,
                                "command": ""
                            }
                        }));
                    }
                }
                Item::Type(t) => {
                    let def_line = text
                        .lines()
                        .position(|l| l.contains(&format!("type {}", t.name)))
                        .unwrap_or(0);
                    lenses.push(code_lens_value(def_line, count_text_references(text, &t.name)));
                }
                Item::Trait(t) => {
                    let def_line = text
                        .lines()
                        .position(|l| l.contains(&format!("trait {}", t.name)))
                        .unwrap_or(0);
                    lenses.push(code_lens_value(def_line, count_text_references(text, &t.name)));
                }
                Item::Impl(i) => {
                    let def_line = text.lines().position(|l| l.contains("impl")).unwrap_or(0);
                    lenses.push(serde_json::json!({
                        "range": {
                            "start": { "line": def_line, "character": 0 },
                            "end": { "line": def_line, "character": 0 }
                        },
                        "command": {
                            "title": format!("{} method{}", i.methods.len(), if i.methods.len() == 1 { "" } else { "s" }),
                            "command": ""
                        }
                    }));
                }
                Item::Actor(a) => {
                    let def_line = text
                        .lines()
                        .position(|l| l.contains(&format!("actor {}", a.name)))
                        .unwrap_or(0);
                    lenses.push(code_lens_value(def_line, count_text_references(text, &a.name)));
                }
                _ => {}
            }
        }
        lenses
    }
}

/// Build a code lens JSON object showing reference count at given line
pub(crate) fn code_lens_value(line: usize, count: usize) -> Value {
    serde_json::json!({
        "range": {
            "start": { "line": line, "character": 0 },
            "end": { "line": line, "character": 0 }
        },
        "command": {
            "title": format!("{} reference{}", count, if count == 1 { "" } else { "s" }),
            "command": ""
        }
    })
}
