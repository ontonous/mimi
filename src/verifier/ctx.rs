use crate::ast::{Expr, File};
use crate::diagnostic::Diagnostic;
use std::collections::HashMap;
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
    pub violated_ensures: Vec<String>,
    pub violated_indices: Vec<usize>,
}

pub(crate) struct Z3VarMap {
    pub(crate) int_vars: HashMap<String, Z3Int>,
    pub(crate) real_vars: HashMap<String, Z3Real>,
    pub(crate) string_nonempty: HashMap<String, Z3Bool>,
    /// String length variables: s_len = Z3Int for each string param.
    pub(crate) string_len: HashMap<String, Z3Int>,
}

impl Z3VarMap {
    pub(crate) fn new() -> Self {
        Self {
            int_vars: HashMap::new(),
            real_vars: HashMap::new(),
            string_nonempty: HashMap::new(),
            string_len: HashMap::new(),
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

    #[inline]
    pub(crate) fn is_real(&self, name: &str) -> bool {
        self.real_vars.contains_key(name)
    }

    /// Get or create an Int variable. If the key already exists as Real, creates a new Int.
    pub(crate) fn get_or_create_int(&mut self, name: &str) -> Z3Int {
        self.int_vars
            .entry(name.to_string())
            .or_insert_with(|| Z3Int::new_const(name))
            .clone()
    }

    /// Get or create a Real variable. If the key already exists as Int, creates a new Real.
    pub(crate) fn get_or_create_real(&mut self, name: &str) -> Z3Real {
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
}

impl Verifier {
    pub fn new() -> Result<Self, String> {
        Self::with_timeout(DEFAULT_TIMEOUT_MS)
    }

    pub fn with_timeout(timeout_ms: u64) -> Result<Self, String> {
        let solver = std::panic::catch_unwind(|| Solver::new())
            .map_err(|_| "failed to initialize Z3 solver (is libz3 installed?)".to_string())?;
        let mut params = z3::Params::new();
        params.set_u32("timeout", timeout_ms as u32);
        solver.set_params(&params);
        Ok(Self { solver, timeout_ms, func_defs: HashMap::new(), let_subst: HashMap::new() })
    }

    /// Check satisfiability with timeout and crash protection.
    /// Returns Unknown on timeout/crash instead of panicking.
    /// If Z3 panics/crashes (e.g. segfault in libz3), recreates the solver
    /// because Z3's C API does not guarantee a usable solver state after a
    /// crash. The old solver is dropped and a fresh one is created.
    pub(crate) fn check_safe(&mut self) -> SatResult {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.solver.check()))
                .ok();
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

    pub fn verify_file(&mut self, file: &File) -> Vec<VerificationResult> {
        let mut results = Vec::new();
        self.verify_items(&file.items, &mut results);
        results
    }
}
