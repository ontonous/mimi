use crate::ast::{Expr, File, Item};
use crate::diagnostic::Diagnostic;
use std::collections::HashMap;
use z3::ast::String as Z3String;
use z3::ast::{Bool as Z3Bool, Int as Z3Int, Real as Z3Real};
use z3::SatResult;
use z3::Solver;

pub(crate) const DEFAULT_TIMEOUT_MS: u64 = 5000;

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

/// Wraps a Z3 Solver with crash-recovery tracking.
/// Flow paradigm: the state machine owns SolverSession directly instead of
/// hiding it behind &mut self on Verifier. Push/pop/replace are explicit
/// transitions on the solver rather than implicit side effects.
pub struct SolverSession {
    pub(crate) solver: Solver,
    /// True after check() replaces the solver on crash. When set, pop() is a
    /// no-op — the fresh solver starts at Z3 depth 0; pending old-solver pops
    /// are moot. Cleared on the next successful check() or reset().
    pub(crate) replaced: bool,
    /// B6: True after solver replacement — subsequent check() returns Unknown
    /// because assertions from before the replacement are lost. The new solver
    /// is empty, so any check result would be misleading (false Sat/Unsat).
    /// Only cleared by reset() which starts a completely fresh verification.
    pub(crate) poisoned: bool,
    pub(crate) timeout_ms: u64,
}

impl SolverSession {
    pub fn new(timeout_ms: u64) -> Result<Self, String> {
        let solver = std::panic::catch_unwind(Solver::new)
            .map_err(|_| "failed to initialize Z3 solver (is libz3 installed?)".to_string())?;
        let mut params = z3::Params::new();
        params.set_u32("timeout", timeout_ms as u32);
        solver.set_params(&params);
        Ok(Self {
            solver,
            replaced: false,
            poisoned: false,
            timeout_ms,
        })
    }

