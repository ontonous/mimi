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
    pub(crate) bool_vars: HashMap<String, Z3Bool>,
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
            bool_vars: HashMap::new(),
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

    pub(crate) fn insert_bool(&mut self, name: impl Into<String>, var: Z3Bool) {
        self.bool_vars.insert(name.into(), var);
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
    pub(crate) fn get_bool(&self, name: &str) -> Option<&Z3Bool> {
        self.bool_vars.get(name)
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
    ///
    /// AU-H1: warn once and use a stable `{name}_i` Int const (cannot losslessly
    /// project Real→Int). Callers that mix Int/Real on one binder still degrade,
    /// but the conflict is no longer silent.
    pub(crate) fn get_or_create_int(&mut self, name: &str) -> Z3Int {
        if self.real_vars.contains_key(name) {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                eprintln!(
                    "[mimi verifier] AU-H1: variable '{}' used as both Real and Int; \
                     Int constraints use '{}_i' (verification may be incomplete)",
                    name, name
                );
            }
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
    /// return `Real::from_int` of that Int so constraints stay linked (AU-H1).
    pub(crate) fn get_or_create_real(&mut self, name: &str) -> Z3Real {
        if let Some(iv) = self.int_vars.get(name) {
            // Link Real view to existing Int — no separate unconnected `_r` var.
            return Z3Real::from_int(iv);
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
    /// V-C4: status of each verified function. Callee ensures are only
    /// admitted as axioms when the callee status is `Verified`.
    pub(crate) func_status: HashMap<String, VerifStatus>,
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
    /// Function names materialised from CheckedProgram (qualified).
    pub(crate) checked_function_names: std::collections::HashSet<String>,
    pub(crate) checked_function_effects: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_function_returns: std::collections::HashMap<String, String>,
    pub(crate) checked_comptime_functions: std::collections::HashSet<String>,
    /// Flow transition keys materialised from CheckedProgram: "flow::event::source".
    pub(crate) checked_transitions: std::collections::HashSet<String>,
    /// Capability names materialised from CheckedProgram.
    pub(crate) checked_capabilities: std::collections::HashSet<String>,
    /// Session names materialised from CheckedProgram.
    pub(crate) checked_sessions: std::collections::HashSet<String>,
    pub(crate) checked_session_displays: std::collections::HashMap<String, String>,
    /// Ownership ledger owners materialised from CheckedProgram.
    pub(crate) checked_ownership_owners: std::collections::HashSet<String>,
    pub(crate) checked_ownership_summaries:
        std::collections::HashMap<String, (usize, usize, usize, usize, usize, bool)>,
    /// Type definition names materialised from CheckedProgram.
    pub(crate) checked_type_defs: std::collections::HashSet<String>,
    pub(crate) checked_type_fields: std::collections::HashMap<String, Vec<(String, String)>>,
    pub(crate) checked_type_variants: std::collections::HashMap<String, Vec<(String, Option<String>)>>,
    pub(crate) checked_type_aliases: std::collections::HashMap<String, String>,
    /// Extern function names materialised from CheckedProgram.
    pub(crate) checked_extern_funcs: std::collections::HashSet<String>,
    pub(crate) checked_extern_abis: std::collections::HashMap<String, String>,
    pub(crate) checked_extern_signatures: std::collections::HashMap<String, (usize, String)>,
    pub(crate) checked_call_sites: std::collections::HashMap<String, (String, String, usize, Option<usize>, Vec<String>, Option<String>, String)>,
    /// Protocol names materialised from CheckedProgram.
    pub(crate) checked_protocols: std::collections::HashSet<String>,
    pub(crate) checked_protocol_transitions: std::collections::HashMap<String, Vec<(String, String, String)>>,
    pub(crate) checked_protocol_payloads: std::collections::HashMap<String, String>,
    /// Trait names materialised from CheckedProgram.
    pub(crate) checked_traits: std::collections::HashSet<String>,
    pub(crate) checked_method_signatures: std::collections::HashMap<String, (usize, String)>,
    /// Actor names materialised from CheckedProgram.
    pub(crate) checked_actors: std::collections::HashSet<String>,
    pub(crate) checked_actor_method_signatures: std::collections::HashMap<String, (usize, String)>,
    /// Flow mailbox depths materialised from CheckedProgram.
    pub(crate) checked_mailbox_depths: std::collections::HashMap<String, usize>,
    /// Flow max_children materialised from CheckedProgram.
    pub(crate) checked_max_children: Option<usize>,
    /// Persistent field sets materialised from CheckedProgram.
    pub(crate) checked_persistent_fields: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_transactional_fields: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_metadata_shadow_fields: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_constants: std::collections::HashSet<String>,
    pub(crate) checked_constant_values: std::collections::HashMap<String, (Option<String>, String)>,
    pub(crate) checked_flow_protocols: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_fallback_transitions: std::collections::HashSet<String>,
    pub(crate) checked_ffi_pinned_transitions: std::collections::HashSet<String>,
    pub(crate) checked_transition_param_arity: std::collections::HashMap<String, usize>,
}

/// Backward-compatible verifier with its own solver session.
/// Legacy API: LSP, main/verify.rs, tests.
pub struct Verifier {
    pub(crate) ctx: VerifierCtx,
    pub(crate) session: SolverSession,
}


fn encode_checked_const_value(value: &crate::core::ResolvedConstValue) -> String {
    match value {
        crate::core::ResolvedConstValue::Int(v) => format!("int:{}", v),
        crate::core::ResolvedConstValue::Float(v) => format!("float:{}", v),
        crate::core::ResolvedConstValue::Bool(v) => format!("bool:{}", v),
        crate::core::ResolvedConstValue::String(v) => format!("string:{}", v),
        crate::core::ResolvedConstValue::Unit => "unit".into(),
        crate::core::ResolvedConstValue::Complex => "complex".into(),
    }
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

    pub fn verify_checked(
        &mut self,
        program: &crate::core::CheckedProgram<'_>,
    ) -> Vec<VerificationResult> {
        self.ctx.checked_function_names = program
            .functions()
            .values()
            .map(|function| function.qualified_name.clone())
            .collect();
        self.ctx.checked_function_effects = program
            .functions()
            .values()
            .map(|function| (function.qualified_name.clone(), function.effects.clone()))
            .collect();
        self.ctx.checked_function_returns = program
            .functions()
            .values()
            .map(|function| {
                (
                    function.qualified_name.clone(),
                    crate::core::fmt_type(&function.ret),
                )
            })
            .collect();
        self.ctx.checked_comptime_functions = program
            .functions()
            .values()
            .filter(|function| function.is_comptime)
            .map(|function| function.qualified_name.clone())
            .collect();
        self.ctx.checked_transitions = program
            .transitions()
            .keys()
            .map(|id| format!("{}::{}::{}", id.flow.0, id.event, id.source.name))
            .collect();
        self.ctx.checked_capabilities = program
            .capabilities()
            .values()
            .map(|capability| capability.qualified_name.clone())
            .collect();
        self.ctx.checked_sessions = program
            .sessions()
            .values()
            .map(|session| session.qualified_name.clone())
            .collect();
        self.ctx.checked_session_displays = program
            .sessions()
            .values()
            .map(|session| (session.qualified_name.clone(), session.body_display.clone()))
            .collect();
        self.ctx.checked_ownership_owners = program
            .ownership_ledgers()
            .keys()
            .map(|owner| owner.0.clone())
            .collect();
        let mut ownership_summaries = std::collections::HashMap::new();
        for (owner, ledger) in program.ownership_ledgers() {
            ownership_summaries.insert(
                owner.0.clone(),
                (
                    ledger.action_count(crate::core::ResourceActionKind::Introduce),
                    ledger.action_count(crate::core::ResourceActionKind::Move),
                    ledger.action_count(crate::core::ResourceActionKind::Drop),
                    ledger.action_count(crate::core::ResourceActionKind::Return),
                    ledger.branch_merges.len(),
                    ledger.has_maybe_consumed_merge(),
                ),
            );
        }
        self.ctx.checked_ownership_summaries = ownership_summaries;
        self.ctx.checked_type_defs = program
            .type_defs()
            .values()
            .map(|type_def| type_def.qualified_name.clone())
            .collect();
        let mut type_fields = std::collections::HashMap::new();
        let mut type_variants = std::collections::HashMap::new();
        let mut type_aliases = std::collections::HashMap::new();
        for type_def in program.type_defs().values() {
            if !type_def.fields.is_empty() {
                type_fields.insert(type_def.qualified_name.clone(), type_def.fields.clone());
            }
            if !type_def.variants.is_empty() {
                type_variants.insert(type_def.qualified_name.clone(), type_def.variants.clone());
            }
            if let Some(alias) = &type_def.alias_of {
                type_aliases.insert(type_def.qualified_name.clone(), alias.clone());
            }
        }
        self.ctx.checked_type_fields = type_fields;
        self.ctx.checked_type_variants = type_variants;
        self.ctx.checked_type_aliases = type_aliases;

        let mut extern_funcs = std::collections::HashSet::new();
        let mut extern_abis = std::collections::HashMap::new();
        for block in program.extern_blocks().values() {
            for func in &block.funcs {
                extern_funcs.insert(func.clone());
                extern_abis.insert(func.clone(), block.abi.clone());
            }
        }
        self.ctx.checked_extern_funcs = extern_funcs;
        self.ctx.checked_extern_abis = extern_abis;
        let mut extern_signatures = std::collections::HashMap::new();
        for block in program.extern_blocks().values() {
            for sig in &block.signatures {
                extern_signatures.insert(sig.name.clone(), (sig.params.len(), sig.ret.clone()));
            }
        }
        self.ctx.checked_extern_signatures = extern_signatures;
        let mut call_sites = std::collections::HashMap::new();
        for (node_id, site) in program.call_sites() {
            call_sites.insert(
                node_id.0.clone(),
                (
                    site.owner.clone(),
                    site.callee.clone(),
                    site.argc,
                    site.expected_argc,
                    site.effects.clone(),
                    site.ret.clone(),
                    match site.kind {
                        crate::core::ResolvedCallKind::Function => "function".into(),
                        crate::core::ResolvedCallKind::Extern => "extern".into(),
                        crate::core::ResolvedCallKind::Method => "method".into(),
                        crate::core::ResolvedCallKind::Unknown => "unknown".into(),
                    },
                ),
            );
        }
        self.ctx.checked_call_sites = call_sites;
        self.ctx.checked_protocols = program
            .protocols()
            .values()
            .map(|protocol| protocol.qualified_name.clone())
            .collect();
        let mut protocol_transitions = std::collections::HashMap::new();
        let mut protocol_payloads = std::collections::HashMap::new();
        for protocol in program.protocols().values() {
            protocol_transitions.insert(
                protocol.qualified_name.clone(),
                protocol
                    .transition_records
                    .iter()
                    .map(|tr| {
                        (
                            tr.event.clone(),
                            tr.from_state.clone(),
                            tr.to_states.first().cloned().unwrap_or_default(),
                        )
                    })
                    .collect(),
            );
            for state in &protocol.state_payloads {
                if let Some(ty) = &state.payload_type {
                    protocol_payloads.insert(
                        format!("{}.{}", protocol.qualified_name, state.name),
                        ty.clone(),
                    );
                }
            }
        }
        self.ctx.checked_protocol_transitions = protocol_transitions;
        self.ctx.checked_protocol_payloads = protocol_payloads;
        self.ctx.checked_traits = program
            .traits()
            .values()
            .map(|trait_def| trait_def.qualified_name.clone())
            .collect();
        let mut method_signatures = std::collections::HashMap::new();
        for trait_def in program.traits().values() {
            for method in &trait_def.method_signatures {
                method_signatures.insert(
                    format!("{}.{}", trait_def.qualified_name, method.name),
                    (method.params.len(), method.ret.clone()),
                );
            }
        }
        for impl_def in program.impls().values() {
            for method in &impl_def.method_signatures {
                method_signatures.insert(
                    format!("{}.{}", impl_def.qualified_name, method.name),
                    (method.params.len(), method.ret.clone()),
                );
            }
        }
        self.ctx.checked_method_signatures = method_signatures;
        self.ctx.checked_actors = program
            .actors()
            .values()
            .map(|actor| actor.qualified_name.clone())
            .collect();
        let mut actor_method_signatures = std::collections::HashMap::new();
        for actor in program.actors().values() {
            for method in &actor.method_signatures {
                actor_method_signatures.insert(
                    format!("{}.{}", actor.qualified_name, method.name),
                    (method.params.len(), method.ret.clone()),
                );
            }
        }
        self.ctx.checked_actor_method_signatures = actor_method_signatures;
        let mut mailbox_depths = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if let Some(depth) = flow.mailbox_depth {
                mailbox_depths.insert(flow.id.0.clone(), depth);
            }
        }
        self.ctx.checked_mailbox_depths = mailbox_depths;
        self.ctx.checked_max_children = program.flows().values().find_map(|flow| flow.max_children);
        let mut persistent_fields = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if !flow.persistent_fields.is_empty() {
                persistent_fields.insert(flow.id.0.clone(), flow.persistent_fields.clone());
            }
        }
        self.ctx.checked_persistent_fields = persistent_fields;
        let mut transactional_fields = std::collections::HashMap::new();
        let mut metadata_shadow_fields = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if !flow.transactional_fields.is_empty() {
                transactional_fields.insert(flow.id.0.clone(), flow.transactional_fields.clone());
            }
            if !flow.metadata_shadow_fields.is_empty() {
                metadata_shadow_fields
                    .insert(flow.id.0.clone(), flow.metadata_shadow_fields.clone());
            }
        }
        self.ctx.checked_transactional_fields = transactional_fields;
        self.ctx.checked_metadata_shadow_fields = metadata_shadow_fields;
        self.ctx.checked_constants = program
            .constants()
            .values()
            .map(|constant| constant.qualified_name.clone())
            .collect();
        let mut constant_values = std::collections::HashMap::new();
        for constant in program.constants().values() {
            constant_values.insert(
                constant.qualified_name.clone(),
                (
                    constant.ty.clone(),
                    encode_checked_const_value(&constant.value),
                ),
            );
        }
        self.ctx.checked_constant_values = constant_values;
        let mut flow_protocols = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if !flow.impl_protocols.is_empty() {
                flow_protocols.insert(flow.id.0.clone(), flow.impl_protocols.clone());
            }
        }
        self.ctx.checked_flow_protocols = flow_protocols;
        self.ctx.checked_fallback_transitions = program
            .transitions()
            .values()
            .filter(|transition| transition.is_fallback)
            .map(|transition| {
                format!(
                    "{}::{}::{}",
                    transition.id.flow.0, transition.id.event, transition.id.source.name
                )
            })
            .collect();
        self.ctx.checked_ffi_pinned_transitions = program
            .transitions()
            .values()
            .filter(|transition| transition.is_ffi_pinned)
            .map(|transition| {
                format!(
                    "{}::{}::{}",
                    transition.id.flow.0, transition.id.event, transition.id.source.name
                )
            })
            .collect();
        self.ctx.checked_transition_param_arity = program
            .transitions()
            .values()
            .map(|transition| {
                (
                    format!(
                        "{}::{}::{}",
                        transition.id.flow.0, transition.id.event, transition.id.source.name
                    ),
                    transition.params.len(),
                )
            })
            .collect();
        self.verify_file(program.file())
    }

    pub(crate) fn has_checked_function(&self, name: &str) -> bool {
        self.ctx.checked_function_names.contains(name)
    }

    pub(crate) fn checked_function_effects(&self, name: &str) -> Option<Vec<String>> {
        self.ctx.checked_function_effects.get(name).cloned()
    }

    pub(crate) fn checked_function_return_type(&self, name: &str) -> Option<&str> {
        self.ctx.checked_function_returns.get(name).map(String::as_str)
    }

    pub(crate) fn is_checked_comptime_function(&self, name: &str) -> bool {
        self.ctx.checked_comptime_functions.contains(name)
    }

    pub(crate) fn has_checked_transition(&self, flow: &str, event: &str, source: &str) -> bool {
        self.ctx
            .checked_transitions
            .contains(&format!("{}::{}::{}", flow, event, source))
    }

    pub(crate) fn has_checked_session(&self, name: &str) -> bool {
        self.ctx.checked_sessions.contains(name)
    }

    pub(crate) fn checked_session_display(&self, name: &str) -> Option<&str> {
        self.ctx.checked_session_displays.get(name).map(String::as_str)
    }

    pub(crate) fn has_checked_ownership_owner(&self, owner: &str) -> bool {
        self.ctx.checked_ownership_owners.contains(owner)
    }

    pub(crate) fn checked_ownership_summary(
        &self,
        owner: &str,
    ) -> Option<(usize, usize, usize, usize, usize, bool)> {
        self.ctx.checked_ownership_summaries.get(owner).copied()
    }

    pub(crate) fn has_checked_type_def(&self, name: &str) -> bool {
        self.ctx.checked_type_defs.contains(name)
    }

    pub(crate) fn checked_type_fields(
        &self,
        name: &str,
    ) -> Option<Vec<(String, String)>> {
        self.ctx.checked_type_fields.get(name).cloned()
    }

    pub(crate) fn checked_type_variants(
        &self,
        name: &str,
    ) -> Option<Vec<(String, Option<String>)>> {
        self.ctx.checked_type_variants.get(name).cloned()
    }

    pub(crate) fn checked_type_alias_of(&self, name: &str) -> Option<&str> {
        self.ctx.checked_type_aliases.get(name).map(String::as_str)
    }

    pub(crate) fn has_checked_extern_func(&self, name: &str) -> bool {
        self.ctx.checked_extern_funcs.contains(name)
    }

    pub(crate) fn checked_extern_abi(&self, name: &str) -> Option<&str> {
        self.ctx.checked_extern_abis.get(name).map(String::as_str)
    }

    pub(crate) fn checked_extern_signature(&self, name: &str) -> Option<(usize, String)> {
        self.ctx.checked_extern_signatures.get(name).cloned()
    }

    pub(crate) fn has_checked_call_to(&self, callee: &str) -> bool {
        self.ctx
            .checked_call_sites
            .values()
            .any(|(_, name, _, _, _, _, _)| name == callee)
    }

    pub(crate) fn checked_call_return_type(&self, callee: &str) -> Option<String> {
        self.ctx.checked_call_sites.values().find_map(|(_, name, _, _, _, ret, _)| {
            if name == callee {
                ret.clone()
            } else {
                None
            }
        })
    }

    pub(crate) fn has_checked_call_with_effect(&self, callee: &str, effect: &str) -> bool {
        self.ctx.checked_call_sites.values().any(|(_, name, _, _, effects, _, _)| {
            name == callee && effects.iter().any(|e| e == effect)
        })
    }

    pub(crate) fn checked_call_arity_mismatches(&self) -> usize {
        self.ctx
            .checked_call_sites
            .values()
            .filter(|(_, _, argc, expected, _, _, _)| expected.map(|exp| exp != *argc).unwrap_or(false))
            .count()
    }

    pub(crate) fn has_checked_protocol(&self, name: &str) -> bool {
        self.ctx.checked_protocols.contains(name)
    }

    pub(crate) fn checked_protocol_transitions(
        &self,
        protocol: &str,
    ) -> Option<Vec<(String, String, String)>> {
        self.ctx.checked_protocol_transitions.get(protocol).cloned()
    }

    pub(crate) fn checked_protocol_payload(
        &self,
        protocol: &str,
        state: &str,
    ) -> Option<String> {
        self.ctx
            .checked_protocol_payloads
            .get(&format!("{protocol}.{state}"))
            .cloned()
    }

    pub(crate) fn has_checked_trait(&self, name: &str) -> bool {
        self.ctx.checked_traits.contains(name)
    }

    pub(crate) fn checked_method_signature(&self, key: &str) -> Option<(usize, String)> {
        self.ctx.checked_method_signatures.get(key).cloned()
    }

    pub(crate) fn has_checked_actor(&self, name: &str) -> bool {
        self.ctx.checked_actors.contains(name)
    }

    pub(crate) fn checked_actor_method_signature(
        &self,
        actor: &str,
        method: &str,
    ) -> Option<(usize, String)> {
        self.ctx
            .checked_actor_method_signatures
            .get(&format!("{actor}.{method}"))
            .cloned()
    }

    pub(crate) fn checked_mailbox_depth(&self, flow_name: &str) -> Option<usize> {
        self.ctx.checked_mailbox_depths.get(flow_name).copied().or_else(|| {
            self.ctx.checked_mailbox_depths.iter().find_map(|(qualified, depth)| {
                qualified
                    .rsplit("::")
                    .next()
                    .filter(|bare| *bare == flow_name)
                    .map(|_| *depth)
            })
        })
    }

    pub(crate) fn checked_max_children(&self) -> Option<usize> {
        self.ctx.checked_max_children
    }

    pub(crate) fn checked_persistent_fields(&self, flow_name: &str) -> Option<Vec<String>> {
        self.lookup_checked_field_set(&self.ctx.checked_persistent_fields, flow_name)
    }

    pub(crate) fn checked_transactional_fields(&self, flow_name: &str) -> Option<Vec<String>> {
        self.lookup_checked_field_set(&self.ctx.checked_transactional_fields, flow_name)
    }

    pub(crate) fn checked_metadata_shadow_fields(&self, flow_name: &str) -> Option<Vec<String>> {
        self.lookup_checked_field_set(&self.ctx.checked_metadata_shadow_fields, flow_name)
    }

    pub(crate) fn has_checked_constant(&self, name: &str) -> bool {
        self.ctx.checked_constants.contains(name)
    }

    pub(crate) fn checked_constant_value(
        &self,
        name: &str,
    ) -> Option<(Option<String>, String)> {
        self.ctx.checked_constant_values.get(name).cloned()
    }

    pub(crate) fn checked_flow_protocols(&self, flow_name: &str) -> Option<Vec<String>> {
        self.lookup_checked_field_set(&self.ctx.checked_flow_protocols, flow_name)
    }

    pub(crate) fn is_checked_fallback_transition(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> bool {
        self.ctx
            .checked_fallback_transitions
            .contains(&format!("{}::{}::{}", flow, event, source))
    }

    pub(crate) fn is_checked_ffi_pinned_transition(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> bool {
        self.ctx
            .checked_ffi_pinned_transitions
            .contains(&format!("{}::{}::{}", flow, event, source))
    }

    pub(crate) fn checked_transition_param_arity(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> Option<usize> {
        self.ctx
            .checked_transition_param_arity
            .get(&format!("{}::{}::{}", flow, event, source))
            .copied()
    }

    fn lookup_checked_field_set(
        &self,
        map: &std::collections::HashMap<String, Vec<String>>,
        flow_name: &str,
    ) -> Option<Vec<String>> {
        map.get(flow_name).cloned().or_else(|| {
            map.iter().find_map(|(qualified, fields)| {
                qualified
                    .rsplit("::")
                    .next()
                    .filter(|bare| *bare == flow_name)
                    .map(|_| fields.clone())
            })
        })
    }

    pub(crate) fn verify_file(&mut self, file: &File) -> Vec<VerificationResult> {
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
                // V-H6: register actor/impl/flow methods for call-site ensures lookup.
                Item::Actor(a) => {
                    for m in &a.methods {
                        let mut f = m.clone();
                        f.name = format!("{}::{}", a.name, m.name);
                        self.func_defs.insert(f.name.clone(), f);
                        self.func_defs.insert(m.name.clone(), m.clone());
                    }
                }
                Item::Impl(i) => {
                    for m in &i.methods {
                        let mut f = m.clone();
                        f.name = format!("{}::{}::{}", i.type_name, i.trait_name, m.name);
                        self.func_defs.insert(f.name.clone(), f);
                        self.func_defs.insert(m.name.clone(), m.clone());
                    }
                }
                Item::Flow(flow) => {
                    for t in &flow.transitions {
                        if let Some(body) = &t.body {
                            let f = crate::ast::FuncDef {
                                name: format!("{}::{}", flow.name, t.name),
                                pub_: false,
                                params: t.params.clone(),
                                ret: None,
                                body: body.clone(),
                                where_clause: vec![],
                                generics: vec![],
                                effects: vec![],
                                is_comptime: false,
                                is_async: false,
                                extern_abi: None,
                                pos: t.pos,
                            };
                            self.func_defs.insert(f.name.clone(), f);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
