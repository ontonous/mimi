use super::*;
use crate::runtime::{mimi_lexer_tokenize, mimi_parse_source};
use std::ffi::CString;

impl<'a> Interpreter<'a> {
    // === MimiSpec runtime ===
    pub(crate) fn builtin_lexer(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("lexer expects 1 argument (source string)"));
        }
        match &args[0] {
            Value::String(source) => {
                let c_source = CString::new(source.as_str())
                    .map_err(|_| InterpError::new("lexer: source contains null bytes"))?;
                let result_ptr = mimi_lexer_tokenize(c_source.as_ptr());
                if result_ptr.is_null() {
                    return Ok(Value::String("[]".to_string()));
                }
                let result = unsafe { std::ffi::CStr::from_ptr(result_ptr) }
                    .to_string_lossy()
                    .into_owned();
                // SAFETY: result_ptr was just allocated by mimi_lexer_tokenize via alloc_c_string/malloc
                unsafe { libc::free(result_ptr as *mut libc::c_void) };
                Ok(Value::String(result))
            }
            _ => Err(InterpError::new("lexer expects a string source")),
        }
    }

    pub(crate) fn builtin_parse(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("parse expects 1 argument (source string)"));
        }
        match &args[0] {
            Value::String(source) => {
                let c_source = CString::new(source.as_str())
                    .map_err(|_| InterpError::new("parse: source contains null bytes"))?;
                let result_ptr = mimi_parse_source(c_source.as_ptr());
                if result_ptr.is_null() {
                    return Ok(Value::String(
                        r#"{"functions":[],"types":[],"imports":[],"has_main":false}"#.to_string(),
                    ));
                }
                // SAFETY: result_ptr was just allocated by mimi_parse_source via alloc_c_string/malloc
                let result = unsafe { std::ffi::CStr::from_ptr(result_ptr) }
                    .to_string_lossy()
                    .into_owned();
                unsafe { libc::free(result_ptr as *mut libc::c_void) };
                Ok(Value::String(result))
            }
            _ => Err(InterpError::new("parse expects a string source")),
        }
    }
}
