//! Standalone crate root for building the Mimi runtime as a static library.
//!
//! This file is compiled separately with `rustc` to produce `libmimi_runtime.a`,
//! which is linked with LLVM-codegened Mimi programs.
//!
//! ```sh
//! rustc --edition 2021 --crate-type staticlib --cfg standalone --crate-name mimi_runtime \
//!       -o libmimi_runtime.a src/runtime/standalone.rs
//! ```

#![allow(clippy::not_unsafe_ptr_arg_deref)]

// The implementation is shared with the main crate's `src/runtime/mod.rs`.
// This avoids code duplication while allowing both in-crate and standalone builds.
include!("mod.rs");
