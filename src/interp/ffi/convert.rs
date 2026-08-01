use super::super::*;
use crate::ffi::Errno;

/// JSON <-> Value conversion delegates.
///
/// 0.33 (Phase D/0.33.20): the FFI argument/return conversion logic moved to
/// `ffi_runtime.rs` (shared with the bytecode VM). These two delegates are
/// kept on `Interpreter` because the tree-walker's to_json/from_json builtins
/// and turbofish from_json::<T> evaluation still call them.

impl<'a> Interpreter<'a> {
    pub(crate) fn value_to_json(&self, v: &Value) -> Result<serde_json::Value, Errno> {
        self.ffi_runtime.value_to_json(v)
    }

    pub(in crate::interp) fn json_to_value(&self, jv: &serde_json::Value) -> Value {
        self.ffi_runtime.json_to_value(jv)
    }
}
