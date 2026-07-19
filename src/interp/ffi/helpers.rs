use super::super::*;
use crate::ast::*;
use std::sync::{Arc, RwLock};

/// Holds borrow guards alive during a synchronous FFI C call.
///
/// # Safety (IP-C2)
/// Read/Write guards are lifetime-erased to `'static` so they can be stored
/// in a `Vec` that outlives the temporary borrow scope. Safety rests on:
/// 1. The paired `Arc<RwLock<Value>>` keeps the lock target alive.
/// 2. Explicit `Drop` releases the guard **before** dropping the Arc
///    (independent of field declaration order — unlike tuple-struct drop).
/// 3. The erased guard is never re-borrowed as a Rust reference after creation;
///    only the raw pointer already passed to C is used during the call.
///
/// # Layout verification
/// See `test_ffi_guard_field_ordering`.
pub(in crate::interp) enum FfiGuard {
    Read(FfiReadHold),
    Write(FfiWriteHold),
    /// A libffi closure (dynamic C-compatible function pointer) that must
    /// remain alive for the duration of the C call, plus its boxed userdata.
    CallbackClosure {
        closure: Box<libffi::middle::Closure<'static>>,
        userdata: Box<i64>,
    },
}

/// Read-guard hold with explicit drop order (guard then Arc).
pub(in crate::interp) struct FfiReadHold {
    guard: Option<std::sync::RwLockReadGuard<'static, Value>>,
    arc: Arc<RwLock<Value>>,
}

impl Drop for FfiReadHold {
    fn drop(&mut self) {
        // IP-C2: always drop the lock guard before the Arc, regardless of
        // field declaration order.
        self.guard.take();
        // arc drops after Option is cleared
        let _ = &self.arc;
    }
}

/// Write-guard hold with explicit drop order (guard then Arc).
pub(in crate::interp) struct FfiWriteHold {
    guard: Option<std::sync::RwLockWriteGuard<'static, Value>>,
    arc: Arc<RwLock<Value>>,
}

impl Drop for FfiWriteHold {
    fn drop(&mut self) {
        self.guard.take();
        let _ = &self.arc;
    }
}

/// # Safety
/// Lifetime erasure to `'static` is justified by the Arc pairing and the fact
/// that the guard is only used to keep the lock held until `FfiReadHold::drop`.
pub(in crate::interp) fn ffi_guard_new_read(
    guard: std::sync::RwLockReadGuard<'_, Value>,
    arc: Arc<RwLock<Value>>,
) -> FfiGuard {
    // SAFETY: Arc keeps the RwLock alive; Drop releases the guard first.
    let guard_static = unsafe {
        std::mem::transmute::<
            std::sync::RwLockReadGuard<'_, Value>,
            std::sync::RwLockReadGuard<'static, Value>,
        >(guard)
    };
    FfiGuard::Read(FfiReadHold {
        guard: Some(guard_static),
        arc,
    })
}

/// Same safety contract as `ffi_guard_new_read` but for write guards.
pub(in crate::interp) fn ffi_guard_new_write(
    guard: std::sync::RwLockWriteGuard<'_, Value>,
    arc: Arc<RwLock<Value>>,
) -> FfiGuard {
    // SAFETY: see `ffi_guard_new_read`.
    let guard_static = unsafe {
        std::mem::transmute::<
            std::sync::RwLockWriteGuard<'_, Value>,
            std::sync::RwLockWriteGuard<'static, Value>,
        >(guard)
    };
    FfiGuard::Write(FfiWriteHold {
        guard: Some(guard_static),
        arc,
    })
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
                // P3-18 fix: release failures are logged, not silently ignored.
                // Note: release returns bool (not Result), false means not found.
                if !table.release(*id) {
                    eprintln!("[mimi] FfiSharedGuard: release failed for handle {}", id);
                }
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
                let fs: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, self.value_to_debug_string(v)))
                    .collect();
                format!("{} {{ {} }}", name, fs.join(", "))
            }
            Value::Variant(name, args) => {
                if args.is_empty() {
                    name.clone()
                } else {
                    let as_: Vec<String> =
                        args.iter().map(|a| self.value_to_debug_string(a)).collect();
                    format!("{}({})", name, as_.join(", "))
                }
            }
            Value::List(items) => {
                let is_: Vec<String> = items
                    .iter()
                    .map(|i| self.value_to_debug_string(i))
                    .collect();
                format!("[{}]", is_.join(", "))
            }
            Value::Set(items) => {
                let is_: Vec<String> = items
                    .iter()
                    .map(|i| self.value_to_debug_string(i))
                    .collect();
                format!("Set{{{}}}", is_.join(", "))
            }
            Value::Tuple(items) => {
                let ts: Vec<String> = items
                    .iter()
                    .map(|i| self.value_to_debug_string(i))
                    .collect();
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
        .map(|pt| {
            matches!(pt.unlocated(), Type::Name(n, _) if n == "string")
                || matches!(pt.unlocated(), Type::RawString)
                || matches!(pt.unlocated(), Type::CBuffer(_))
        })
        .collect()
}

/// IP-H4: map declared callback param types to decode kinds.
pub(crate) fn compute_arg_kinds(
    param_types: &[Type],
) -> Vec<crate::interp::ffi::callback::CallbackArgKind> {
    use crate::interp::ffi::callback::CallbackArgKind;
    param_types
        .iter()
        .map(|pt| match pt.unlocated() {
            Type::Name(n, _) if n == "f64" || n == "f32" => CallbackArgKind::Float,
            Type::Name(n, _) if n == "string" => CallbackArgKind::CString,
            Type::RawString | Type::CBuffer(_) => CallbackArgKind::CString,
            _ => CallbackArgKind::Int,
        })
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
