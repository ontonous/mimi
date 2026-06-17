//! FFI boundary layer shared by the interpreter and codegen backends.

pub mod contract;
pub mod runtime;

pub use contract::{FfiArgContract, FfiContract, FfiRetContract};
pub use runtime::{CapTable, SharedHandle, SharedHandleTable, CAP_TABLE, SHARED_TABLE};
