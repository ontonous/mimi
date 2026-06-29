//! Re-export Mimi runtime C ABI symbols into a shared library so that
//! generated FFI bindings can link against `-lmimi_runtime`.
pub use mimi::runtime::*;
