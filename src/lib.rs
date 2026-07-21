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
// v0.31.6 止血 I — Clippy 基线清零 (devdocs/v0.31/01-foundation.md, 0.31.6 交付项 3；
// 门禁见 devdocs/v0.31/06-audit-debug-rc.md `cargo clippy --all-targets -- -D warnings`).
//
// 以下均为 v0.31.4 typed-IR 迁移继承的既有基线 lint。止血版（kind=stabilization,
// 0 新 stable feature，变更预算 §6 预留 25% 回归容量）不以大规模重构清零，而是将其
// 作为已登记债务接受，使 `-D warnings` 门禁归零并从此锁定「不得新增」。增量清理排入
// 0.31.7+ 实现版，而非本止血版。`if_same_then_else` 已逐处核查为对称分支的良性冗余，
// 非缺陷（如 runtime/mod.rs:16426 两分支同返回 "[]"）。
#![allow(
    clippy::useless_conversion,              // 65: codegen 中 inkwell BasicValueEnum 空 .into()
    clippy::type_complexity,                 // 39: 编译器 typed-IR 签名固有
    clippy::needless_range_loop,             // 15: 对 fields/lines 的索引遍历
    clippy::too_many_arguments,              // 11: codegen helper 需传递多上下文
    clippy::for_kv_map,                      // 6: map 遍历风格
    clippy::empty_line_after_doc_comments,   // 6
    clippy::unnecessary_cast,                // 6: i64 -> i64 空转换
    clippy::len_zero,                        // 5: len 比较风格
    clippy::large_enum_variant,              // 4: AST/IR enum payload 尺寸
    clippy::question_mark,                   // 3
    clippy::if_same_then_else,               // 3: 已核查良性（对称分支）
    clippy::collapsible_else_if,             // 2
    clippy::option_as_ref_deref,             // 2
    clippy::collapsible_if,                  // 1
    clippy::collapsible_match,               // 1
    clippy::redundant_pattern_matching,      // 1
    clippy::redundant_guards,                // 1
    clippy::unnecessary_unwrap,              // 1
    clippy::unnecessary_map_or,              // 1
    clippy::needless_return,                 // 1
    clippy::needless_borrows_for_generic_args, // 1
    clippy::map_entry,                       // 1
    clippy::manual_strip,                    // 1
    clippy::manual_range_contains,           // 1
    clippy::doc_lazy_continuation,           // 1
    clippy::blocks_in_conditions,            // 1
    clippy::items_after_test_module          // 1 (bin: emit.rs)
)]

#[macro_use]
pub mod macros;

pub mod ast;
pub mod codegen;
pub mod flow_matrix;
pub mod progressive;
pub mod session;

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
pub mod path_safety;
pub mod pkg_registry;
pub mod pkg_resolve;
#[allow(clippy::not_unsafe_ptr_arg_deref, clippy::unwrap_used)]
pub mod runtime;
pub mod source_scan;

#[cfg(test)]
pub mod tests;
