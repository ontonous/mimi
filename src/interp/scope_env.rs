use super::*;

/// Scope-level evaluation state.
///
/// Contains the mutable state needed to evaluate expressions within scopes:
/// variable bindings, mutability tracking, and the call stack for error context.
///
/// PERF (0.33): scope HashMaps are pooled and reused across push/pop cycles
/// to eliminate the malloc/free hotspot (perf: >50% in _int_malloc/_int_free
/// for recursive workloads like fib(25) with 242K calls).
#[derive(Debug)]
pub struct ScopeEnv {
    /// Stack of variable bindings (one HashMap per scope)
    pub env: Vec<HashMap<String, Value>>,
    /// Track which variables have been moved (for move semantics)
    pub moved_vars: Vec<HashMap<String, bool>>,
    /// Track which variables are mutable
    pub mut_vars: Vec<HashMap<String, bool>>,
    /// Call stack for error context (function names being executed)
    pub call_stack: Vec<String>,
    /// Pools of cleared HashMaps ready for reuse (avoids repeated alloc/free).
    env_pool: Vec<HashMap<String, Value>>,
    moved_pool: Vec<HashMap<String, bool>>,
    mut_pool: Vec<HashMap<String, bool>>,
}

impl ScopeEnv {
    pub fn new() -> Self {
        ScopeEnv {
            env: vec![HashMap::new()],
            moved_vars: vec![HashMap::new()],
            mut_vars: vec![HashMap::new()],
            call_stack: Vec::new(),
            env_pool: Vec::new(),
            moved_pool: Vec::new(),
            mut_pool: Vec::new(),
        }
    }

    /// Push a new scope level onto the env stack.
    pub fn push_scope(&mut self) {
        self.env.push(
            self.env_pool
                .pop()
                .unwrap_or_else(|| HashMap::with_capacity(8)),
        );
        self.moved_vars.push(
            self.moved_pool
                .pop()
                .unwrap_or_else(|| HashMap::with_capacity(8)),
        );
        self.mut_vars.push(
            self.mut_pool
                .pop()
                .unwrap_or_else(|| HashMap::with_capacity(8)),
        );
    }

    /// Pop the current scope level. HashMaps are cleared and pooled for reuse.
    pub fn pop_scope(&mut self) {
        if let Some(mut m) = self.env.pop() {
            m.clear();
            self.env_pool.push(m);
        }
        if let Some(mut m) = self.moved_vars.pop() {
            m.clear();
            self.moved_pool.push(m);
        }
        if let Some(mut m) = self.mut_vars.pop() {
            m.clear();
            self.mut_pool.push(m);
        }
    }

    /// Run a closure inside a freshly pushed scope, guaranteeing that the
    /// scope is popped even if the closure returns an error.
    pub fn with_scope<F, T>(&mut self, f: F) -> Result<T, InterpError>
    where
        F: FnOnce(&mut Self) -> Result<T, InterpError>,
    {
        self.push_scope();
        let result = f(self);
        self.pop_scope();
        result
    }

    /// Push a function name onto the call stack.
    pub fn push_call(&mut self, func_name: &str) {
        self.call_stack.push(func_name.to_string());
    }

    /// Pop the most recent call stack entry.
    pub fn pop_call(&mut self) {
        self.call_stack.pop();
    }

    /// Bind a value to a name in the current scope (immutable by default).
    pub fn bind(&mut self, name: &str, value: Value) -> Result<(), InterpError> {
        let env = self
            .env
            .last_mut()
            .ok_or("internal error: scope stack empty in bind")?;
        env.insert(name.into(), value);
        self.moved_vars
            .last_mut()
            .ok_or("internal error: scope stack empty in bind")?
            .insert(name.into(), false);
        // Default to immutable unless explicitly marked as mutable
        self.mut_vars
            .last_mut()
            .ok_or("internal error: scope stack empty in bind")?
            .entry(name.into())
            .or_insert(false);
        Ok(())
    }

    /// Bind a value to a name in the current scope (mutable).
    pub fn bind_mut(&mut self, name: &str, value: Value) -> Result<(), InterpError> {
        self.env
            .last_mut()
            .ok_or("internal error: scope stack empty in bind_mut")?
            .insert(name.into(), value);
        self.moved_vars
            .last_mut()
            .ok_or("internal error: scope stack empty in bind_mut")?
            .insert(name.into(), false);
        self.mut_vars
            .last_mut()
            .ok_or("internal error: scope stack empty in bind_mut")?
            .insert(name.into(), true);
        Ok(())
    }

