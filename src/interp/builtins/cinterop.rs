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
                // IP-H1: range check + mincore mapped-page probe + bounded NUL scan.
                // Still TOCTOU vs concurrent munmap, but avoids SIGSEGV on
                // clearly unmapped addresses (same strategy as runtime RT-H4).
                let addr = *ptr as usize;
                if !(0x10000usize..usize::MAX - 4096).contains(&addr) {
                    return Err(InterpError::new(format!(
                        "c_str_to_string: invalid pointer {:#x} (out of valid range)",
                        ptr
                    )));
                }
                let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
                let page_size = if page_size == 0 { 4096 } else { page_size };
                let page_start = (addr / page_size) * page_size;
                let mut mvec: u8 = 0;
                let mapped = unsafe {
                    libc::mincore(page_start as *mut std::ffi::c_void, page_size, &mut mvec)
                };
                if mapped != 0 {
                    return Err(InterpError::new(format!(
                        "c_str_to_string: pointer {:#x} is not mapped",
                        ptr
                    )));
                }
                let page_offset = addr - page_start;
                let max_scan = page_size.saturating_sub(page_offset).min(64 * 1024);
                let base = *ptr as *const u8;
                let mut len = 0usize;
                // SAFETY: mincore confirmed page is mapped; scan stays in-page.
                unsafe {
                    while len < max_scan {
                        if *base.add(len) == 0 {
                            let slice = std::slice::from_raw_parts(base, len);
                            return Ok(Value::String(
                                String::from_utf8_lossy(slice).into_owned(),
                            ));
                        }
                        len += 1;
                    }
                }
                Err(InterpError::new(format!(
                    "c_str_to_string: no NUL terminator within {} bytes at {:#x}",
                    max_scan, ptr
                )))
            }
            other => Err(InterpError::new(format!(
                "c_str_to_string: argument must be a pointer (int), found {}",
                super::value::type_name(other)
            ))),
        }
    }
}
