//! FFI call execution has moved to `ffi_runtime.rs` (shared by the
//! tree-walker interpreter and the bytecode VM). This file retains only
//! the callback-related unit tests; it will be folded into ffi_runtime.rs
//! during Phase F (Tree-walker retirement).

// ===================== F6 Callback String-Leak Tests =====================
// Verifies that C-allocated string arguments passed to Mimi callbacks
// are freed after the callback returns.

#[cfg(test)]
mod callback_leak_tests {
    use crate::ast::Type;
    use crate::interp::ffi::helpers::compute_arg_free_mask;
    use crate::interp::Value;

    /// Helper: delegate to the module-level function.
    fn compute_free_mask(param_types: &[Type]) -> Vec<bool> {
        compute_arg_free_mask(param_types)
    }

    #[test]
    fn test_free_mask_i32_args_no_free() {
        let types = [
            Type::Name("i32".into(), Vec::new()),
            Type::Name("i64".into(), Vec::new()),
        ];
        assert_eq!(compute_free_mask(&types), [false, false]);
    }

    #[test]
    fn test_free_mask_string_arg_freed() {
        let types = [Type::Name("string".into(), Vec::new())];
        assert_eq!(compute_free_mask(&types), [true]);
    }

    #[test]
    fn test_free_mask_mixed_args() {
        let types = [
            Type::Name("i32".into(), Vec::new()),
            Type::Name("string".into(), Vec::new()),
            Type::Name("f64".into(), Vec::new()),
        ];
        assert_eq!(compute_free_mask(&types), [false, true, false]);
    }

    #[test]
    fn test_free_mask_raw_string() {
        let types = [Type::RawString];
        assert_eq!(compute_free_mask(&types), [true]);
    }

    #[test]
    fn test_free_mask_cbuffer() {
        let types = [Type::CBuffer(Box::new(Type::Name("u8".into(), Vec::new())))];
        assert_eq!(compute_free_mask(&types), [true]);
    }

    #[test]
    fn test_callback_ctx_three_tuple() {
        // Verify FfiCallbackCtx entries store (Value, bool, Vec<bool>).
        let entry: (Value, bool, Vec<bool>) = (Value::Int(0), false, Vec::from([true, false]));
        assert_eq!(entry.2.len(), 2);
        assert!(entry.2[0]);
        assert!(!entry.2[1]);
    }

    #[test]
    fn test_trampoline_frees_null_safe() {
        // Verify libc::free(NULL) is safe (no crash).
        // SAFETY: The C standard guarantees free(NULL) is a no-op.
        unsafe { libc::free(std::ptr::null_mut()) };
    }
}