    /// Check satisfiability with timeout and crash protection.
    /// Returns Unknown on timeout/crash instead of panicking.
    /// On crash: replaces the solver (Z3's C API may be corrupt after crash)
    /// and sets replaced = true so pending pop() calls are skipped.
    /// On Sat/Unsat: clears replaced flag.
    pub fn check(&mut self) -> SatResult {
        // B6: If poisoned (solver was replaced after crash/timeout), all
        // assertions from the original solver are lost. The fresh solver is
        // empty, so checking it would produce misleading results (false Sat
        // on an empty solver). Return Unknown to signal verification
        // incompleteness.
        if self.poisoned {
            return SatResult::Unknown;
        }
        // H14-fix: distinguish Z3 crash from timeout. A crash (panic) is now
        // logged to stderr so verification incompleteness is visible.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.solver.check()));
        match result {
            Ok(SatResult::Sat) => {
                self.replaced = false;
                SatResult::Sat
            }
            Ok(SatResult::Unsat) => {
                self.replaced = false;
                SatResult::Unsat
            }
            Ok(SatResult::Unknown) => {
                // Normal timeout — solver may be in an inconsistent state.
                // Replace with a fresh solver. The new solver starts at Z3
                // depth 0, but callers (check_scope) have a pending push()
                // that was on the OLD solver — the new solver never saw it.
                // Setting `replaced = true` ensures the next pop() is a
                // no-op, preventing Z3 UB (pop below depth 0).
                let mut params = z3::Params::new();
                params.set_u32("timeout", self.timeout_ms as u32);
                let new_solver = Solver::new();
                new_solver.set_params(&params);
                new_solver.reset();
                // AU-H6: do not Drop the old solver after timeout/crash — Z3 may
                // be corrupted and Z3_del_solver can double-free. Leak it.
                let old = std::mem::replace(&mut self.solver, new_solver);
                std::mem::forget(old);
                self.replaced = true; // skip next pop() — push was on old solver
                self.poisoned = true; // B6: assertions lost, future checks unreliable
                SatResult::Unknown
            }
            Err(panic_payload) => {
                // H14-fix: Z3 solver crash — log it so verification
                // incompleteness is visible rather than silently Unknown.
                let msg = panic_payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic_payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "(non-string panic payload)".to_string());
                eprintln!("[mimi verifier] Z3 solver crashed: {}", msg);
                let mut params = z3::Params::new();
                params.set_u32("timeout", self.timeout_ms as u32);
                let new_solver = Solver::new();
                new_solver.set_params(&params);
                // RT-H5 (audit): after replacing, the new solver starts at
                // Z3 depth 0. But callers (check_scope) have a pending push()
                // that was on the OLD solver — the new solver never saw it.
                // Setting `replaced = true` ensures the next pop() is a
                // no-op, preventing Z3 UB (pop below depth 0).
                new_solver.reset();
                // AU-H6: leak corrupted solver instead of Drop → Z3_del_solver.
                let old = std::mem::replace(&mut self.solver, new_solver);
                std::mem::forget(old);
                self.replaced = true; // skip next pop() — push was on old solver
                self.poisoned = true; // B6: assertions lost, future checks unreliable
                SatResult::Unknown
            }
        }
    }

    /// RT-H5 (audit): reset clears all assertions and resets Z3 depth to 0.
    /// This is always safe to call regardless of `replaced` state — a fresh
    /// Solver::new() followed by reset() is idempotent with a reused solver.
    pub fn reset(&mut self) {
        self.solver.reset();
        self.replaced = false;
        self.poisoned = false;
    }

    pub fn push(&mut self) {
        self.solver.push();
    }

    /// Pop solver scope. NO-OP if the solver was replaced by check() (fresh
    /// solver starts at Z3 depth 0; pending old-solver pops are irrelevant).
    pub fn pop(&mut self) {
        if !self.replaced {
            self.solver.pop(1);
        }
    }

    /// Assert a boolean constraint into the solver.
    /// Uses z3's `Borrow<Bool>` bound — all callers pass boolean comparisons.
    pub fn assert<T: std::borrow::Borrow<z3::ast::Bool>>(&self, ast: T) {
        self.solver.assert(ast);
    }

    pub fn get_model(&self) -> Option<z3::Model> {
        self.solver.get_model()
    }

    pub fn set_params(&self, params: &z3::Params) {
        self.solver.set_params(params);
    }

    /// Push, assert constraint, check, pop.
    ///
    /// Wraps the common push→assert→check→pop pattern used by call-site
    /// precondition checking (both Mimi-internal calls and extern FFI calls).
    ///
    /// Returns the SatResult and, if Sat, the model for counterexample
    /// extraction. The solver scope is cleaned up by pop() even on crash
    /// (pop is a no-op when the solver was replaced during check()).
    pub fn check_scope<T: std::borrow::Borrow<z3::ast::Bool>>(
        &mut self,
        constraint: T,
    ) -> (SatResult, Option<z3::Model>) {
        self.push();
        self.assert(constraint);
        let result = self.check();
        let model = if matches!(result, SatResult::Sat) {
            self.get_model()
        } else {
            None
        };
        self.pop();
        (result, model)
    }

    /// Multi-assertion version of check_scope. Pushes a scope, asserts all
    /// constraints, checks, and pops. Useful for ensures checks where
    /// multiple NOT(ensures) are asserted simultaneously.
    ///
    /// Returns Sat if any constraint is satisfiable (i.e., a postcondition
    /// may be violated). Returns Unsat if all constraints are unsatisfiable
    /// (all postconditions hold). Returns Unknown on timeout/crash.
    ///
    /// If constraints is empty, returns Sat immediately (no-op) — matching
    /// Z3's behavior that an empty assertion set is trivially satisfiable.
    pub fn check_scope_multi<T: std::borrow::Borrow<z3::ast::Bool>>(
        &mut self,
        constraints: Vec<T>,
    ) -> (SatResult, Option<z3::Model>) {
        if constraints.is_empty() {
            return (SatResult::Sat, None);
        }
        self.push();
        for c in constraints {
            self.assert(c);
        }
        let result = self.check();
        let model = if matches!(result, SatResult::Sat) {
            self.get_model()
        } else {
            None
        };
        self.pop();
        (result, model)
    }

    pub fn dump_smt2(&self) -> Option<String> {
        let s = self.solver.to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// Context for verification lookups (func_defs, let_subst).
/// Owned by VerifierState in the Flow path — contains no Z3 solver state.
#[derive(Default)]
pub struct VerifierCtx {
    pub(crate) func_defs: HashMap<String, crate::ast::FuncDef>,
    /// Mapping from let-variable names to their init expressions.
    /// Populated during verify_func to enable substitution of local variables
    /// when encoding body-return expressions. Fixes P0.1 for let-binding calls:
    /// `let y = double(x); y` now correctly resolves `y` to `double(x)`.
    // TODO(#issue-TBD): this field is written but never read — see §21 red
    // line 3 (escape hatch). The current let-substitution logic uses local
    // variables in verify_func (func.rs:393); the ctx-level field is a
    // vestigial design. Either remove it or wire it into the Z3 encoding
    // path so the substitution survives function boundaries.
    #[allow(dead_code)]
    pub(crate) let_subst: HashMap<String, Expr>,
}

/// Backward-compatible verifier with its own solver session.
/// Legacy API: LSP, main/verify.rs, tests.
pub struct Verifier {
    pub(crate) ctx: VerifierCtx,
    pub(crate) session: SolverSession,
}

impl Verifier {
    pub fn new() -> Result<Self, String> {
        Self::with_timeout(DEFAULT_TIMEOUT_MS)
    }

    pub fn with_timeout(timeout_ms: u64) -> Result<Self, String> {
        SolverSession::new(timeout_ms).map(|session| Self {
            ctx: VerifierCtx::default(),
            session,
        })
    }

    pub fn verify_file(&mut self, file: &File) -> Vec<VerificationResult> {
        let mut results = Vec::new();
        VerifierCtx::verify_items(&mut self.ctx, &mut self.session, &file.items, &mut results);
        results
    }

    pub fn set_timeout(&mut self, timeout_ms: u64) {
        self.session.timeout_ms = timeout_ms;
        let mut params = z3::Params::new();
        params.set_u32("timeout", timeout_ms as u32);
        self.session.set_params(&params);
    }

    /// AU-H3: true after Z3 crash/timeout replacement — session assertions lost.
    pub fn is_poisoned(&self) -> bool {
        self.session.poisoned
    }

    pub fn dump_smt2(&self) -> Option<String> {
        self.session.dump_smt2()
    }
}

impl VerifierCtx {
    pub fn collect_func_defs(&mut self, items: &[Item]) {
        for item in items {
            match item {
                Item::Func(f) => {
                    self.func_defs.insert(f.name.clone(), f.clone());
                }
                Item::Module(m) => self.collect_func_defs(&m.items),
                _ => {}
            }
        }
    }
}
