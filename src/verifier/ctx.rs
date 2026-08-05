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
    /// v0.31.25: Proof artifact binding the result to its semantic context.
    /// None for NoObligations / InfrastructureError (no proof attempted).
    pub artifact: Option<ProofArtifact>,
    /// v0.31.25: When status is NotInTrustedSubset, indicates whether the
    /// unsupported construct is in the contract (requires/ensures) or the body.
    /// Contract-level → `mimi verify` hard error.
    /// Body-level → SolverUnknown (doesn't block unless `#[verified]`).
    pub trusted_subset_domain: Option<TrustedSubsetDomain>,
}

/// v0.31.25: Verification domain isolation — where the unsupported construct lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustedSubsetDomain {
    /// The contract expression (requires/ensures) contains unsupported constructs.
    /// `mimi verify` treats this as a hard error.
    Contract,
    /// The function body contains unsupported constructs, but contracts are scalar.
    /// Produces SolverUnknown; doesn't block `mimi verify` (blocks `#[verified]`).
    Body,
}

/// v0.31.25: Eight-result verification algebra.
///
/// Replaces the 3-state Verified/Failed/Unknown with a precise taxonomy
/// that distinguishes *why* verification did not produce a proof.
/// All non-Proven results are fail-closed: `mimi verify` reports them,
/// `mimi build --verify-ffi` rejects Unknown/Timeout/InfrastructureError.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifStatus {
    /// Verification condition discharged — postcondition holds under precondition.
    Proven,
    /// Counterexample found — postcondition is violable.
    Disproven,
    /// Contract or body contains constructs outside the trusted subset
    /// (heap, Flow, Actor, loop, recursion, FFI, etc.).
    NotInTrustedSubset,
    /// Solver returned `unknown` (incomplete theory, quantifier instantiation).
    SolverUnknown,
    /// Solver exceeded the time budget.
    Timeout,
    /// Z3 not available, solver crash, or encoding error.
    InfrastructureError,
    /// Contract references runtime-only state (e.g. `old(self.field)` on
    /// mutable state, channel contents, actor mailbox depth).
    RuntimeOnlyContract,
    /// No verification conditions generated (pure function with no
    /// requires/ensures, or empty body).
    NoObligations,
}

impl VerifStatus {
    /// Backward-compatible alias for Proven.
    #[allow(non_upper_case_globals)]
    pub const Verified: VerifStatus = VerifStatus::Proven;
    /// Backward-compatible alias for Disproven.
    #[allow(non_upper_case_globals)]
    pub const Failed: VerifStatus = VerifStatus::Disproven;

    /// True when the result is a definitive proof or disproof.
    pub fn is_definitive(&self) -> bool {
        matches!(self, VerifStatus::Proven | VerifStatus::Disproven)
    }

    /// True when verification did not produce a proof (fail-closed).
    pub fn is_inconclusive(&self) -> bool {
        !self.is_definitive()
    }

    /// True when the result indicates a solver/infrastructure limitation
    /// (as opposed to a semantic limitation like NotInTrustedSubset).
    pub fn is_solver_limitation(&self) -> bool {
        matches!(
            self,
            VerifStatus::SolverUnknown | VerifStatus::Timeout | VerifStatus::InfrastructureError
        )
    }

