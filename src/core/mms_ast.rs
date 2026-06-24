//! MimiSpec AST types — crate-independent type definitions for parsed `mms{}` blocks.
//!
//! These types mirror `mimispec::ast` types without depending on the mimispec crate.
//! The Rust-side `parse_stmt.rs` converts from mimispec crate types; the Mimi-side
//! parser directly produces these types.

/// Top-level parsed MimiSpec file
#[derive(Debug, Clone)]
pub struct MmsFile {
    pub imports: Vec<String>,
    pub fragments: Vec<MmsFragment>,
}

/// A fragment (top-level declaration) in a MimiSpec file
#[derive(Debug, Clone)]
pub enum MmsFragment {
    Module {
        name: String,
        desc: Option<String>,
        items: Vec<MmsFragment>,
    },
    Func {
        name: String,
        desc: Option<String>,
    },
    TypeDef {
        name: String,
        desc: Option<String>,
    },
    Steps,
    Placeholder,
    Unknown(String),
}

/// Result of parsing an mms{} block content
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MmsParseResult {
    pub file: Option<MmsFile>,
    pub errors: Vec<MmsParseError>,
}

/// A parsing error with source location
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MmsParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}
