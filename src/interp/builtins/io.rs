use super::*;

impl<'a> Interpreter<'a> {
    // === I/O ===
    pub(crate) fn builtin_println(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
        println!("{}", parts.join(" "));
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_print(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
        print!("{}", parts.join(" "));
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_input(&mut self, _args: Vec<Value>) -> Result<Value, InterpError> {
        use std::io::{self, BufRead};
        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line) {
            Ok(_) => {
                if line.ends_with('\n') {
                    line.pop();
                }
                if line.ends_with('\r') {
                    line.pop();
                }
                Ok(Value::Variant("Ok".into(), vec![Value::String(line)]))
            }
            Err(e) => Ok(Value::Variant(
                "Err".into(),
                vec![Value::String(format!("input error: {}", e))],
            )),
        }
    }

    // === Assertions ===
    pub(crate) fn builtin_assert(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.is_empty() || args.len() > 2 {
            return Err(InterpError::new(
                "assert expects 1 or 2 arguments (condition, optional message)",
            ));
        }
        let msg = if args.len() == 2 {
            match &args[1] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            }
        } else {
            format!("{}", args[0])
        };
        if !is_truthy(&args[0]) {
            return Err(InterpError::new(format!("assertion failed: {}", msg)));
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_assert_eq(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("assert_eq expects 2 arguments"));
        }
        if !values_equal(&args[0], &args[1]) {
            return Err(InterpError::new(format!(
                "assertion failed: {} != {}",
                args[0], args[1]
            )));
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_assert_ne(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("assert_ne expects 2 arguments"));
        }
        if values_equal(&args[0], &args[1]) {
            return Err(InterpError::new(format!(
                "assertion failed: {} == {}",
                args[0], args[1]
            )));
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_assert_approx_eq(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("assert_approx_eq expects 2 arguments"));
        }
        match (&args[0], &args[1]) {
            (Value::Float(a), Value::Float(b)) => {
                if (a - b).abs() > f64::EPSILON {
                    return Err(InterpError::new(format!(
                        "assertion failed: {} != {} (difference: {})",
                        a,
                        b,
                        (a - b).abs()
                    )));
                }
                Ok(Value::Unit)
            }
            (Value::Int(a), Value::Int(b)) => {
                if a != b {
                    return Err(InterpError::new(format!(
                        "assertion failed: {} != {}",
                        a, b
                    )));
                }
                Ok(Value::Unit)
            }
            _ => {
                if !values_equal(&args[0], &args[1]) {
                    return Err(InterpError::new(format!(
                        "assertion failed: {} != {}",
                        args[0], args[1]
                    )));
                }
                Ok(Value::Unit)
            }
        }
    }
    // === File I/O ===
    pub(crate) fn builtin_read_file(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("read_file expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => match std::fs::read_to_string(path) {
                Ok(content) => Ok(Value::Variant("Ok".into(), vec![Value::String(content)])),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!("read_file error: {}", e))],
                )),
            },
            _ => Err(InterpError::new("read_file expects a string path")),
        }
    }

    pub(crate) fn builtin_write_file(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "write_file expects 2 arguments (path, content)",
            ));
        }
        match (&args[0], &args[1]) {
            (Value::String(path), Value::String(content)) => match std::fs::write(path, content) {
                Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                Err(e) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String(format!("write_file error: {}", e))],
                )),
            },
            _ => Err(InterpError::new("write_file expects (string, string)")),
        }
    }

    pub(crate) fn builtin_file_exists(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("file_exists expects 1 argument"));
        }
        match &args[0] {
            Value::String(path) => Ok(Value::Bool(std::path::Path::new(path).exists())),
            _ => Err(InterpError::new("file_exists expects a string path")),
        }
    }
    // === Directory & path operations ===

    pub(crate) fn builtin_listdir(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("listdir expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => match std::fs::read_dir(path) {
                Ok(rd) => {
                    let entries: Vec<Value> = rd
                        .filter_map(|e| e.ok())
                        .filter_map(|e| {
                            e.file_name().to_str().map(|s| Value::String(s.to_string()))
                        })
                        .collect();
                    Ok(Value::List(entries))
                }
                Err(_) => Ok(Value::List(vec![])),
            },
            _ => Err(InterpError::new("listdir expects a string path")),
        }
    }

    pub(crate) fn builtin_is_dir(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("is_dir expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => Ok(Value::Bool(std::path::Path::new(path).is_dir())),
            _ => Err(InterpError::new("is_dir expects a string path")),
        }
    }

    pub(crate) fn builtin_is_file(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("is_file expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => Ok(Value::Bool(std::path::Path::new(path).is_file())),
            _ => Err(InterpError::new("is_file expects a string path")),
        }
    }

    pub(crate) fn builtin_path_join(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("path_join expects 2 arguments (a, b)"));
        }
        match (&args[0], &args[1]) {
            (Value::String(a), Value::String(b)) => {
                let joined = std::path::Path::new(a)
                    .join(b)
                    .to_string_lossy()
                    .into_owned();
                Ok(Value::String(joined))
            }
            _ => Err(InterpError::new("path_join expects (string, string)")),
        }
    }

    pub(crate) fn builtin_path_ext(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("path_ext expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => {
                let ext = std::path::Path::new(path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                Ok(Value::String(ext))
            }
            _ => Err(InterpError::new("path_ext expects a string path")),
        }
    }

    pub(crate) fn builtin_path_basename(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("path_basename expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => {
                let name = std::path::Path::new(path)
                    .file_name()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                Ok(Value::String(name))
            }
            _ => Err(InterpError::new("path_basename expects a string path")),
        }
    }

    pub(crate) fn builtin_path_dirname(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("path_dirname expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => {
                let dir = std::path::Path::new(path)
                    .parent()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                Ok(Value::String(dir))
            }
            _ => Err(InterpError::new("path_dirname expects a string path")),
        }
    }

    pub(crate) fn builtin_walk_dir(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("walk_dir expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => {
                let mut results = Vec::new();
                Self::walk_dir_recursive_impl(path, &mut results);
                Ok(Value::List(
                    results.into_iter().map(Value::String).collect(),
                ))
            }
            _ => Err(InterpError::new("walk_dir expects a string path")),
        }
    }

    fn walk_dir_recursive_impl(dir: &str, results: &mut Vec<String>) {
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let path_str = path.to_string_lossy().into_owned();
            if path.is_dir() {
                Self::walk_dir_recursive_impl(&path_str, results);
            } else {
                results.push(path_str);
            }
        }
    }

    pub(crate) fn builtin_mkdir_p(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("mkdir_p expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => Ok(Value::Bool(std::fs::create_dir_all(path).is_ok())),
            _ => Err(InterpError::new("mkdir_p expects a string path")),
        }
    }

    pub(crate) fn builtin_remove_file(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("remove_file expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => Ok(Value::Bool(std::fs::remove_file(path).is_ok())),
            _ => Err(InterpError::new("remove_file expects a string path")),
        }
    }

    // === Process & advanced file operations ===

    pub(crate) fn builtin_exec(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("exec expects 1 argument (command)"));
        }
        match &args[0] {
            Value::String(cmd) => {
                // NOTE: `sh -c` enables shell builtins (exit, cd, etc.) and
                // pipelines, but also allows shell injection if `cmd` comes
                // from untrusted input. Use `exec_safe(args: List<string>)`
                // for safer execution without shell interpretation.
                let output = std::process::Command::new("sh").arg("-c").arg(cmd).output();
                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                        let exit_code = out.status.code().unwrap_or(-1);
                        let mut fields = std::collections::HashMap::new();
                        fields.insert("exit_code".to_string(), Value::Int(exit_code as i64));
                        fields.insert("stdout".to_string(), Value::String(stdout));
                        fields.insert("stderr".to_string(), Value::String(stderr));
                        Ok(Value::Record(Some("ExecResult".to_string()), fields))
                    }
                    Err(e) => {
                        let mut fields = std::collections::HashMap::new();
                        fields.insert("exit_code".to_string(), Value::Int(-1));
                        fields.insert("stdout".to_string(), Value::String(String::new()));
                        fields.insert(
                            "stderr".to_string(),
                            Value::String(format!("exec error: {}", e)),
                        );
                        Ok(Value::Record(Some("ExecResult".to_string()), fields))
                    }
                }
            }
            _ => Err(InterpError::new("exec expects a string command")),
        }
    }

    pub(crate) fn builtin_file_stat(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("file_stat expects 1 argument (path)"));
        }
        match &args[0] {
            Value::String(path) => {
                let mut fields = std::collections::HashMap::new();
                match std::fs::metadata(path) {
                    Ok(meta) => {
                        fields.insert("size".to_string(), Value::Int(meta.len() as i64));
                        let modified = meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        fields.insert("modified".to_string(), Value::Int(modified));
                        fields.insert("is_file".to_string(), Value::Bool(meta.is_file()));
                        fields.insert("is_dir".to_string(), Value::Bool(meta.is_dir()));
                    }
                    Err(_) => {
                        fields.insert("size".to_string(), Value::Int(-1));
                        fields.insert("modified".to_string(), Value::Int(0));
                        fields.insert("is_file".to_string(), Value::Bool(false));
                        fields.insert("is_dir".to_string(), Value::Bool(false));
                    }
                }
                Ok(Value::Record(Some("StatResult".to_string()), fields))
            }
            _ => Err(InterpError::new("file_stat expects a string path")),
        }
    }

    pub(crate) fn builtin_append_file(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "append_file expects 2 arguments (path, content)",
            ));
        }
        match (&args[0], &args[1]) {
            (Value::String(path), Value::String(content)) => {
                use std::io::Write;
                let ok = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut file| file.write_all(content.as_bytes()))
                    .is_ok();
                Ok(Value::Bool(ok))
            }
            _ => Err(InterpError::new("append_file expects (string, string)")),
        }
    }

    pub(crate) fn builtin_exec_pipe(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("exec_pipe expects 1 argument (command)"));
        }
        match &args[0] {
            Value::String(cmd) => {
                let output = std::process::Command::new("sh").arg("-c").arg(cmd).output();
                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                        Ok(Value::String(stdout))
                    }
                    Err(e) => Err(InterpError::new(format!("exec_pipe error: {}", e))),
                }
            }
            _ => Err(InterpError::new("exec_pipe expects a string command")),
        }
    }

    pub(crate) fn builtin_set_env(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("set_env expects 2 arguments (key, value)"));
        }
        match (&args[0], &args[1]) {
            (Value::String(key), Value::String(value)) => {
                // SAFETY: set_var is unsafe in Rust 2024+, but safe in practice for single-threaded use
                unsafe { std::env::set_var(key, value) };
                Ok(Value::Bool(true))
            }
            _ => Err(InterpError::new("set_env expects (string, string)")),
        }
    }

    // === Binary I/O & streaming line reading ===

    pub(crate) fn builtin_read_file_partial(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "read_file_partial expects 2 arguments (path, max_bytes)",
            ));
        }
        match (&args[0], &args[1]) {
            (Value::String(path), Value::Int(max)) => match std::fs::read(path) {
                Ok(bytes) => {
                    let limit = (*max).max(0) as usize;
                    let slice = if limit > 0 && bytes.len() > limit {
                        &bytes[..limit]
                    } else {
                        &bytes
                    };
                    let s = String::from_utf8_lossy(slice).to_string();
                    Ok(Value::String(s))
                }
                Err(e) => Err(InterpError::new(format!("read_file_partial: {}", e))),
            },
            _ => Err(InterpError::new("read_file_partial expects (string, int)")),
        }
    }

    pub(crate) fn builtin_read_file_bytes(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new(
                "read_file_bytes expects 1 argument (path)",
            ));
        }
        match &args[0] {
            Value::String(path) => match std::fs::read(path) {
                Ok(bytes) => {
                    let s = String::from_utf8_lossy(&bytes).to_string();
                    Ok(Value::String(s))
                }
                Err(e) => Err(InterpError::new(format!("read_file_bytes: {}", e))),
            },
            _ => Err(InterpError::new("read_file_bytes expects a string path")),
        }
    }

    pub(crate) fn builtin_write_file_bytes(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "write_file_bytes expects 2 arguments (path, data)",
            ));
        }
        match (&args[0], &args[1]) {
            (Value::String(path), Value::String(data)) => {
                match std::fs::write(path, data.as_bytes()) {
                    Ok(_) => Ok(Value::Bool(true)),
                    Err(e) => Err(InterpError::new(format!("write_file_bytes: {}", e))),
                }
            }
            _ => Err(InterpError::new(
                "write_file_bytes expects (string, string)",
            )),
        }
    }

    pub(crate) fn builtin_read_lines_each(
        &mut self,
        args: Vec<Value>,
    ) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "read_lines_each expects 2 arguments (path, callback)",
            ));
        }
        match (&args[0], &args[1]) {
            (Value::String(path), callback) => {
                use std::io::BufRead;
                let file = std::fs::File::open(path)
                    .map_err(|e| InterpError::new(format!("read_lines_each: {}", e)))?;
                let reader = std::io::BufReader::new(file);
                let mut count: i64 = 0;
                for line_result in reader.lines() {
                    match line_result {
                        Ok(line) => {
                            if self.early_return.is_some() {
                                break;
                            }
                            match callback {
                                Value::Closure {
                                    params,
                                    body,
                                    captured,
                                    ..
                                } => {
                                    if !params.is_empty() {
                                        self.push_scope();
                                        for (n, v) in captured {
                                            if let Err(e) = self.bind(n, v.clone()) {
                                                self.pop_scope();
                                                return Err(e);
                                            }
                                        }
                                        if let Err(e) =
                                            self.bind(&params[0].name, Value::String(line))
                                        {
                                            self.pop_scope();
                                            return Err(e);
                                        }
                                        let _ = self.eval_block(body);
                                        self.pop_scope();
                                    }
                                }
                                _ => {
                                    return Err(InterpError::new(
                                        "read_lines_each expects a closure as second argument",
                                    ));
                                }
                            }
                            if self.early_return.is_some() {
                                break;
                            }
                            count += 1;
                        }
                        Err(_) => break,
                    }
                }
                Ok(Value::Int(count))
            }
            _ => Err(InterpError::new(
                "read_lines_each expects (string, closure)",
            )),
        }
    }

    pub(crate) fn builtin_read_lines_json(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new(
                "read_lines_json expects 1 argument (path)",
            ));
        }
        match &args[0] {
            Value::String(path) => {
                use std::io::BufRead;
                let file = std::fs::File::open(path)
                    .map_err(|e| InterpError::new(format!("read_lines_json: {}", e)))?;
                let reader = std::io::BufReader::new(file);
                let mut result = String::from("[");
                let mut first = true;
                for line in reader.lines().map_while(Result::ok) {
                    if !first {
                        result.push(',');
                    }
                    first = false;
                    result.push('"');
                    for ch in line.chars() {
                        match ch {
                            '"' => result.push_str("\\\""),
                            '\\' => result.push_str("\\\\"),
                            '\n' => result.push_str("\\n"),
                            '\r' => result.push_str("\\r"),
                            '\t' => result.push_str("\\t"),
                            c if c < '\x20' => {
                                result.push_str(&format!("\\u{:04x}", c as u32));
                            }
                            c => result.push(c),
                        }
                    }
                    result.push('"');
                }
                result.push(']');
                Ok(Value::String(result))
            }
            _ => Err(InterpError::new("read_lines_json expects a string path")),
        }
    }

    // === Crypto operations ===

    pub(crate) fn builtin_sha256(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("sha256 expects 1 argument"));
        }
        match &args[0] {
            Value::String(data) => {
                let hash = crate::runtime::sha256_bytes(data.as_bytes());
                let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
                Ok(Value::String(hex))
            }
            _ => Err(InterpError::new("sha256 expects a string")),
        }
    }

    pub(crate) fn builtin_base64_encode(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("base64_encode expects 1 argument"));
        }
        match &args[0] {
            Value::String(data) => {
                let encoded = crate::runtime::base64_encode_bytes(data.as_bytes());
                Ok(Value::String(encoded))
            }
            _ => Err(InterpError::new("base64_encode expects a string")),
        }
    }

    pub(crate) fn builtin_base64_decode(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("base64_decode expects 1 argument"));
        }
        match &args[0] {
            Value::String(data) => match crate::runtime::base64_decode_str(data) {
                Ok(decoded) => Ok(Value::Variant("Ok".into(), vec![Value::String(decoded)])),
                Err(_) => Ok(Value::Variant(
                    "Err".into(),
                    vec![Value::String("invalid base64".to_string())],
                )),
            },
            _ => Err(InterpError::new("base64_decode expects a string")),
        }
    }

    // === I/O (stderr) ===
    pub(crate) fn builtin_eprintln(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
        eprintln!("{}", parts.join(" "));
        Ok(Value::Unit)
    }
}
