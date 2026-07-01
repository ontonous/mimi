use super::*;

impl<'a> Interpreter<'a> {
    // === C interop ===
    pub(crate) fn builtin_str_to_c_str(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("str_to_c_str expects 1 argument (string)"));
        }
        match &args[0] {
            Value::String(s) => {
                // Return a tuple (pointer, length) for C compatibility
                // The pointer is the raw pointer to the CString data
                let c_str = std::ffi::CString::new(s.as_str())
                    .map_err(|e| InterpError::new(format!("failed to create C string: {}", e)))?;
                let ptr = c_str.into_raw() as i64;
                Ok(Value::Tuple(vec![
                    Value::Int(ptr),
                    Value::Int(s.len() as i64),
                ]))
            }
            other => Err(InterpError::new(format!(
                "str_to_c_str: argument must be a string, found {}",
                super::value::type_name(other)
            ))),
        }
    }

    pub(crate) fn builtin_c_str_to_string(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new(
                "c_str_to_string expects 1 argument (pointer)",
            ));
        }
        match &args[0] {
            Value::Int(ptr) => {
                if *ptr == 0 {
                    return Ok(Value::String(String::new()));
                }
                // NOTE: catch_unwind only catches Rust panics, NOT SIGSEGV (signals).
                // In release mode, an invalid pointer dereference will crash the process.
                // This guard catches only Rust-level panics (e.g. overflow in CStr::to_string_lossy)
                // but does NOT provide protection against truly invalid/unmapped pointers.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // First byte probe: this is best-effort — if the pointer is truly
                    // invalid (unmapped), the process WILL crash regardless of catch_unwind.
                    let ptr_raw = *ptr as *const u8;
                    // SAFETY: pointer was validated non-null; probe is best-effort inside catch_unwind.
                    let _first_byte = unsafe { *ptr_raw };
                    // SAFETY: pointer was validated non-null and probed before conversion.
                    let c_str = unsafe { std::ffi::CStr::from_ptr(*ptr as *const i8) };
                    Value::String(c_str.to_string_lossy().into_owned())
                }));
                match result {
                    Ok(v) => Ok(v),
                    Err(_) => Err(InterpError::new(format!(
                        "c_str_to_string: invalid pointer {:#x} (panic during string conversion)",
                        ptr
                    ))),
                }
            }
            other => Err(InterpError::new(format!(
                "c_str_to_string: argument must be a pointer (int), found {}",
                super::value::type_name(other)
            ))),
        }
    }
}
