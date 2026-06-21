use super::super::*;
use crate::ast::*;
use std::sync::{Arc, RwLock};

/// Holds borrow guards alive during a synchronous FFI C call.
/// Each guard variant pairs the lock guard (dropped first) with the `Arc`
/// that keeps the underlying data alive (dropped second).
///
/// # Safety invariant (field ordering)
/// The `Arc` is stored AFTER the guard so that on drop, the guard is
/// released first (unlocking the RwLock) before the Arc potentially frees it.
/// Do NOT reorder these fields without auditing all transmute sites.
///
/// # Safety invariant (guard source)
/// Guards are always created via `arc.read()`/`arc.write()` from the same
/// `Arc<RwLock<Value>>` that is stored alongside them. This ensures the
/// data referenced by the guard cannot be freed before the guard is dropped.
pub(in crate::interp) enum FfiGuard {
    Read(std::sync::RwLockReadGuard<'static, Value>, Arc<RwLock<Value>>),
    Write(std::sync::RwLockWriteGuard<'static, Value>, Arc<RwLock<Value>>),
    /// A libffi closure (dynamic C-compatible function pointer) that must
    /// remain alive for the duration of the C call, plus its boxed userdata.
    CallbackClosure {
        closure: Box<libffi::middle::Closure<'static>>,
        userdata: Box<i64>,
    },
}

/// RAII guard that tracks shared handles created during an FFI call and
/// releases them from the per-thread SHARED_TABLE on drop (all exit paths).
pub(in crate::interp) struct FfiSharedGuard {
    handles: Vec<i64>,
}

impl FfiSharedGuard {
    pub(in crate::interp) fn new() -> Self {
        Self {
            handles: Vec::new(),
        }
    }

    pub(in crate::interp) fn register(&mut self, handle_id: i64) {
        self.handles.push(handle_id);
    }
}

impl Drop for FfiSharedGuard {
    fn drop(&mut self) {
        crate::ffi::runtime::with_shared_table(|table| {
            for id in &self.handles {
                let _ = table.release(*id);
            }
        });
    }
}

impl<'a> Interpreter<'a> {
    pub(crate) fn value_to_debug_string(&self, v: &Value) -> String {
        match v {
            Value::Int(n) => format!("{}", n),
            Value::Float(f) => format!("{}", f),
            Value::Bool(b) => format!("{}", b),
            Value::String(s) => format!("\"{}\"", s),
            Value::Record(type_name, fields) => {
                let name = type_name.as_deref().unwrap_or("Record");
                let fs: Vec<String> = fields.iter()
                    .map(|(k, v)| format!("{}: {}", k, self.value_to_debug_string(v)))
                    .collect();
                format!("{} {{ {} }}", name, fs.join(", "))
            }
            Value::Variant(name, args) => {
                if args.is_empty() {
                    name.clone()
                } else {
                    let as_: Vec<String> = args.iter().map(|a| self.value_to_debug_string(a)).collect();
                    format!("{}({})", name, as_.join(", "))
                }
            }
            Value::List(items) => {
                let is_: Vec<String> = items.iter().map(|i| self.value_to_debug_string(i)).collect();
                format!("[{}]", is_.join(", "))
            }
            Value::Tuple(items) => {
                let ts: Vec<String> = items.iter().map(|i| self.value_to_debug_string(i)).collect();
                format!("({})", ts.join(", "))
            }
            Value::Unit => "unit".to_string(),
            _ => format!("{:?}", v),
        }
    }

    pub(crate) fn values_equal(&self, a: &Value, b: &Value) -> bool {
        // Delegate to the canonical implementation in value.rs to avoid duplication.
        // The canonical version supports more Value variants (Shared, Ref, DynTrait, etc.)
        // and uses relative epsilon for float comparison.
        crate::interp::value::values_equal(a, b)
    }
}

/// Compute which callback parameters are C-allocated strings that Mimi must free.
/// `true` for `string`, `RawString`, and `CBuffer` types.
pub(crate) fn compute_arg_free_mask(param_types: &[Type]) -> Vec<bool> {
    param_types
        .iter()
        .map(|pt| matches!(pt, Type::Name(n, _) if n == "string")
            || matches!(pt, Type::RawString)
            || matches!(pt, Type::CBuffer(_)))
        .collect()
}
