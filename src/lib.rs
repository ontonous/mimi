#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![allow(unexpected_cfgs)]

pub mod ast;
pub mod codegen;
pub mod contracts;
pub mod diagnostic;
pub mod error;
pub mod ffi;
pub mod fmt;
pub mod interp;
pub mod lexer;
pub mod lint;
pub mod loader;
pub mod lockfile;
pub mod lsp;
pub mod manifest;
pub mod parser;
pub mod span;
pub mod verifier;

pub mod core;
pub mod doc_core;
pub mod pkg_registry;
pub mod pkg_resolve;
pub mod runtime;

#[cfg(test)]
pub mod tests;
