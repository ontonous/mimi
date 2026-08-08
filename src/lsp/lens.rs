use serde_json::Value;

use crate::ast::{Item, Stmt};
use crate::lsp::symbols::count_text_references;
use crate::lsp::LspServer;
use crate::verifier::VerifStatus;

impl LspServer {
    /// Compute code lenses for a document: reference counts and verification status
    pub fn compute_code_lens(&self, text: &str, uri: &str) -> Vec<Value> {
        let mut lenses = Vec::new();
        let file = match self.parse_with_recovery_for_uri(text, Some(uri)) {
            Some(f) => f,
            None => return lenses,
        };
        for item in &file.items {
            match item {
                Item::Func(f) => {
                    // 0.35.15 (DX backlog #3): AST span replaces the
                    // `func {name}` substring scan (which landed on the
                    // first mention, e.g. a call site or comment).
                    let def_line = f.meta.span.start_line.saturating_sub(1);
                    lenses.push(code_lens_value(
                        def_line,
                        count_text_references(text, &f.name),
                    ));

                    // Add verification status lens if function has contracts
                    let has_contracts = f.body.iter().any(|s| {
                        matches!(
                            s.unlocated(),
                            Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Invariant(_, _)
                        )
                    });
                    if has_contracts {
                        // 0.34.44 (ADR-008 §2): same engine-scoped key as the
                        // write path (state.rs) — reading with any other shape
                        // would silently resurrect pre-isolation entries.
                        let cache_key = super::verification_cache_key(uri, &f.name);
                        let verify_title =
                            if let Some(entry) = self.verification_cache.get(&cache_key) {
                                match entry.status.clone() {
                                    VerifStatus::Proven => format!("✓ {}", entry.message),
                                    VerifStatus::Disproven => format!("✗ {}", entry.message),
                                    _ => format!("? {}", entry.message),
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
                    // 0.35.15 (DX backlog #3): AST span replaces the
                    // `type {name}` substring scan.
                    let def_line = t.meta.span.start_line.saturating_sub(1);
                    lenses.push(code_lens_value(
                        def_line,
                        count_text_references(text, &t.name),
                    ));
                }
                Item::Trait(t) => {
                    // 0.35.15 (DX backlog #3): AST span replaces the
                    // `trait {name}` substring scan.
                    let def_line = t.meta.span.start_line.saturating_sub(1);
                    lenses.push(code_lens_value(
                        def_line,
                        count_text_references(text, &t.name),
                    ));
                }
                Item::Impl(i) => {
                    // 0.35.15 (DX backlog #3): AST span replaces the
                    // `contains("impl")` scan, which landed on the FIRST
                    // impl in the file regardless of which item was being
                    // rendered.
                    let def_line = i.meta.span.start_line.saturating_sub(1);
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
                    // 0.35.15 (DX backlog #3): AST span replaces the
                    // `actor {name}` substring scan.
                    let def_line = a.meta.span.start_line.saturating_sub(1);
                    lenses.push(code_lens_value(
                        def_line,
                        count_text_references(text, &a.name),
                    ));
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
