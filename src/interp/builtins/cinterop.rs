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
                let ptr = c_str.as_ptr() as i64;
                self.cstring_registry.borrow_mut().push(c_str);
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
                // Reject clearly invalid pointer ranges. Valid heap pointers from
                // the Mimi runtime are >= 64KB and well below the top of user space.
                if !(0x10000usize..usize::MAX - 4096).contains(&(*ptr as usize)) {
                    return Err(InterpError::new(format!(
                        "c_str_to_string: invalid pointer {:#x} (out of valid range)",
                        ptr
                    )));
                }
                // SAFETY: pointer is non-null and in a valid heap range.
                // SIGSEGV is still possible if memory has been freed between the
                // range check and dereference (TOCTOU), but this is the best
                // we can do without signal handlers.
                let c_str = unsafe { std::ffi::CStr::from_ptr(*ptr as *const i8) };
                Ok(Value::String(c_str.to_string_lossy().into_owned()))
            }
            other => Err(InterpError::new(format!(
                "c_str_to_string: argument must be a pointer (int), found {}",
                super::value::type_name(other)
            ))),
        }
    }
}
