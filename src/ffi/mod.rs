//! FFI boundary layer shared by the interpreter and codegen backends.

pub mod contract;

pub use contract::{FfiArgContract, FfiContract, FfiRetContract};