    /// Look up a variable by name, searching from innermost to outermost scope.
    /// Returns None if the variable was moved or doesn't exist.
    pub fn lookup(&self, name: &str) -> Option<Value> {
        self.lookup_ref(name).cloned()
    }

    /// Look up a variable by name, returning a borrowed reference.
    /// Avoids the deep clone of `lookup()` for read-only access patterns
    /// (comparisons, type checks, pattern matching, field access).
    pub fn lookup_ref(&self, name: &str) -> Option<&Value> {
        for (scope, moved) in self.env.iter().zip(self.moved_vars.iter()).rev() {
            if let Some(v) = scope.get(name) {
                if moved.get(name).copied().unwrap_or(false) {
                    return None; // Treat moved vars as undefined
                }
                return Some(v);
            }
        }
        None
    }

    /// Remove and return a variable's value from the innermost scope that
    /// contains it. Replaces the slot with `Value::Unit` as a sentinel.
    /// Used by the take-before-eval optimization in `eval_assign` to avoid
    /// cloning heap-heavy values (String, List, Record) when the variable
    /// is being reassigned and read exactly once in the RHS.
    pub fn take(&mut self, name: &str) -> Option<Value> {
        for (scope, moved) in self.env.iter_mut().zip(self.moved_vars.iter()).rev() {
            if let Some(v) = scope.get_mut(name) {
                if moved.get(name).copied().unwrap_or(false) {
                    return None;
                }
                return Some(std::mem::replace(v, Value::Unit));
            }
        }
        None
    }

    /// Check if a variable is mutable.
    pub fn is_mutable(&self, name: &str) -> bool {
        for mut_scope in self.mut_vars.iter().rev() {
            if let Some(&is_mut) = mut_scope.get(name) {
                return is_mut;
            }
        }
        false
    }

    /// Check if a variable has been moved.
    pub fn is_moved(&self, name: &str) -> bool {
        for moved in self.moved_vars.iter().rev() {
            if let Some(&m) = moved.get(name) {
                return m;
            }
        }
        false
    }

    /// Mark a variable as moved.
    pub fn mark_moved(&mut self, name: &str) {
        for moved in self.moved_vars.iter_mut().rev() {
            if moved.contains_key(name) {
                moved.insert(name.into(), true);
                return;
            }
        }
    }

    /// Assign a value to an existing variable (mutable check enforced).
    pub fn assign(&mut self, name: &str, value: Value) -> Result<(), InterpError> {
        for (scope, moved) in self.env.iter_mut().zip(self.moved_vars.iter_mut()).rev() {
            if scope.contains_key(name) {
                // Check if variable is mutable
                for mut_scope in self.mut_vars.iter().rev() {
                    if let Some(&is_mut) = mut_scope.get(name) {
                        if !is_mut {
                            return Err(InterpError::new(format!(
                                "cannot assign to immutable variable '{}'",
                                name
                            )));
                        }
                        break;
                    }
                }
                scope.insert(name.into(), value);
                moved.insert(name.into(), false);
                return Ok(());
            }
        }
        Err(InterpError::new(format!(
            "undefined variable '{}' in assignment",
            name
        )))
    }

    /// Force-update a variable's value, bypassing the mutability check.
    /// Used by `push()` write-back — push mutates in place in codegen
    /// regardless of `mut`, so the interpreter must match (L1 consistency).
    pub fn force_update(&mut self, name: &str, value: Value) {
        for (scope, moved) in self.env.iter_mut().zip(self.moved_vars.iter_mut()).rev() {
            if scope.contains_key(name) {
                scope.insert(name.into(), value);
                moved.insert(name.into(), false);
                return;
            }
        }
    }

    /// Convert a string error into an InterpError with current call stack context.
    pub fn interp_err(&self, msg: String) -> InterpError {
        InterpError::new(msg).with_call_stack(self.call_stack.clone())
    }

    /// Convert a string error with operation context into an InterpError.
    pub fn interp_err_op(&self, msg: String, op: &str) -> InterpError {
        InterpError::with_op(msg, op).with_call_stack(self.call_stack.clone())
    }
}

impl Default for ScopeEnv {
    fn default() -> Self {
        Self::new()
    }
}
