//! FFI boundary layer shared by the interpreter and codegen backends.

pub mod contract;
pub mod c_header;
pub mod runtime;
pub mod callback;

pub use contract::{FfiArgContract, FfiContract, FfiRetContract};
pub use runtime::{CAP_TABLE, SHARED_TABLE};
