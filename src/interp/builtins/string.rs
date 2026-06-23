use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn builtin_to_string(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("to_string expects 1 argument"));
        }
        Ok(Value::String(args[0].to_string()))
    }
    // === String operations ===
    pub(crate) fn builtin_str_char_at(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_char_at expects 2 arguments (string, index)")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::Int(idx)) => {
                let i = *idx as usize;
                s.chars().nth(i)
                    .map(|c| Value::String(c.to_string()))
                    .ok_or_else(|| InterpError::new(format!("str_char_at: index {} out of bounds (len {})", i, s.chars().count())))
            }
            _ => Err(InterpError::new("str_char_at expects (string, int)")),
        }
    }

    pub(crate) fn builtin_str_substring(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 { return Err(InterpError::new("str_substring expects 3 arguments (string, start, end)")); }
        match (&args[0], &args[1], &args[2]) {
            (Value::String(s), Value::Int(start), Value::Int(end)) => {
                let chars: Vec<char> = s.chars().collect();
                let s_idx = (*start as usize).min(chars.len());
                let e_idx = (*end as usize).min(chars.len());
                if s_idx > e_idx {
                    return Err(InterpError::new("str_substring: start > end"));
                }
                Ok(Value::String(chars[s_idx..e_idx].iter().collect()))
            }
            _ => Err(InterpError::new("str_substring expects (string, int, int)")),
        }
    }

    pub(crate) fn builtin_str_parse_int(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_parse_int expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(s.trim().parse::<i64>()
                .map(|n| Value::Tuple(vec![Value::Bool(true), Value::Int(n)]))
                .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Int(0)]))),
            _ => Err(InterpError::new("str_parse_int expects a string")),
        }
    }

    pub(crate) fn builtin_str_parse_float(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_parse_float expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(s.trim().parse::<f64>()
                .map(|n| Value::Tuple(vec![Value::Bool(true), Value::Float(n)]))
                .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Float(0.0)]))),
            _ => Err(InterpError::new("str_parse_float expects a string")),
        }
    }

    pub(crate) fn builtin_str_split(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_split expects 2 arguments (string, delimiter)")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(delimiter)) => {
                let mut parts = Vec::new();
                for p in s.split(delimiter.as_str()) {
                    parts.push(Value::String(p.to_string()));
                }
                Ok(Value::List(parts))
            }
            _ => Err(InterpError::new("str_split expects (string, string)")),
        }
    }

    pub(crate) fn builtin_str_join(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_join expects 2 arguments (list, separator)")); }
        match (&args[0], &args[1]) {
            (Value::List(parts), Value::String(sep)) => {
                let mut strings = Vec::new();
                for p in parts {
                    match p {
                        Value::String(s) => strings.push(s.clone()),
                        _ => return Err(InterpError::new("str_join: list elements must be strings")),
                    }
                }
                Ok(Value::String(strings.join(sep)))
            }
            _ => Err(InterpError::new("str_join expects (list, string)")),
        }
    }

    pub(crate) fn builtin_str_trim(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_trim expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.trim().to_string())),
            _ => Err(InterpError::new("str_trim expects a string")),
        }
    }

    pub(crate) fn builtin_str_starts_with(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_starts_with expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(prefix)) => {
                Ok(Value::Bool(s.starts_with(prefix.as_str())))
            }
            _ => Err(InterpError::new("str_starts_with expects (string, string)")),
        }
    }

    pub(crate) fn builtin_str_ends_with(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_ends_with expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(suffix)) => {
                Ok(Value::Bool(s.ends_with(suffix.as_str())))
            }
            _ => Err(InterpError::new("str_ends_with expects (string, string)")),
        }
    }

    pub(crate) fn builtin_str_replace(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 { return Err(InterpError::new("str_replace expects 3 arguments")); }
        match (&args[0], &args[1], &args[2]) {
            (Value::String(s), Value::String(from), Value::String(to)) => {
                Ok(Value::String(s.replace(from.as_str(), to.as_str())))
            }
            _ => Err(InterpError::new("str_replace expects (string, string, string)")),
        }
    }

    pub(crate) fn builtin_str_to_upper(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_to_upper expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.to_uppercase())),
            _ => Err(InterpError::new("str_to_upper expects a string")),
        }
    }

    pub(crate) fn builtin_str_to_lower(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("str_to_lower expects 1 argument")); }
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.to_lowercase())),
            _ => Err(InterpError::new("str_to_lower expects a string")),
        }
    }

    pub(crate) fn builtin_str_repeat(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_repeat expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::Int(n)) => {
                if *n < 0 { return Err(InterpError::new("str_repeat: count must be non-negative")); }
                Ok(Value::String(s.repeat(*n as usize)))
            }
            _ => Err(InterpError::new("str_repeat expects (string, int)")),
        }
    }

    pub(crate) fn builtin_str_contains(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_contains expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(sub)) => {
                Ok(Value::Bool(s.contains(sub.as_str())))
            }
            _ => Err(InterpError::new("str_contains expects (string, string)")),
        }
    }

    pub(crate) fn builtin_regex_match(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("regex_match expects 2 arguments (text, pattern)")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(pattern)) => {
                let re = regex::Regex::new(pattern)
                    .map_err(|e| InterpError::new(format!("regex_match: invalid pattern '{}': {}", pattern, e)))?;
                Ok(Value::Bool(re.is_match(s)))
            }
            _ => Err(InterpError::new("regex_match expects (string, string)")),
        }
    }

    pub(crate) fn builtin_regex_find(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("regex_find expects 2 arguments (text, pattern)")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(pattern)) => {
                let re = regex::Regex::new(pattern)
                    .map_err(|e| InterpError::new(format!("regex_find: invalid pattern '{}': {}", pattern, e)))?;
                match re.find(s) {
                    Some(m) => Ok(Value::String(m.as_str().to_string())),
                    None => Ok(Value::String(String::new())),
                }
            }
            _ => Err(InterpError::new("regex_find expects (string, string)")),
        }
    }

    pub(crate) fn builtin_regex_replace(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 { return Err(InterpError::new("regex_replace expects 3 arguments (text, pattern, replacement)")); }
        match (&args[0], &args[1], &args[2]) {
            (Value::String(s), Value::String(pattern), Value::String(replacement)) => {
                let re = regex::Regex::new(pattern)
                    .map_err(|e| InterpError::new(format!("regex_replace: invalid pattern '{}': {}", pattern, e)))?;
                Ok(Value::String(re.replace_all(s, replacement.as_str()).to_string()))
            }
            _ => Err(InterpError::new("regex_replace expects (string, string, string)")),
        }
    }

    pub(crate) fn builtin_char_code(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("char_code expects 2 arguments (string, index)")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::Int(idx)) => {
                let i = *idx as usize;
                s.chars().nth(i)
                    .map(|c| Value::Int(c as i64))
                    .ok_or_else(|| InterpError::new(format!("char_code: index {} out of bounds (len {})", i, s.chars().count())))
            }
            _ => Err(InterpError::new("char_code expects (string, int)")),
        }
    }

    pub(crate) fn builtin_chr(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 { return Err(InterpError::new("chr expects 1 argument (code point)")); }
        match &args[0] {
            Value::Int(code) => {
                if *code < 0 || *code > 0x10FFFF {
                    return Err(InterpError::new(format!("chr: code point {} out of range (0-0x10FFFF)", code)));
                }
                match char::from_u32(*code as u32) {
                    Some(c) => Ok(Value::String(c.to_string())),
                    None => Err(InterpError::new(format!("chr: invalid Unicode code point {}", code))),
                }
            }
            _ => Err(InterpError::new("chr expects an integer")),
        }
    }

    pub(crate) fn builtin_str_index_of(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 { return Err(InterpError::new("str_index_of expects 2 arguments")); }
        match (&args[0], &args[1]) {
            (Value::String(s), Value::String(sub)) => {
                match s.find(sub.as_str()) {
                    Some(idx) => Ok(Value::Tuple(vec![Value::Bool(true), Value::Int(idx as i64)])),
                    None => Ok(Value::Tuple(vec![Value::Bool(false), Value::Int(-1)])),
                }
            }
            _ => Err(InterpError::new("str_index_of expects (string, string)")),
        }
    }
}
