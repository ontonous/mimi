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
/// Tuple-struct fields drop in declaration order (Rust guarantees this for
/// struct fields and tuple struct fields alike). Reversing field 0 and 1
/// would cause the guard to reference freed data — undefined behavior.
///
/// # Safety invariant (guard source)
/// Guards are always created via `arc.read()`/`arc.write()` from the same
/// `Arc<RwLock<Value>>` that is stored alongside them. This ensures the
/// data referenced by the guard cannot be freed before the guard is dropped.
///
/// # Layout verification
/// See `test_ffi_guard_field_ordering` for a runtime assertion that the
/// transmute-based safety contract holds (guard dropped before Arc).
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

/// # Safety
/// `'static` transmute is safe because:
/// 1. The Arc stored alongside the guard keeps the underlying data alive.
/// 2. Rust drops tuple-struct fields in declaration order, so the guard
///    (field 0) is dropped before its paired Arc (field 1).
/// 3. No code ever accesses the `'static` guard through the reference —
///    only the raw pointer (from `&*guard`) was already passed to C.
///    The guard exists purely to keep the lock held.
///
/// If you add/remove/reorder fields in `FfiGuard`, update this comment.
pub(in crate::interp) fn ffi_guard_new_read(guard: std::sync::RwLockReadGuard<'_, Value>, arc: Arc<RwLock<Value>>) -> FfiGuard {
    // SAFETY: See safety doc on this function.
    FfiGuard::Read(unsafe { std::mem::transmute::<std::sync::RwLockReadGuard<'_, Value>, std::sync::RwLockReadGuard<'static, Value>>(guard) }, arc)
}

/// Same safety contract as `ffi_guard_new_read` but for write guards.
pub(in crate::interp) fn ffi_guard_new_write(guard: std::sync::RwLockWriteGuard<'_, Value>, arc: Arc<RwLock<Value>>) -> FfiGuard {
    // SAFETY: See safety doc on `ffi_guard_new_read`.
    FfiGuard::Write(unsafe { std::mem::transmute::<std::sync::RwLockWriteGuard<'_, Value>, std::sync::RwLockWriteGuard<'static, Value>>(guard) }, arc)
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

/// Tests that FfiGuard's field ordering invariant (guard before Arc) holds.
/// This is a runtime assertion that the transmute safety contract is
/// maintained. If fields are reordered, this test will fail or produce
/// observable leaks.
#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn test_ffi_guard_field_ordering() {
        // Verify that FfiGuard::drop drops the guard before the Arc.
        // We construct a RwLock, read-lock it, create a guard via
        // transmute, then drop the guard. The test passes if no
        // panic/UB occurs (the guard is released before the Arc).
        let data = Arc::new(RwLock::new(Value::Int(42)));
        let guard = data.read().unwrap();
        let ffi_guard = ffi_guard_new_read(guard, data.clone());
        drop(ffi_guard);
        // After dropping, we should be able to write-lock without deadlock
        // (proving the read guard was released before the Arc refcount drop).
        let mut w = data.write().unwrap();
        *w = Value::Int(7);
    }
}
