use crate::ast::{Expr, File};
use crate::diagnostic::Diagnostic;
use std::collections::HashMap;
use z3::ast::String as Z3String;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};
use z3::SatResult;
use z3::Solver;

const DEFAULT_TIMEOUT_MS: u64 = 5000;

#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub func_name: String,
    pub status: VerifStatus,
    pub message: String,
    pub diagnostic: Option<Diagnostic>,
    pub duration_us: u64,
    pub constraint_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifStatus {
    Verified,
    Failed,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Counterexample {
    pub assignments: Vec<(String, i64)>,
    pub real_assignments: Vec<(String, f64)>,
    pub string_assignments: Vec<(String, String)>,
    pub violated_ensures: Vec<String>,
    pub violated_indices: Vec<usize>,
}

pub(crate) struct Z3VarMap {
    pub(crate) int_vars: HashMap<String, Z3Int>,
    pub(crate) real_vars: HashMap<String, Z3Real>,
    pub(crate) string_nonempty: HashMap<String, Z3Bool>,
    /// String length variables: s_len = Z3Int for each string param.
    pub(crate) string_len: HashMap<String, Z3Int>,
    /// Z3 string theory variables for string params. Enables string equality,
    /// contains, at, and other native Z3 string operations.
    pub(crate) string_vars: HashMap<String, Z3String>,
    /// List length variables: xs_len = Z3Int for each list param.
    /// Used to model length-preserving list operations like sort().
    pub(crate) list_len: HashMap<String, Z3Int>,
}

impl Z3VarMap {
    pub(crate) fn new() -> Self {
        Self {
            int_vars: HashMap::new(),
            real_vars: HashMap::new(),
            string_nonempty: HashMap::new(),
            string_len: HashMap::new(),
            string_vars: HashMap::new(),
            list_len: HashMap::new(),
        }
    }

    pub(crate) fn insert_int(&mut self, name: impl Into<String>, var: Z3Int) {
        self.int_vars.insert(name.into(), var);
    }

    pub(crate) fn insert_real(&mut self, name: impl Into<String>, var: Z3Real) {
        self.real_vars.insert(name.into(), var);
    }

    pub(crate) fn insert_string_nonempty(&mut self, name: impl Into<String>, var: Z3Bool) {
        self.string_nonempty.insert(name.into(), var);
    }

    /// Register a length variable for a string parameter.
    pub(crate) fn insert_string_len(&mut self, name: impl Into<String>, var: Z3Int) {
        self.string_len.insert(name.into(), var);
    }

    #[inline]
    pub(crate) fn get_int(&self, name: &str) -> Option<&Z3Int> {
        self.int_vars.get(name)
    }

    #[inline]
    pub(crate) fn get_real(&self, name: &str) -> Option<&Z3Real> {
        self.real_vars.get(name)
    }

    #[inline]
    pub(crate) fn get_string_nonempty(&self, name: &str) -> Option<&Z3Bool> {
        self.string_nonempty.get(name)
    }

    #[inline]
    pub(crate) fn get_string_len(&self, name: &str) -> Option<&Z3Int> {
        self.string_len.get(name)
    }

    /// Register a length variable for a list parameter (e.g., sort() preserves length).
    pub(crate) fn insert_list_len(&mut self, name: impl Into<String>, var: Z3Int) {
        self.list_len.insert(name.into(), var);
    }

    #[inline]
    pub(crate) fn get_list_len(&self, name: &str) -> Option<&Z3Int> {
        self.list_len.get(name)
    }

    /// Register a Z3 string theory variable for a string parameter.
    pub(crate) fn insert_string_var(&mut self, name: impl Into<String>, var: Z3String) {
        self.string_vars.insert(name.into(), var);
    }

    #[inline]
    pub(crate) fn get_string_var(&self, name: &str) -> Option<&Z3String> {
        self.string_vars.get(name)
    }

    #[inline]
    pub(crate) fn is_real(&self, name: &str) -> bool {
        self.real_vars.contains_key(name)
    }

    /// Get or create an Int variable. If the same name is already registered as Real,
    /// this signals a type-conflict bug — the same logical variable is being used as
    /// both Real and Int, causing Z3 constraint fragmentation.
    /// P2.1 fix: detect the conflict and use a suffixed name instead of silently
    /// creating a separate Z3 variable.
    pub(crate) fn get_or_create_int(&mut self, name: &str) -> Z3Int {
        if self.real_vars.contains_key(name) {
            // Type conflict — same name used as Real. Use suffixed name to avoid
            // creating a duplicate Z3 variable for the same logical name.
            let int_name = format!("{}_i", name);
            return self
                .int_vars
                .entry(int_name.clone())
                .or_insert_with(|| Z3Int::new_const(int_name))
                .clone();
        }
        self.int_vars
            .entry(name.to_string())
            .or_insert_with(|| Z3Int::new_const(name))
            .clone()
    }

    /// Get or create a Real variable. If the same name is already registered as Int,
    /// this signals a type-conflict bug — the same logical variable is being used as
    /// both Int and Real, causing Z3 constraint fragmentation.
    /// P2.1 fix: detect the conflict and use a suffixed name instead of silently
    /// creating a separate Z3 variable.
    pub(crate) fn get_or_create_real(&mut self, name: &str) -> Z3Real {
        if self.int_vars.contains_key(name) {
            // Type conflict — same name used as Int. Use suffixed name to avoid
            // creating a duplicate Z3 variable for the same logical name.
            let real_name = format!("{}_r", name);
            return self
                .real_vars
                .entry(real_name.clone())
                .or_insert_with(|| Z3Real::new_const(real_name))
                .clone();
        }
        self.real_vars
            .entry(name.to_string())
            .or_insert_with(|| Z3Real::new_const(name))
            .clone()
    }
}

pub struct Verifier {
    pub(crate) solver: Solver,
    pub(crate) timeout_ms: u64,
    /// Function definitions indexed by name, collected from the merged file.
    /// Used by cross-module verification to look up callee ensures.
    pub(crate) func_defs: HashMap<String, crate::ast::FuncDef>,
    /// Mapping from let-variable names to their init expressions.
    /// Populated during verify_func to enable substitution of local variables
    /// when encoding body-return expressions. Fixes P0.1 for let-binding calls:
    /// `let y = double(x); y` now correctly resolves `y` to `double(x)`.
    pub(crate) let_subst: HashMap<String, Expr>,
    /// E1: Tracks the number of push() calls without corresponding pop().
    /// When check_safe() replaces the solver (Unknown/crash), the fresh solver
    /// starts at depth 0 — callers must not pop(1) in that case.
    pub(crate) push_depth: u32,
}

impl Verifier {
    pub fn new() -> Result<Self, String> {
        Self::with_timeout(DEFAULT_TIMEOUT_MS)
    }

    pub fn with_timeout(timeout_ms: u64) -> Result<Self, String> {
        let solver = std::panic::catch_unwind(Solver::new)
            .map_err(|_| "failed to initialize Z3 solver (is libz3 installed?)".to_string())?;
        let mut params = z3::Params::new();
        params.set_u32("timeout", timeout_ms as u32);
        solver.set_params(&params);
        Ok(Self {
            solver,
            timeout_ms,
            func_defs: HashMap::new(),
            let_subst: HashMap::new(),
            push_depth: 0,
        })
    }

    /// Check satisfiability with timeout and crash protection.
    /// Returns Unknown on timeout/crash instead of panicking.
    /// If Z3 panics/crashes (e.g. segfault in libz3), recreates the solver
    /// because Z3's C API does not guarantee a usable solver state after a
    /// crash. The old solver is dropped and a fresh one is created.
    pub(crate) fn check_safe(&mut self) -> SatResult {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.solver.check())).ok();
        match result {
            Some(SatResult::Sat) => SatResult::Sat,
            Some(SatResult::Unsat) => SatResult::Unsat,
            _ => {
                // 2.1/2.2: Z3 crashed or timed out — solver may be corrupt.
                // Replace with a fresh solver. Params (incl. timeout) are
                // re-applied because the new solver starts with defaults.
                // Callers must check SatResult and return/abort on Unknown
                // rather than continuing to use the solver's assertion stack
                // (which is now empty after replacement).
                let mut params = z3::Params::new();
                params.set_u32("timeout", self.timeout_ms as u32);
                let new_solver = Solver::new();
                new_solver.set_params(&params);
                let _ = std::mem::replace(&mut self.solver, new_solver);
                self.push_depth = 0;
                SatResult::Unknown
            }
        }
    }

    /// Update the solver timeout. Useful for LSP dynamic timeout adjustment.
    pub fn set_timeout(&mut self, timeout_ms: u64) {
        self.timeout_ms = timeout_ms;
        let mut params = z3::Params::new();
        params.set_u32("timeout", timeout_ms as u32);
        self.solver.set_params(&params);
    }

    /// E1: Push a new solver scope, tracking depth for safe pop after Unknown.
    pub(crate) fn solver_push(&mut self) {
        self.solver.push();
        self.push_depth += 1;
    }

    /// E1: Pop solver scope, but only if push_depth > 0 (safe after solver replacement).
    pub(crate) fn solver_pop(&mut self, levels: u32) {
        if self.push_depth >= levels {
            self.push_depth -= levels;
            self.solver.pop(levels);
        }
    }

    pub fn verify_file(&mut self, file: &File) -> Vec<VerificationResult> {
        let mut results = Vec::new();
        self.verify_items(&file.items, &mut results);
        results
    }

    /// Dump the current solver state as an SMT-LIB2 string.
    /// Returns `None` if the solver has no assertions.
    pub fn dump_smt2(&self) -> Option<String> {
        let s = self.solver.to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}
