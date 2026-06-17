//! FFI boundary layer shared by the interpreter and codegen backends.

pub mod contract;
pub mod c_header;
pub mod runtime;

pub use contract::{FfiArgContract, FfiContract, FfiRetContract};
pub use c_header::generate_c_header;
pub use runtime::{CapTable, SharedHandle, SharedHandleTable, CAP_TABLE, SHARED_TABLE};
