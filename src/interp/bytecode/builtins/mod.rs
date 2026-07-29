//! Builtin function modules, organized by category.
//!
//! Each module exports standalone `fn(vm, args) -> Result<Value>` functions
//! and a `register(reg: &mut BuiltinRegistry)` function.

pub mod convert;
pub mod hof;
pub mod io;
pub mod list;
pub mod math;
pub mod string;
pub mod system;

use super::registry::BuiltinRegistry;

/// Register all builtins from all category modules.
pub fn register_all(reg: &mut BuiltinRegistry) {
    io::register(reg);
    convert::register(reg);
    string::register(reg);
    math::register(reg);
    list::register(reg);
    hof::register(reg);
    system::register(reg);
}
