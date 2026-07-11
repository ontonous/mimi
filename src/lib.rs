//! Mimi language reference implementation.
//!
//! This crate provides the compiler, interpreter, and runtime for the Mimi
//! language. It is intended to be consumed both as the `mimi` CLI binary and
//! as a library (`libmimi`) for tooling, testing, and the upcoming v0.29
//! MimiSpec bootstrap.
//!
//! # Public API stability
//!
//! The modules exported here are the supported library surface. Stability
//! commitments for the v0.29 bootstrap are:
//!
//! - **`ast`**, **`lexer`**, **`parser`**: stable parsing pipeline. The AST
//!   shape and lexer/parser entry points are required by the bootstrap parser.
//! - **`core`**: stable type-checking API (`check`, `check_strict`).
//! - **`codegen`**: stable LLVM code generation API (`CodeGenerator`).
//! - **`interp`**: stable interpreter API for `mimi run` semantics and
//!   comptime evaluation.
//! - **`contracts`**, **`verifier`**: stable contract extraction and Z3
//!   verification API.
//! - **`runtime`**: stable runtime symbols used by generated binaries.
//! - **`loader`**, **`manifest`**, **`lockfile`**, **`pkg_registry`**,
//!   **`pkg_resolve`**: package management API.
//!
//! Modules marked `pub` but not listed above are still public for testing and
//! tooling, but may evolve more quickly.
//!
//! # Bootstrap interface (v0.29)
//!
//! The v0.29 bootstrap will compile the MimiSpec parser using Mimi itself. The
//! expected bootstrap pipeline is:
//!
//! ```text
//! mimispec source
//!     -> lexer::Lexer::tokenize
//!     -> parser::Parser::parse_file
//!     -> core::check
//!     -> codegen::CodeGenerator::compile_file
//!     -> object file / executable
//! ```
//!
//! The `mimispec` external crate remains the canonical parser during the
//! transition; `mimi mms` exposes it through `mimispec::parse`.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![allow(unexpected_cfgs)]

#[macro_use]
pub mod macros;

pub mod ast;
pub mod codegen;
pub mod flow_matrix;

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
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub mod runtime;

#[cfg(test)]
pub mod tests;