    /// 0.31.42: Plain-language description for non-expert users.
    ///
    /// Translates the SMT/verification jargon into actionable guidance.
    /// Used by CLI display and LSP hover.
    pub fn plain_language(&self) -> &'static str {
        match self {
            VerifStatus::Proven => "contract verified — the function always satisfies its ensures clause when the requires clause holds",
            VerifStatus::Disproven => "contract violated — there exist inputs satisfying requires that break ensures",
            VerifStatus::NotInTrustedSubset => "not verifiable — the function uses features outside the verified subset (loops, heap, FFI, actors, etc.)",
            VerifStatus::SolverUnknown => "inconclusive — the solver could not determine correctness (try simplifying the contract or adding lemmas)",
            VerifStatus::Timeout => "timed out — the solver exceeded its time budget (try simplifying the contract or increasing the timeout)",
            VerifStatus::InfrastructureError => "infrastructure error — the solver is unavailable or crashed (check Z3 installation)",
            VerifStatus::RuntimeOnlyContract => "runtime-only contract — the contract references mutable state that cannot be verified statically",
            VerifStatus::NoObligations => "no obligations — the function has no requires/ensures to verify",
        }
    }

    /// 0.31.42: Actionable hint for the user.
    ///
    /// Returns `None` for Proven/NoObligations (no action needed).
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            VerifStatus::Proven | VerifStatus::NoObligations => None,
            VerifStatus::Disproven => Some("check the counterexample below — it shows specific inputs that violate the contract"),
            VerifStatus::NotInTrustedSubset => Some("consider extracting the verifiable logic into a pure helper function"),
            VerifStatus::SolverUnknown => Some("try adding intermediate assertions (math:) to guide the solver"),
            VerifStatus::Timeout => Some("try breaking the contract into smaller lemmas or increasing --timeout"),
            VerifStatus::InfrastructureError => Some("install Z3: apt install z3 / brew install z3"),
            VerifStatus::RuntimeOnlyContract => Some("consider using a pure function for the verifiable invariant"),
        }
    }

    /// 0.31.42: CLI icon for compact display.
    pub fn icon(&self) -> &'static str {
        match self {
            VerifStatus::Proven => "✓",
            VerifStatus::Disproven => "✗",
            VerifStatus::NotInTrustedSubset => "⊘",
            VerifStatus::SolverUnknown => "?",
            VerifStatus::Timeout => "⏱",
            VerifStatus::InfrastructureError => "⚠",
            VerifStatus::RuntimeOnlyContract => "↻",
            VerifStatus::NoObligations => "·",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Counterexample {
    pub assignments: Vec<(String, i64)>,
    pub real_assignments: Vec<(String, f64)>,
    pub string_assignments: Vec<(String, String)>,
    pub violated_ensures: Vec<String>,
    pub violated_indices: Vec<usize>,
}

/// v0.31.25: Proof artifact — binds a verification result to its semantic
/// context for replay and tamper detection.
///
/// Cache key: `(semantics_version, solver_version, integer_model, vir_hash)`.
/// A proof is only valid if the artifact matches the current compilation.
#[derive(Debug, Clone)]
pub struct ProofArtifact {
    /// Semantic model version (bumped on any verification semantics change).
    pub semantics_version: u32,
    /// Integer model: "checked" (overflow/div-zero definedness) or "unbounded".
    pub integer_model: String,
    /// Float model: "opaque" (uninterpreted sort, no arithmetic proofs).
    pub float_model: String,
    /// Solver name and version (e.g. "z3-4.13.0").
    pub solver_version: String,
    /// BLAKE3 hash of the source file (tamper detection).
    pub source_hash: String,
    /// BLAKE3 hash of the Resolved IR (semantic identity).
    pub resolved_ir_hash: String,
    /// BLAKE3 hash of the VIR (verification-ir identity, span-free).
    pub vir_hash: String,
}

impl ProofArtifact {
    /// Current semantics version. Bump when verification semantics change.
    pub const SEMANTICS_VERSION: u32 = 1;

    /// Create a new artifact with the current semantics version.
    pub fn new(solver_version: String, source_hash: String) -> Self {
        Self {
            semantics_version: Self::SEMANTICS_VERSION,
            // P0-9: i32 has checked overflow/div-zero definedness; i64 is
            // modeled as unbounded integers with NO definedness checks.
            // "checked" was a lie for i64 — use "checked_i32" to be precise.
            integer_model: "checked_i32".to_string(),
            float_model: "opaque".to_string(),
            solver_version,
            source_hash,
            resolved_ir_hash: String::new(),
            vir_hash: String::new(),
        }
    }

    /// Check if this artifact is compatible with the current compilation.
    pub fn is_compatible(&self, current: &ProofArtifact) -> bool {
        self.semantics_version == current.semantics_version
            && self.integer_model == current.integer_model
            && self.float_model == current.float_model
            && self.solver_version == current.solver_version
            && self.vir_hash == current.vir_hash
    }

    /// Proof cache key: `(semantics_version, solver_version, integer_model, vir_hash)`.
    /// Two proofs with the same key are interchangeable.
    pub fn cache_key(&self) -> String {
        format!(
            "v{}:{}:{}:{}",
            self.semantics_version, self.solver_version, self.integer_model, self.vir_hash
        )
    }
}

/// v0.31.25: Compute a semantic hash for proof caching.
///
/// 0.31.28: Uses BLAKE3 for cryptographic tamper detection.
/// BLAKE3 is deterministic across Rust versions and platforms,
/// unlike SipHash (DefaultHasher) which is randomized per-process.
///
/// The input should be a span-free, variable-normalized string representation
/// of the VIR (Verification IR). Variable normalization ensures that
/// renaming local variables does not invalidate the cache.
pub fn compute_semantic_hash(normalized_vir: &str) -> String {
    blake3::hash(normalized_vir.as_bytes()).to_hex().to_string()
}

/// P1-24: Compute a deterministic BLAKE3 hash of the Resolved IR from
/// CheckedProgram. Covers all ResolvedFunction signatures (name, params,
/// return type, effects) — the semantic identity of the program.
///
/// This hash changes when function signatures change, enabling tamper
/// detection at the Resolved IR level (complementing `vir_hash` which
/// covers the VIR level and `source_hash` which covers the source text).
pub fn compute_resolved_ir_hash(program: &crate::core::CheckedProgram) -> String {
    use std::fmt::Write;
    let mut repr = String::new();
    // Sort by qualified_name for deterministic ordering (HashMap iteration
    // order is non-deterministic).
    let mut funcs: Vec<_> = program.functions().values().collect();
    funcs.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    for f in funcs {
        let _ = write!(repr, "fn {}(", f.qualified_name);
        for (i, (name, ty)) in f.params.iter().enumerate() {
            if i > 0 {
                repr.push(',');
            }
            let _ = write!(repr, "{}:{}", name, crate::core::fmt_type(ty));
        }
        let _ = write!(repr, ")->{}", crate::core::fmt_type(&f.ret));
        if !f.effects.is_empty() {
            let _ = write!(repr, " @{}", f.effects.join(","));
        }
        repr.push('\n');
    }
    blake3::hash(repr.as_bytes()).to_hex().to_string()
}

/// Mock verification from CheckedProgram (Z3-unavailable fallback).
/// Replaces the AST-based `mock_verify_file` with a CheckedProgram-based path.
/// Used by verify_checked when Z3 is unavailable (C4 partial, 0.32.27+).
pub(crate) fn mock_verify_checked(
    program: &crate::core::CheckedProgram,
) -> Vec<VerificationResult> {
    let mut results = Vec::new();
    // Functions and callables with contracts.
    for (node_id, callable) in program.callables() {
        let has_contracts = !callable.contracts.is_empty()
            || callable
                .body
                .root
                .statements
                .iter()
                .any(|s| matches!(s.kind, crate::core::ir::ResolvedStmtKind::Math(_)));
        let func_name = program
            .functions()
            .values()
            .find(|f| f.node_id == *node_id)
            .map(|f| f.qualified_name.clone())
            .unwrap_or_else(|| format!("{:?}", node_id));
        results.push(VerificationResult {
            func_name,
            status: VerifStatus::InfrastructureError,
            message: if has_contracts {
                "Z3 solver not available"
            } else {
                "no contracts"
            }
            .into(),
            diagnostic: None,
            duration_us: 0,
            constraint_count: 0,
            artifact: None,
            trusted_subset_domain: None,
        });
    }
    // Extern blocks with contracts (not included in callables).
    for block in program.extern_blocks().values() {
        for signature in &block.signatures {
            if signature.requires.is_some() || signature.ensures.is_some() {
                results.push(VerificationResult {
                    func_name: format!("extern {}", signature.name),
                    status: VerifStatus::InfrastructureError,
                    message: "Z3 solver not available".into(),
                    diagnostic: None,
                    duration_us: 0,
                    constraint_count: 0,
                    artifact: None,
                    trusted_subset_domain: None,
                });
            }
        }
    }
    results
}

/// Normalize a VIR string for semantic hashing:
/// - Strip span annotations (line:col references)
/// - Canonicalize variable names (%0, %1, %2, ...)
/// - Remove comments and whitespace variations
///
/// This ensures that cosmetically different but semantically identical
/// VIRs produce the same hash.
#[allow(dead_code)] // Infrastructure for proof cache (0.31.25-6 门禁)
pub fn normalize_vir_for_hash(vir: &str) -> String {
    let mut result = String::with_capacity(vir.len());
    let mut var_counter = 0u32;
    let mut var_map = HashMap::new();
    for line in vir.lines() {
        let trimmed = line.trim();
        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with(';') {
            continue;
        }
        // Strip span annotations (e.g., @span:12:5)
        let cleaned: String = trimmed
            .chars()
            .collect::<String>()
            .split("@span:")
            .next()
            .unwrap_or(trimmed)
            .trim()
            .to_string();
        // Canonicalize variable names: %name → %N
        let mut normalized = String::new();
        let mut chars = cleaned.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                let mut name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc.is_alphanumeric() || nc == '_' || nc == '.' {
                        name.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let canonical = var_map.entry(name).or_insert_with(|| {
                    let id = var_counter;
                    var_counter += 1;
                    id
                });
                normalized.push_str(&format!("%{}", canonical));
            } else {
                normalized.push(c);
            }
        }
        result.push_str(&normalized);
        result.push('\n');
    }
    result
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
    /// V-7 (audit 2026-08-05): an EMPTY constraint set means there is no
    /// potential violation to witness — every postcondition holds vacuously —
    /// so the violation question is UNSAT. (The previous `(Sat, None)` return
    /// was semantically inverted: callers interpreting Sat as "a violation is
    /// satisfiable" would reject obligations that were never generated.)
    pub fn check_scope_multi<T: std::borrow::Borrow<z3::ast::Bool>>(
        &mut self,
        constraints: Vec<T>,
    ) -> (SatResult, Option<z3::Model>) {
        if constraints.is_empty() {
            return (SatResult::Unsat, None);
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
    /// P1-24: BLAKE3 hash of the source file (tamper detection).
    /// Computed at the verify_source / verify_checked entry point.
    /// Empty when source text is unavailable (e.g. LSP path).
    pub(crate) source_hash: String,
    /// P1-24: BLAKE3 hash of the Resolved IR (semantic identity).
    /// Computed from CheckedProgram ResolvedFunction signatures.
    pub(crate) resolved_ir_hash: String,
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
    pub(crate) checked_function_returns: std::collections::HashMap<String, String>,
    pub(crate) checked_function_params: std::collections::HashMap<String, Vec<(String, String)>>,
    pub(crate) checked_comptime_functions: std::collections::HashSet<String>,
    /// Flow transition keys materialised from CheckedProgram: "flow::event::source".
    pub(crate) checked_transitions: std::collections::HashSet<String>,
    /// Capability names materialised from CheckedProgram.
    pub(crate) checked_capabilities: std::collections::HashSet<String>,
    pub(crate) checked_capability_combined: std::collections::HashMap<String, String>,
    /// Session names materialised from CheckedProgram.
    pub(crate) checked_sessions: std::collections::HashSet<String>,
    pub(crate) checked_session_displays: std::collections::HashMap<String, String>,
    /// Ownership ledger owners materialised from CheckedProgram.
    pub(crate) checked_ownership_owners: std::collections::HashSet<String>,
    pub(crate) checked_ownership_summaries:
        std::collections::HashMap<String, (usize, usize, usize, usize, usize, bool)>,
    pub(crate) checked_ownership_resources: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_ownership_actions: std::collections::HashMap<String, Vec<(String, String)>>,
    pub(crate) checked_ownership_merges:
        std::collections::HashMap<String, Vec<(String, String, String, String)>>,
    pub(crate) checked_backend_requirements: Vec<(String, String)>,
    pub(crate) checked_node_meta_count: usize,
    pub(crate) checked_node_meta_paths: std::collections::HashSet<String>,
    pub(crate) checked_node_meta_precision: std::collections::HashMap<String, String>,
    pub(crate) checked_node_meta_spans:
        std::collections::HashMap<String, (usize, usize, usize, usize)>,
    /// Type definition names materialised from CheckedProgram.
    pub(crate) checked_type_defs: std::collections::HashSet<String>,
    pub(crate) checked_type_fields: std::collections::HashMap<String, Vec<(String, String)>>,
    pub(crate) checked_type_variants:
        std::collections::HashMap<String, Vec<(String, Option<String>)>>,
    pub(crate) checked_type_aliases: std::collections::HashMap<String, String>,
    /// Extern function names materialised from CheckedProgram.
    pub(crate) checked_extern_funcs: std::collections::HashSet<String>,
    pub(crate) checked_extern_abis: std::collections::HashMap<String, String>,
    pub(crate) checked_extern_signatures: std::collections::HashMap<String, (usize, String)>,
    pub(crate) checked_extern_params: std::collections::HashMap<String, Vec<(String, String)>>,
    pub(crate) checked_extern_no_panic: std::collections::HashSet<String>,
    pub(crate) checked_extern_unsafe: std::collections::HashSet<String>,
    /// Protocol names materialised from CheckedProgram.
    pub(crate) checked_protocols: std::collections::HashSet<String>,
    pub(crate) checked_protocol_transitions:
        std::collections::HashMap<String, Vec<(String, String, String)>>,
    pub(crate) checked_protocol_payloads: std::collections::HashMap<String, String>,
    pub(crate) checked_protocol_states: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_protocol_state_payloads: std::collections::HashMap<String, (String, String)>,
    /// Trait names materialised from CheckedProgram.
    pub(crate) checked_traits: std::collections::HashSet<String>,
    pub(crate) checked_method_signatures: std::collections::HashMap<String, (usize, String)>,
    /// Trait/impl method parameter directories: "TraitName.Method" -> [(param_name, type display)].
    pub(crate) checked_method_params: std::collections::HashMap<String, Vec<(String, String)>>,
    /// Actor names materialised from CheckedProgram.
    pub(crate) checked_actors: std::collections::HashSet<String>,
    pub(crate) checked_actor_method_signatures: std::collections::HashMap<String, (usize, String)>,
    pub(crate) checked_actor_method_params:
        std::collections::HashMap<String, Vec<(String, String)>>,
    pub(crate) checked_actor_fields: std::collections::HashMap<String, Vec<(String, String, bool)>>,
    /// Flow mailbox depths materialised from CheckedProgram.
    pub(crate) checked_mailbox_depths: std::collections::HashMap<String, usize>,
    pub(crate) checked_flow_state_payloads:
        std::collections::HashMap<String, Vec<(String, String)>>,
    pub(crate) checked_flow_states: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_flow_events: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_item_kinds: std::collections::HashMap<String, String>,
    /// Flow max_children materialised from CheckedProgram.
    pub(crate) checked_max_children: Option<usize>,
    /// Persistent field sets materialised from CheckedProgram.
    pub(crate) checked_persistent_fields: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_constants: std::collections::HashSet<String>,
    pub(crate) checked_constant_values: std::collections::HashMap<String, (Option<String>, String)>,
    pub(crate) checked_flow_protocols: std::collections::HashMap<String, Vec<String>>,
    pub(crate) checked_fallback_transitions: std::collections::HashSet<String>,
    pub(crate) checked_ffi_pinned_transitions: std::collections::HashSet<String>,
    pub(crate) checked_transition_param_arity: std::collections::HashMap<String, usize>,
    pub(crate) checked_transition_params: std::collections::HashMap<String, Vec<(String, String)>>,
    pub(crate) checked_transitions_by_flow:
        std::collections::HashMap<String, Vec<(String, String, String, bool, bool, usize)>>,
    pub(crate) checked_transitions_by_event:
        std::collections::HashMap<String, Vec<(String, String, String, bool, bool, usize)>>,
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

// Checked-directory queries form the verifier/tooling boundary; the compiler
// binary intentionally uses only a subset in any one target.
#[allow(dead_code)]
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

    /// P1-24: Set the source hash for ProofArtifact tamper detection.
    /// Call before `verify_checked` when source text is available.
    pub fn set_source_hash(&mut self, hash: String) {
        self.ctx.source_hash = hash;
    }

    pub fn verify_checked(
        &mut self,
        program: &crate::core::CheckedProgram,
    ) -> Vec<VerificationResult> {
        self.ctx.checked_function_names = program
            .functions()
            .values()
            .map(|function| function.qualified_name.clone())
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
        self.ctx.checked_function_params = program
            .functions()
            .values()
            .map(|function| {
                (
                    function.qualified_name.clone(),
                    function
                        .params
                        .iter()
                        .map(|(name, ty)| (name.clone(), crate::core::fmt_type(ty)))
                        .collect(),
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
        self.ctx.checked_capability_combined = program
            .capabilities()
            .values()
            .filter_map(|capability| {
                capability
                    .combined_with
                    .as_ref()
                    .map(|combined| (capability.qualified_name.clone(), combined.clone()))
            })
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
            .resource_analyses()
            .keys()
            .map(|owner| owner.0.clone())
            .collect();
        let mut ownership_summaries = std::collections::HashMap::new();
        let mut ownership_resources = std::collections::HashMap::new();
        let mut ownership_actions = std::collections::HashMap::new();
        let mut ownership_merges = std::collections::HashMap::new();
        for (owner, analysis) in program.resource_analyses() {
            let cfg = program.callable_cfg(owner);
            let merges = cfg
                .map(|cfg| analysis.branch_merges(cfg))
                .unwrap_or_default();
            ownership_summaries.insert(
                owner.0.clone(),
                (
                    analysis.action_count(crate::core::CanonicalActionKind::Introduce),
                    analysis.action_count(crate::core::CanonicalActionKind::Move),
                    analysis.action_count(crate::core::CanonicalActionKind::Drop),
                    analysis.action_count(crate::core::CanonicalActionKind::Return),
                    merges.len(),
                    merges
                        .iter()
                        .any(|m| m.merged_state == crate::core::Availability::MaybeConsumed),
                ),
            );
            ownership_resources.insert(owner.0.clone(), analysis.resources());
            ownership_actions.insert(
                owner.0.clone(),
                analysis
                    .actions
                    .iter()
                    .filter(|a| {
                        !matches!(
                            a.kind,
                            crate::core::CanonicalActionKind::Read
                                | crate::core::CanonicalActionKind::Write
                        )
                    })
                    .map(|action| (action.kind.as_str().to_string(), action.resource_display()))
                    .collect(),
            );
            ownership_merges.insert(
                owner.0.clone(),
                merges
                    .iter()
                    .map(|merge| {
                        let encode = |s: crate::core::Availability| match s {
                            crate::core::Availability::Available => "available",
                            crate::core::Availability::Consumed => "consumed",
                            crate::core::Availability::MaybeConsumed => "maybe_consumed",
                        };
                        (
                            merge.resource.clone(),
                            encode(merge.then_state).to_string(),
                            encode(merge.else_state).to_string(),
                            encode(merge.merged_state).to_string(),
                        )
                    })
                    .collect(),
            );
        }
        self.ctx.checked_ownership_summaries = ownership_summaries;
        self.ctx.checked_ownership_resources = ownership_resources;
        self.ctx.checked_ownership_actions = ownership_actions;
        self.ctx.checked_ownership_merges = ownership_merges;

        self.ctx.checked_backend_requirements = program
            .backend_requirements()
            .iter()
            .map(|req| (req.capability.to_string(), req.flow.0.clone()))
            .collect();
        self.ctx.checked_node_meta_count = program.node_meta().len();
        self.ctx.checked_node_meta_paths = program
            .node_meta()
            .keys()
            .map(|node_id| node_id.0.clone())
            .collect();
        let mut node_meta_precision = std::collections::HashMap::new();
        for (node_id, meta) in program.node_meta() {
            let precision = match meta.precision {
                crate::core::SpanPrecision::Exact => "exact",
                crate::core::SpanPrecision::SourceAnchor => "source_anchor",
                crate::core::SpanPrecision::DeclarationFallback => "declaration_fallback",
            };
            node_meta_precision.insert(node_id.0.clone(), precision.to_string());
        }
        self.ctx.checked_node_meta_precision = node_meta_precision;
        let mut node_meta_spans = std::collections::HashMap::new();
        for (node_id, meta) in program.node_meta() {
            let span = meta.origin.user_span();
            node_meta_spans.insert(
                node_id.0.clone(),
                (span.start_line, span.start_col, span.end_line, span.end_col),
            );
        }
        self.ctx.checked_node_meta_spans = node_meta_spans;
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
        let mut extern_params = std::collections::HashMap::new();
        for block in program.extern_blocks().values() {
            for sig in &block.signatures {
                extern_signatures.insert(sig.name.clone(), (sig.params.len(), sig.ret.clone()));
                extern_params.insert(sig.name.clone(), sig.params.clone());
            }
        }
        self.ctx.checked_extern_signatures = extern_signatures;
        self.ctx.checked_extern_params = extern_params;
        let mut extern_no_panic = std::collections::HashSet::new();
        let mut extern_unsafe = std::collections::HashSet::new();
        for block in program.extern_blocks().values() {
            for func in &block.funcs {
                if block.no_panic {
                    extern_no_panic.insert(func.clone());
                }
                if block.unsafe_ {
                    extern_unsafe.insert(func.clone());
                }
            }
        }
        self.ctx.checked_extern_no_panic = extern_no_panic;
        self.ctx.checked_extern_unsafe = extern_unsafe;
        self.ctx.checked_protocols = program
            .protocols()
            .values()
            .map(|protocol| protocol.qualified_name.clone())
            .collect();
        let mut protocol_transitions = std::collections::HashMap::new();
        let mut protocol_payloads = std::collections::HashMap::new();
        let mut protocol_states = std::collections::HashMap::new();
        let mut protocol_state_payloads = std::collections::HashMap::new();
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
            let mut state_names = protocol.states.clone();
            state_names.sort();
            protocol_states.insert(protocol.qualified_name.clone(), state_names);
            for state in &protocol.state_payloads {
                if let Some(ty) = &state.payload_type {
                    protocol_payloads.insert(
                        format!("{}.{}", protocol.qualified_name, state.name),
                        ty.clone(),
                    );
                    protocol_state_payloads.insert(
                        format!("{}.{}", protocol.qualified_name, state.name),
                        (state.payload_name.clone().unwrap_or_default(), ty.clone()),
                    );
                }
            }
        }
        self.ctx.checked_protocol_transitions = protocol_transitions;
        self.ctx.checked_protocol_payloads = protocol_payloads;
        self.ctx.checked_protocol_states = protocol_states;
        self.ctx.checked_protocol_state_payloads = protocol_state_payloads;
        self.ctx.checked_traits = program
            .traits()
            .values()
            .map(|trait_def| trait_def.qualified_name.clone())
            .collect();
        let mut method_signatures = std::collections::HashMap::new();
        let mut method_params = std::collections::HashMap::new();
        for trait_def in program.traits().values() {
            for method in &trait_def.method_signatures {
                let key = format!("{}.{}", trait_def.qualified_name, method.name);
                method_signatures.insert(key.clone(), (method.params.len(), method.ret.clone()));
                method_params.insert(key.clone(), method.params.clone());
            }
        }
        for impl_def in program.impls().values() {
            for method in &impl_def.method_signatures {
                let key = format!("{}.{}", impl_def.qualified_name, method.name);
                method_signatures.insert(key.clone(), (method.params.len(), method.ret.clone()));
                method_params.insert(key.clone(), method.params.clone());
            }
        }
        self.ctx.checked_method_signatures = method_signatures;
        self.ctx.checked_method_params = method_params;
        self.ctx.checked_actors = program
            .actors()
            .values()
            .map(|actor| actor.qualified_name.clone())
            .collect();
        let mut actor_method_signatures = std::collections::HashMap::new();
        let mut actor_method_params = std::collections::HashMap::new();
        for actor in program.actors().values() {
            for method in &actor.method_signatures {
                let key = format!("{}.{}", actor.qualified_name, method.name);
                actor_method_signatures
                    .insert(key.clone(), (method.params.len(), method.ret.clone()));
                actor_method_params.insert(key.clone(), method.params.clone());
            }
        }
        self.ctx.checked_actor_method_signatures = actor_method_signatures;
        self.ctx.checked_actor_method_params = actor_method_params;
        let mut actor_fields = std::collections::HashMap::new();
        for actor in program.actors().values() {
            if !actor.fields.is_empty() {
                actor_fields.insert(
                    actor.qualified_name.clone(),
                    actor
                        .fields
                        .iter()
                        .map(|(name, ty, mut_)| (name.clone(), crate::core::fmt_type(ty), *mut_))
                        .collect(),
                );
            }
        }
        self.ctx.checked_actor_fields = actor_fields;
        let mut mailbox_depths = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if let Some(depth) = flow.mailbox_depth {
                mailbox_depths.insert(flow.id.0.clone(), depth);
            }
        }
        self.ctx.checked_mailbox_depths = mailbox_depths;
        let mut flow_state_payloads = std::collections::HashMap::new();
        for flow in program.flows().values() {
            for (state_name, state) in &flow.states {
                if !state.payload.is_empty() {
                    flow_state_payloads.insert(
                        format!("{}.{}", flow.id.0, state_name),
                        state
                            .payload
                            .iter()
                            .map(|(name, ty)| (name.clone(), crate::core::fmt_type(ty)))
                            .collect(),
                    );
                }
            }
        }
        self.ctx.checked_flow_state_payloads = flow_state_payloads;
        let mut flow_states = std::collections::HashMap::new();
        for flow in program.flows().values() {
            let mut names: Vec<String> = flow.states.keys().cloned().collect();
            names.sort();
            flow_states.insert(flow.id.0.clone(), names);
        }
        self.ctx.checked_flow_states = flow_states;
        let mut flow_events = std::collections::HashMap::new();
        for flow in program.flows().values() {
            let mut events: Vec<String> = flow
                .transitions
                .iter()
                .map(|tid| tid.event.clone())
                .collect();
            events.sort();
            events.dedup();
            flow_events.insert(flow.id.0.clone(), events);
        }
        self.ctx.checked_flow_events = flow_events;
        let mut item_kinds = std::collections::HashMap::new();
        for item in program.items().values() {
            let kind = match item.kind {
                crate::core::ResolvedItemKind::Function => "function",
                crate::core::ResolvedItemKind::Type => "type",
                crate::core::ResolvedItemKind::Constant => "const",
                crate::core::ResolvedItemKind::Capability => "capability",
                crate::core::ResolvedItemKind::Trait => "trait",
                crate::core::ResolvedItemKind::Impl => "impl",
                crate::core::ResolvedItemKind::ExternBlock => "extern",
                crate::core::ResolvedItemKind::Module => "module",
                crate::core::ResolvedItemKind::Actor => "actor",
                crate::core::ResolvedItemKind::Flow => "flow",
                crate::core::ResolvedItemKind::Protocol => "protocol",
                crate::core::ResolvedItemKind::Session => "session",
            };
            item_kinds.insert(item.qualified_name.clone(), kind.to_string());
        }
        self.ctx.checked_item_kinds = item_kinds;
        self.ctx.checked_max_children = program.flows().values().find_map(|flow| flow.max_children);
        let mut persistent_fields = std::collections::HashMap::new();
        for flow in program.flows().values() {
            if !flow.persistent_fields.is_empty() {
                persistent_fields.insert(flow.id.0.clone(), flow.persistent_fields.clone());
            }
        }
        self.ctx.checked_persistent_fields = persistent_fields;
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

        self.ctx.checked_transition_params = program
            .transitions()
            .values()
            .map(|transition| {
                (
                    format!(
                        "{}::{}::{}",
                        transition.id.flow.0, transition.id.event, transition.id.source.name
                    ),
                    transition
                        .params
                        .iter()
                        .map(|(name, ty)| (name.clone(), crate::core::fmt_type(ty)))
                        .collect(),
                )
            })
            .collect();

        let mut transitions_by_flow: std::collections::HashMap<
            String,
            Vec<(String, String, String, bool, bool, usize)>,
        > = std::collections::HashMap::new();
        for transition in program.transitions().values() {
            let flow = transition.id.flow.0.clone();
            let event = transition.id.event.clone();
            let source = transition.id.source.name.clone();
            let targets = transition
                .targets
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join("|");
            transitions_by_flow.entry(flow).or_default().push((
                event,
                source,
                targets,
                transition.is_fallback,
                transition.is_ffi_pinned,
                transition.params.len(),
            ));
        }
        for list in transitions_by_flow.values_mut() {
            list.sort();
        }
        let mut transitions_by_event: std::collections::HashMap<
            String,
            Vec<(String, String, String, bool, bool, usize)>,
        > = std::collections::HashMap::new();
        for transition in program.transitions().values() {
            let flow = transition.id.flow.0.clone();
            let event = transition.id.event.clone();
            let source = transition.id.source.name.clone();
            let targets = transition
                .targets
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join("|");
            transitions_by_event.entry(event).or_default().push((
                flow,
                source,
                targets,
                transition.is_fallback,
                transition.is_ffi_pinned,
                transition.params.len(),
            ));
        }
        for list in transitions_by_event.values_mut() {
            list.sort();
        }
        self.ctx.checked_transitions_by_flow = transitions_by_flow;
        self.ctx.checked_transitions_by_event = transitions_by_event;
        // P1-24: compute Resolved IR hash from CheckedProgram signatures.
        self.ctx.resolved_ir_hash = compute_resolved_ir_hash(program);
        // v0.32.27: use Resolved IR path instead of AST-based verify_file.
        self.verify_checked_contracts(program)
    }

    pub(crate) fn has_checked_function(&self, name: &str) -> bool {
        self.ctx.checked_function_names.contains(name)
    }

    pub(crate) fn checked_function_return_type(&self, name: &str) -> Option<&str> {
        self.ctx
            .checked_function_returns
            .get(name)
            .map(String::as_str)
    }

    pub(crate) fn checked_function_params(&self, name: &str) -> Option<Vec<(String, String)>> {
        self.ctx.checked_function_params.get(name).cloned()
    }

    pub(crate) fn is_checked_comptime_function(&self, name: &str) -> bool {
        self.ctx.checked_comptime_functions.contains(name)
    }

    pub(crate) fn has_checked_transition(&self, flow: &str, event: &str, source: &str) -> bool {
        self.ctx
            .checked_transitions
            .contains(&format!("{}::{}::{}", flow, event, source))
    }

    pub(crate) fn has_checked_capability(&self, name: &str) -> bool {
        self.ctx.checked_capabilities.contains(name)
    }

    pub(crate) fn checked_capability_combined_with(&self, name: &str) -> Option<&str> {
        self.ctx
            .checked_capability_combined
            .get(name)
            .map(String::as_str)
    }

    pub(crate) fn has_checked_session(&self, name: &str) -> bool {
        self.ctx.checked_sessions.contains(name)
    }

    pub(crate) fn checked_session_display(&self, name: &str) -> Option<&str> {
        self.ctx
            .checked_session_displays
            .get(name)
            .map(String::as_str)
    }

    pub(crate) fn has_checked_ownership_owner(&self, owner: &str) -> bool {
        self.ctx.checked_ownership_owners.contains(owner)
    }

    pub(crate) fn checked_backend_requirements(&self) -> &[(String, String)] {
        &self.ctx.checked_backend_requirements
    }

    pub(crate) fn checked_node_meta_count(&self) -> usize {
        self.ctx.checked_node_meta_count
    }

    pub(crate) fn has_checked_node_meta_path(&self, path: &str) -> bool {
        self.ctx.checked_node_meta_paths.contains(path)
    }

    pub(crate) fn checked_node_meta_precision(&self, path: &str) -> Option<&str> {
        self.ctx
            .checked_node_meta_precision
            .get(path)
            .map(String::as_str)
    }

    pub(crate) fn checked_node_meta_span(
        &self,
        path: &str,
    ) -> Option<(usize, usize, usize, usize)> {
        self.ctx.checked_node_meta_spans.get(path).copied()
    }

    pub(crate) fn requires_checked_capability(&self, capability: &str) -> bool {
        self.ctx
            .checked_backend_requirements
            .iter()
            .any(|(cap, _)| cap == capability)
    }

    pub(crate) fn checked_ownership_summary(
        &self,
        owner: &str,
    ) -> Option<(usize, usize, usize, usize, usize, bool)> {
        self.ctx.checked_ownership_summaries.get(owner).copied()
    }

    pub(crate) fn checked_ownership_resources(&self, owner: &str) -> Option<Vec<String>> {
        self.ctx.checked_ownership_resources.get(owner).cloned()
    }

    pub(crate) fn checked_ownership_actions(&self, owner: &str) -> Option<Vec<(String, String)>> {
        self.ctx.checked_ownership_actions.get(owner).cloned()
    }

    pub(crate) fn checked_ownership_merges(
        &self,
        owner: &str,
    ) -> Option<Vec<(String, String, String, String)>> {
        self.ctx.checked_ownership_merges.get(owner).cloned()
    }

    pub(crate) fn has_checked_type_def(&self, name: &str) -> bool {
        self.ctx.checked_type_defs.contains(name)
    }

    pub(crate) fn checked_type_fields(&self, name: &str) -> Option<Vec<(String, String)>> {
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

    pub(crate) fn checked_extern_params(&self, name: &str) -> Option<Vec<(String, String)>> {
        self.ctx.checked_extern_params.get(name).cloned()
    }

    pub(crate) fn is_checked_extern_no_panic(&self, name: &str) -> bool {
        self.ctx.checked_extern_no_panic.contains(name)
    }

    pub(crate) fn is_checked_extern_unsafe(&self, name: &str) -> bool {
        self.ctx.checked_extern_unsafe.contains(name)
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

    pub(crate) fn checked_protocol_payload(&self, protocol: &str, state: &str) -> Option<String> {
        self.ctx
            .checked_protocol_payloads
            .get(&format!("{protocol}.{state}"))
            .cloned()
    }

    pub(crate) fn checked_protocol_states(&self, protocol: &str) -> Option<Vec<String>> {
        self.ctx.checked_protocol_states.get(protocol).cloned()
    }

    pub(crate) fn checked_protocol_state_payload(
        &self,
        protocol: &str,
        state: &str,
    ) -> Option<(String, String)> {
        self.ctx
            .checked_protocol_state_payloads
            .get(&format!("{protocol}.{state}"))
            .cloned()
    }

    pub(crate) fn has_checked_trait(&self, name: &str) -> bool {
        self.ctx.checked_traits.contains(name)
    }

    pub(crate) fn checked_method_signature(&self, key: &str) -> Option<(usize, String)> {
        self.ctx.checked_method_signatures.get(key).cloned()
    }

    pub(crate) fn checked_method_params(&self, key: &str) -> Option<Vec<(String, String)>> {
        self.ctx.checked_method_params.get(key).cloned()
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

    pub(crate) fn checked_actor_method_params(
        &self,
        actor: &str,
        method: &str,
    ) -> Option<Vec<(String, String)>> {
        self.ctx
            .checked_actor_method_params
            .get(&format!("{actor}.{method}"))
            .cloned()
    }

    pub(crate) fn checked_actor_fields(&self, actor: &str) -> Option<Vec<(String, String, bool)>> {
        self.ctx.checked_actor_fields.get(actor).cloned()
    }

    pub(crate) fn checked_mailbox_depth(&self, flow_name: &str) -> Option<usize> {
        self.ctx
            .checked_mailbox_depths
            .get(flow_name)
            .copied()
            .or_else(|| {
                self.ctx
                    .checked_mailbox_depths
                    .iter()
                    .find_map(|(qualified, depth)| {
                        qualified
                            .rsplit("::")
                            .next()
                            .filter(|bare| *bare == flow_name)
                            .map(|_| *depth)
                    })
            })
    }

    pub(crate) fn checked_flow_state_payload(
        &self,
        flow: &str,
        state: &str,
    ) -> Option<Vec<(String, String)>> {
        self.ctx
            .checked_flow_state_payloads
            .get(&format!("{flow}.{state}"))
            .cloned()
    }

    pub(crate) fn checked_flow_states(&self, flow: &str) -> Option<Vec<String>> {
        self.ctx.checked_flow_states.get(flow).cloned()
    }

    pub(crate) fn checked_flow_events(&self, flow: &str) -> Option<Vec<String>> {
        self.ctx.checked_flow_events.get(flow).cloned()
    }

    pub(crate) fn checked_item_kind(&self, name: &str) -> Option<&str> {
        self.ctx.checked_item_kinds.get(name).map(String::as_str)
    }

    pub(crate) fn checked_max_children(&self) -> Option<usize> {
        self.ctx.checked_max_children
    }

    pub(crate) fn checked_persistent_fields(&self, flow_name: &str) -> Option<Vec<String>> {
        self.lookup_checked_field_set(&self.ctx.checked_persistent_fields, flow_name)
    }

    pub(crate) fn has_checked_constant(&self, name: &str) -> bool {
        self.ctx.checked_constants.contains(name)
    }

    pub(crate) fn checked_constant_value(&self, name: &str) -> Option<(Option<String>, String)> {
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

    pub(crate) fn checked_transitions_for_flow(
        &self,
        flow: &str,
    ) -> Option<Vec<(String, String, String, bool, bool, usize)>> {
        self.ctx.checked_transitions_by_flow.get(flow).cloned()
    }

    pub(crate) fn checked_transitions_for_event(
        &self,
        event: &str,
    ) -> Option<Vec<(String, String, String, bool, bool, usize)>> {
        self.ctx.checked_transitions_by_event.get(event).cloned()
    }

    pub(crate) fn checked_transition_params(
        &self,
        flow: &str,
        event: &str,
        source: &str,
    ) -> Option<Vec<(String, String)>> {
        self.ctx
            .checked_transition_params
            .get(&format!("{}::{}::{}", flow, event, source))
            .cloned()
    }

    pub(crate) fn verify_file(&mut self, file: &File) -> Vec<VerificationResult> {
        let mut results = Vec::new();
        VerifierCtx::verify_items(&mut self.ctx, &mut self.session, &file.items, &mut results);
        results
    }

    /// 0.32.27: Verify contracts from Resolved IR (CheckedProgram.callables).
    ///
    /// Replaces the AST-based `verify_file` path for function contracts.
    /// Iterates `program.callables()` and uses `verify_contracts_from_resolved`
    /// from `resolved_expr.rs` to encode requires/ensures/math as Z3 constraints.
    ///
    /// Does NOT handle extern block contracts (those still use the AST path
    /// through `verify_ffi_checked` / `flow_verify_ffi_call_sites_with_externs`).
    ///
    /// AU-V1 (full-audit-2026-08-05 §11, VERIFIED CRITICAL): the solver scope
    /// is reset before EACH callable. `verify_contracts_from_resolved` asserts
    /// its base constraints — requires, `result == body`, i32 range bounds,
    /// `old(param) == param`, proven `math:` lemmas — directly into the
    /// session (only the individual ensures checks are push/pop scoped).
    /// Every callable also reuses the same Z3 const names (`result` and the
    /// parameter display names). Without a reset, function B was proved under
    /// function A's assumptions → spurious Proven. This also clears state
    /// left over from earlier requests on a long-lived session (LSP keeps one
    /// `Verifier` across requests, lsp/state.rs), so no separate per-request
    /// reset hook is needed: the first contract-bearing callable of every
    /// request starts from a clean solver. Mirrors the per-function
    /// `session.reset()` already used by the AST engines (func.rs
    /// `verify_items_collect`, flow.rs `FlowEvent::Step`).
    pub(crate) fn verify_checked_contracts(
        &mut self,
        program: &crate::core::CheckedProgram,
    ) -> Vec<VerificationResult> {
        let mut results = Vec::new();
        for (node_id, callable) in program.callables() {
            if callable.contracts.is_empty()
                && !crate::verifier::resolved_expr::has_math_obligations(callable)
            {
                continue;
            }
            let func_name = program
                .functions()
                .values()
                .find(|f| f.node_id == *node_id)
                .map(|f| f.qualified_name.clone())
                .unwrap_or_else(|| format!("{:?}", node_id));

            // AU-V1: per-callable isolation — see doc comment above.
            // `reset()` clears all assertions, returns the solver to Z3 depth
            // 0, and clears poisoned/replaced flags; safe on any session
            // state (same primitive the other engines use per function).
            self.session.reset();

            let start = std::time::Instant::now();
            let outcome = crate::verifier::resolved_expr::verify_contracts_from_resolved(
                callable,
                program.resolved_types(),
                &mut self.session,
            );
            let duration_us = start.elapsed().as_micros() as u64;

            let Some((status, message)) = outcome else {
                continue;
            };

            // V-7 (audit 2026-08-05): the Resolved engine previously emitted
            // `artifact: None` for every result — the P1-24 tamper binding was
            // silently disabled on the LSP / `--dump-z3` paths. Bind what this
            // engine can bind: solver identity, the semantic model labels, the
            // source hash and the Resolved IR hash.
            // KNOWN GAP (documented, wiring out of scope): `vir_hash` stays
            // empty — this engine verifies from Resolved IR, not VIR, so full
            // cross-engine proof-cache identity is not achievable until VIR
            // identities are plumbed through. `source_hash` is also empty on
            // the LSP path (no source text handed to the Verifier there), so
            // tamper detection degrades to `resolved_ir_hash` identity only.
            let artifact = Some(ProofArtifact {
                semantics_version: ProofArtifact::SEMANTICS_VERSION,
                // H-24 (audit): i32 definedness VCs are now enforced on this
                // path, so "checked_i32" is honest here (i64 stays unbounded).
                integer_model: "checked_i32".to_string(),
                // H-21 (audit): f64 is rejected fail-closed on this path —
                // no float model is ever assumed.
                float_model: "f64_rejected".to_string(),
                solver_version: format!("z3 {}", z3::full_version()),
                source_hash: self.ctx.source_hash.clone(),
                resolved_ir_hash: self.ctx.resolved_ir_hash.clone(),
                vir_hash: String::new(),
            });

            results.push(VerificationResult {
                func_name,
                status,
                message,
                diagnostic: None,
                duration_us,
                constraint_count: 1,
                artifact,
                trusted_subset_domain: None,
            });
        }
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

/// V-3 (audit 2026-08-05): join a module prefix and an item name into the
/// qualified name used as the `func_defs` / `func_status` key.
fn qualified_item_name(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}::{}", prefix, name)
    }
}

impl VerifierCtx {
    pub fn collect_func_defs(&mut self, items: &[Item]) {
        self.collect_func_defs_prefixed(items, "");
    }

    /// V-3 (audit 2026-08-05): functions are keyed by QUALIFIED name
    /// (`module::…::func`). The checker rejects duplicate definitions per
    /// qualified name, so keying by bare name let two same-named functions
    /// in different modules (or a module function and a top-level function)
    /// silently overwrite each other — a call to the top-level function then
    /// picked up the unrelated module function's ensures as axioms and
    /// produced fake Proven. Module-nested callees are not reachable through
    /// bare identifiers anyway; qualified keys keep their axioms inert until
    /// a qualified call mechanism exists.
    fn collect_func_defs_prefixed(&mut self, items: &[Item], prefix: &str) {
        for item in items {
            match item {
                Item::Func(f) => {
                    let qualified = qualified_item_name(prefix, &f.name);
                    self.func_defs.insert(qualified, f.clone());
                }
                Item::Module(m) => {
                    let qualified = qualified_item_name(prefix, &m.name);
                    self.collect_func_defs_prefixed(&m.items, &qualified);
                }
                // V-H6: register actor/impl/flow methods for call-site ensures
                // lookup. V-3: qualified keys only — the previous bare-name
                // insertion let same-named methods across actors pollute each
                // other identically to the module case.
                Item::Actor(a) => {
                    for m in &a.methods {
                        let mut f = m.clone();
                        f.name = format!("{}::{}", a.name, m.name);
                        let qualified = qualified_item_name(prefix, &f.name);
                        self.func_defs.insert(qualified, f);
                    }
                }
                Item::Impl(i) => {
                    for m in &i.methods {
                        let mut f = m.clone();
                        f.name = format!("{}::{}::{}", i.type_name, i.trait_name, m.name);
                        let qualified = qualified_item_name(prefix, &f.name);
                        self.func_defs.insert(qualified, f);
                    }
                }
                Item::Flow(flow) => {
                    for t in &flow.transitions {
                        if let Some(body) = &t.body {
                            let f = crate::ast::FuncDef {
                                meta: crate::ast::AstNodeMeta::inherited(
                                    t.meta.span,
                                    crate::ast::AstOrigin::RuntimeSystem(
                                        "verifier.transition_function",
                                    ),
                                ),
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
                                has_requires: false,
                                has_ensures: false,
                                has_mutate_params: false,
                            };
                            let qualified = qualified_item_name(prefix, &f.name);
                            self.func_defs.insert(qualified, f);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 0.31.42: Z3 error translation tests ──

    #[test]
    fn verif_status_plain_language_all_variants() {
        let statuses = [
            VerifStatus::Proven,
            VerifStatus::Disproven,
            VerifStatus::NotInTrustedSubset,
            VerifStatus::SolverUnknown,
            VerifStatus::Timeout,
            VerifStatus::InfrastructureError,
            VerifStatus::RuntimeOnlyContract,
            VerifStatus::NoObligations,
        ];
        for status in &statuses {
            let desc = status.plain_language();
            assert!(!desc.is_empty(), "{:?} has empty plain_language", status);
            // Should not contain obscure SMT jargon (Z3 is OK — it's actionable)
            assert!(
                !desc.contains("quantifier") && !desc.contains("instantiation"),
                "{:?} plain_language contains jargon: {}",
                status,
                desc
            );
        }
    }

    #[test]
    fn verif_status_hints() {
        // Proven and NoObligations need no action
        assert!(VerifStatus::Proven.hint().is_none());
        assert!(VerifStatus::NoObligations.hint().is_none());

        // All others have actionable hints
        assert!(VerifStatus::Disproven.hint().is_some());
        assert!(VerifStatus::NotInTrustedSubset.hint().is_some());
        assert!(VerifStatus::SolverUnknown.hint().is_some());
        assert!(VerifStatus::Timeout.hint().is_some());
        assert!(VerifStatus::InfrastructureError.hint().is_some());
        assert!(VerifStatus::RuntimeOnlyContract.hint().is_some());
    }

    #[test]
    fn verif_status_icons() {
        assert_eq!(VerifStatus::Proven.icon(), "✓");
        assert_eq!(VerifStatus::Disproven.icon(), "✗");
        assert_eq!(VerifStatus::NotInTrustedSubset.icon(), "⊘");
        assert_eq!(VerifStatus::SolverUnknown.icon(), "?");
        assert_eq!(VerifStatus::Timeout.icon(), "⏱");
        assert_eq!(VerifStatus::InfrastructureError.icon(), "⚠");
        assert_eq!(VerifStatus::RuntimeOnlyContract.icon(), "↻");
        assert_eq!(VerifStatus::NoObligations.icon(), "·");
    }

    #[test]
    fn verif_status_disproven_message_is_actionable() {
        let desc = VerifStatus::Disproven.plain_language();
        assert!(desc.contains("inputs"), "should mention inputs");
        assert!(desc.contains("requires"), "should mention requires");
        assert!(desc.contains("ensures"), "should mention ensures");

        let hint = VerifStatus::Disproven.hint().unwrap();
        assert!(
            hint.contains("counterexample"),
            "hint should mention counterexample"
        );
    }

    #[test]
    fn verif_status_backward_compat_aliases() {
        assert_eq!(VerifStatus::Verified, VerifStatus::Proven);
        assert_eq!(VerifStatus::Failed, VerifStatus::Disproven);
    }

    // ── 0.31.43: Resolved IR contract extraction consistency ──

    #[test]
    fn resolved_contracts_match_ast_extraction() {
        // Verify that ResolvedCallable.contracts (from Resolved IR) matches
        // the contract extraction from raw AST (the current verifier path).
        // This proves the Resolved IR contract infrastructure is correct and
        // ready for the verifier to consume directly.
        let source = r#"
func safe_div(a: i32, b: i32) -> i32 {
    requires: b != 0
    ensures: result * b == a
    a / b
}
func no_contracts(x: i32) -> i32 { x + 1 }
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        // Extract contracts from raw AST (current verifier path)
        let mut ast_contracts: Vec<(String, Vec<&str>)> = Vec::new();
        for item in &file.items {
            if let crate::ast::Item::Func(f) = item {
                let mut kinds = Vec::new();
                for stmt in &f.body {
                    match stmt.unlocated() {
                        crate::ast::Stmt::Requires(..) => kinds.push("requires"),
                        crate::ast::Stmt::Ensures(..) => kinds.push("ensures"),
                        crate::ast::Stmt::Invariant(..) => kinds.push("invariant"),
                        _ => {}
                    }
                }
                if !kinds.is_empty() {
                    ast_contracts.push((f.name.clone(), kinds));
                }
            }
        }

        // Extract contracts from Resolved IR (new path)
        let mut resolved_contracts: Vec<(String, Vec<&str>)> = Vec::new();
        for (owner, callable) in program.callables() {
            if callable.contracts.is_empty() {
                continue;
            }
            let name = owner
                .0
                .strip_prefix("function:")
                .unwrap_or(&owner.0)
                .to_string();
            let kinds: Vec<&str> = callable
                .contracts
                .iter()
                .map(|c| match c.kind {
                    crate::core::ir::ContractKind::Requires => "requires",
                    crate::core::ir::ContractKind::Ensures => "ensures",
                    crate::core::ir::ContractKind::Invariant => "invariant",
                })
                .collect();
            resolved_contracts.push((name, kinds));
        }

        // Sort both for comparison
        ast_contracts.sort();
        resolved_contracts.sort();

        assert_eq!(
            ast_contracts.len(),
            resolved_contracts.len(),
            "contract count mismatch: AST={} Resolved={}\nAST: {:?}\nResolved: {:?}",
            ast_contracts.len(),
            resolved_contracts.len(),
            ast_contracts,
            resolved_contracts
        );
        for (ast, resolved) in ast_contracts.iter().zip(&resolved_contracts) {
            assert_eq!(
                ast.0, resolved.0,
                "function name mismatch: AST={} Resolved={}",
                ast.0, resolved.0
            );
            assert_eq!(
                ast.1, resolved.1,
                "contract kinds mismatch for {}: AST={:?} Resolved={:?}",
                ast.0, ast.1, resolved.1
            );
        }
    }

    #[test]
    fn resolved_contracts_cover_math_statements() {
        // Math statements are collected separately in the AST path but
        // appear as ResolvedStmtKind::Math in the Resolved IR body.
        let source = r#"
func with_math(x: i32) -> i32 {
    requires: x > 0
    math: { x * x >= 0 }
    x * x
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        let owner = crate::core::NodeId("function:with_math".into());
        let callable = program.callables().get(&owner).expect("with_math callable");

        // Should have at least the requires contract
        assert!(
            callable
                .contracts
                .iter()
                .any(|c| c.kind == crate::core::ir::ContractKind::Requires),
            "requires contract must be present in Resolved IR"
        );

        // Math statements are in the body, not in contracts
        let body = program.resolved_body(&owner).expect("with_math body");
        let has_math = body
            .root
            .statements
            .iter()
            .any(|s| matches!(s.kind, crate::core::ResolvedStmtKind::Math(_)));
        assert!(
            has_math,
            "math statement must be present in Resolved IR body"
        );
    }
}
