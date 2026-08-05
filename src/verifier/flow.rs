use crate::ast::*;
use crate::verifier::ctx::{SolverSession, VerificationResult, VerifierCtx};
use crate::verifier::helpers;

/// Flow event driving the verifier state machine.
#[derive(Debug, Clone, Copy)]
pub enum FlowEvent {
    /// Verify the next queued function (yield per function).
    Step,
}

/// Accumulated output from the verifier flow.
#[derive(Debug, Clone)]
pub struct FlowAcc {
    pub results: Vec<VerificationResult>,
    pub errors: Vec<String>,
}

impl FlowAcc {
    fn new() -> Self {
        FlowAcc {
            results: Vec::new(),
            errors: Vec::new(),
        }
    }
}

/// Pre-collected step: either a function body or an extern function with contracts.
#[derive(Debug, Clone)]
pub enum StepKind {
    Func(FuncDef),
    Extern(ExternFunc),
}

/// Verifier state machine — strict Flow.
///
/// Each transition processes exactly one function (body or extern) and yields
/// the next state. The Z3 solver is owned by the state (SolverSession) and is
/// only mutated inside transition(), eliminating `&mut self` at the
/// state-machine level.
pub enum VerifierState {
    Ready {
        session: SolverSession,
        ctx: VerifierCtx,
        queue: Vec<StepKind>,
        acc: FlowAcc,
    },
    Done(FlowAcc),
}

impl VerifierState {
    /// Create initial Ready state from a parsed File AST.
    /// Collects func_defs and flattens all items into the verification queue.
    pub fn new(file: &File) -> Result<Self, String> {
        Self::with_hashes(file, String::new(), String::new())
    }

    /// Create initial Ready state with proof hashes for ProofArtifact (P1-24).
    pub fn with_hashes(
        file: &File,
        source_hash: String,
        resolved_ir_hash: String,
    ) -> Result<Self, String> {
        let mut session = SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS)?;
        let mut ctx = VerifierCtx {
            source_hash,
            resolved_ir_hash,
            ..VerifierCtx::default()
        };
        ctx.collect_func_defs(&file.items);
        // V-C4 source-order independence: pre-seed func_status so callers can
        // trust callees defined later in the file.
        ctx.preseed_func_status(&mut session, &file.items);
        let queue = flatten_items(&file.items);
        Ok(VerifierState::Ready {
            session,
            ctx,
            queue,
            acc: FlowAcc::new(),
        })
    }

    /// Create Ready state with a specific Z3 timeout (milliseconds).
    pub fn with_timeout(file: &File, timeout_ms: u64) -> Result<Self, String> {
        let mut session = SolverSession::new(timeout_ms)?;
        let mut ctx = VerifierCtx::default();
        ctx.collect_func_defs(&file.items);
        ctx.preseed_func_status(&mut session, &file.items);
        let queue = flatten_items(&file.items);
        Ok(VerifierState::Ready {
            session,
            ctx,
            queue,
            acc: FlowAcc::new(),
        })
    }

    /// Transition: process one function (body or extern) per Step event.
    /// Uses `self` by value — ownership moves in and out.
    pub fn transition(self, event: FlowEvent) -> Result<Self, String> {
        match (self, event) {
            (
                VerifierState::Ready {
                    mut session,
                    mut ctx,
                    mut queue,
                    mut acc,
                },
                FlowEvent::Step,
            ) => match queue.pop() {
                Some(StepKind::Func(func)) => {
                    if !func.body.is_empty() {
                        session.reset();
                        let result = ctx.verify_func(&mut session, &func);
                        // V-C4: record status for later callers that trust ensures.
                        ctx.func_status
                            .insert(func.name.clone(), result.status.clone());
                        acc.results.push(result);
                    }
                    Ok(VerifierState::Ready {
                        session,
                        ctx,
                        queue,
                        acc,
                    })
                }
                Some(StepKind::Extern(func)) => {
                    if func.requires.is_some() || func.ensures.is_some() {
                        session.reset();
                        let result = ctx.verify_extern_func(&mut session, &func);
                        ctx.func_status
                            .insert(func.name.clone(), result.status.clone());
                        acc.results.push(result);
                    }
                    Ok(VerifierState::Ready {
                        session,
                        ctx,
                        queue,
                        acc,
                    })
                }
                None => Ok(VerifierState::Done(acc)),
            },
            (done @ VerifierState::Done(_), _) => Ok(done),
        }
    }

    /// True if the machine has reached terminal state.
    pub fn is_done(&self) -> bool {
        matches!(self, VerifierState::Done(_))
    }

    /// Consume and extract final accumulator.
    pub fn into_output(self) -> FlowAcc {
        match self {
            VerifierState::Done(acc) => acc,
            VerifierState::Ready { acc, .. } => acc,
        }
    }
}

/// Driver: run the verifier to completion with proof hashes (P1-24).
///
/// V-4 (audit 2026-08-05): verification results must not depend on SOURCE
/// ORDER. The single preseed+pass schedule lost callee axioms permanently
/// when a caller preceded its callee in the file (chain C→B→A declared
/// [C,B,A] ⇒ C stayed Disproven forever). The driver now iterates full
/// waves over the same queue until `func_status` stops changing (fixpoint),
/// so callee proofs propagate bottom-up regardless of declaration order.
pub fn flow_verify_file_with_hashes(
    file: &File,
    source_hash: String,
    resolved_ir_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let state = VerifierState::with_hashes(file, source_hash, resolved_ir_hash)?;
    match state {
        VerifierState::Ready {
            session,
            ctx,
            queue,
            acc,
        } => {
            let acc = verify_queue_to_fixpoint(session, ctx, queue, acc)?;
            Ok(acc.results)
        }
        VerifierState::Done(acc) => Ok(acc.results),
    }
}

/// V-4: process the full queue in waves until `func_status` is stable.
///
/// Each wave re-verifies every step in source order, updating `func_status`
/// as it goes; a callee proved in wave N becomes an available axiom to its
/// callers in wave N+1. The wave count is capped at `steps + 1` — a call
/// chain longer than the number of steps cannot exist, so the loop always
/// terminates. Only the final wave's results are reported; earlier waves are
/// scheduling scaffolding.
fn verify_queue_to_fixpoint(
    mut session: SolverSession,
    mut ctx: VerifierCtx,
    queue: Vec<StepKind>,
    mut acc: FlowAcc,
) -> Result<FlowAcc, String> {
    let max_waves = queue.len() + 1;
    for _ in 0..max_waves {
        let status_before = ctx.func_status.clone();
        acc.results.clear();
        let mut wave_queue = queue.clone();
        while let Some(step) = wave_queue.pop() {
            match step {
                StepKind::Func(func) => {
                    if !func.body.is_empty() {
                        session.reset();
                        let result = ctx.verify_func(&mut session, &func);
                        ctx.func_status
                            .insert(func.name.clone(), result.status.clone());
                        acc.results.push(result);
                    }
                }
                StepKind::Extern(func) => {
                    if func.requires.is_some() || func.ensures.is_some() {
                        session.reset();
                        let result = ctx.verify_extern_func(&mut session, &func);
                        ctx.func_status
                            .insert(func.name.clone(), result.status.clone());
                        acc.results.push(result);
                    }
                }
            }
        }
        if ctx.func_status == status_before {
            break; // fixpoint reached — verdicts no longer depend on ordering
        }
    }
    Ok(acc)
}

/// Drive the state machine until Done. Single-pass stepping retained for the
/// `VerifierState` API and its tests; production verification uses the
/// fixpoint driver (`flow_verify_file_with_hashes`).
#[allow(dead_code)]
fn run_to_done(mut state: VerifierState) -> Result<VerifierState, String> {
    loop {
        state = state.transition(FlowEvent::Step)?;
        if state.is_done() {
            break;
        }
    }
    Ok(state)
}

/// Flatten nested items into a linear queue of functions (body + extern).
/// Items are stored in reverse order so that `pop()` returns them in source order.
fn flatten_items(items: &[Item]) -> Vec<StepKind> {
    let mut queue = Vec::new();
    flatten_items_prefixed(items, "", &mut queue);
    queue.reverse(); // pop() yields from end → reverse so first item is at end
    queue
}

/// V-3 (audit 2026-08-05): module-nested functions are queued under their
/// QUALIFIED name (`module::func`), mirroring `collect_func_defs` keying.
/// Bare-name queue entries collided across modules and propagated the wrong
/// `func_status` / result identity.
fn flatten_items_prefixed(items: &[Item], prefix: &str, queue: &mut Vec<StepKind>) {
    fn qualify(prefix: &str, name: &str) -> String {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", prefix, name)
        }
    }
    for item in items {
        match item {
            Item::Func(f) => {
                let mut func = f.clone();
                func.name = qualify(prefix, &func.name);
                queue.push(StepKind::Func(func));
            }
            Item::Module(m) => {
                let qualified = qualify(prefix, &m.name);
                flatten_items_prefixed(&m.items, &qualified, queue);
            }
            // Extern symbols keep their ABI name (no module qualification).
            Item::ExternBlock(block) => {
                for func in &block.funcs {
                    queue.push(StepKind::Extern(func.clone()));
                }
            }
            // V-H6: actor methods, impl methods, flow transitions enter the queue.
            Item::Actor(a) => {
                for m in &a.methods {
                    let mut f = m.clone();
                    f.name = qualify(prefix, &format!("{}::{}", a.name, m.name));
                    queue.push(StepKind::Func(f));
                }
            }
            Item::Impl(i) => {
                for m in &i.methods {
                    let mut f = m.clone();
                    f.name = qualify(
                        prefix,
                        &format!("{}::{}::{}", i.type_name, i.trait_name, m.name),
                    );
                    queue.push(StepKind::Func(f));
                }
            }
            Item::Flow(flow) => {
                for t in &flow.transitions {
                    if let Some(body) = &t.body {
                        // Synthesize a FuncDef for the transition body.
                        let f = FuncDef {
                            meta: AstNodeMeta::inherited(
                                t.meta.span,
                                AstOrigin::RuntimeSystem("verifier.transition_function"),
                            ),
                            name: qualify(prefix, &format!("{}::{}", flow.name, t.name)),
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
                        queue.push(StepKind::Func(f));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Verify FFI call sites using the Flow wrapper.
/// One-shot operation (no per-func stepping).
pub fn flow_verify_ffi_call_sites(file: &File) -> Result<Vec<VerificationResult>, String> {
    let mut session = SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS)?;
    let mut ctx = VerifierCtx::default();
    Ok(ctx.verify_ffi_call_sites(&mut session, file))
}

/// Verify FFI call sites, falling back to mock if Z3 is unavailable.
pub fn flow_verify_ffi_call_sites_or_mock(file: &File) -> Result<Vec<VerificationResult>, String> {
    match SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS) {
        Ok(mut session) => {
            let mut ctx = VerifierCtx::default();
            Ok(ctx.verify_ffi_call_sites(&mut session, file))
        }
        Err(_) => Ok(helpers::mock_verify_file(file)),
    }
}

pub(crate) fn flow_verify_ffi_call_sites_with_externs_or_mock(
    file: &File,
    externs: &std::collections::HashMap<String, crate::ast::ExternFunc>,
) -> Result<Vec<VerificationResult>, String> {
    match SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS) {
        Ok(mut session) => {
            let mut ctx = VerifierCtx::default();
            Ok(ctx.verify_ffi_call_sites_with_externs(&mut session, file, externs))
        }
        Err(_) => Ok(helpers::mock_verify_file(file)),
    }
}

/// Entry for external callers that already have a file (e.g. build pipeline).
/// Falls back to mock verification if Z3 is unavailable.
///
/// Note: Retained for the test helper `flow_verify_source_unchecked`
/// (which verifies AST directly without type-checking).
/// The primary entry point `verify_checked` now routes the Z3 path
/// through `flow_verify_file_with_hashes` and the mock path through
/// `mock_verify_checked` (CheckedProgram-based).
/// Not re-exported; dead_code suppressed — used only in #[cfg(test)].
#[allow(dead_code)]
pub(crate) fn flow_verify_file_or_mock(
    file: &File,
    source_hash: String,
    resolved_ir_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    if SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS).is_ok() {
        flow_verify_file_with_hashes(file, source_hash, resolved_ir_hash)
    } else {
        Ok(helpers::mock_verify_file(file))
    }
}

#[cfg(test)]
fn flow_verify_source_unchecked(source: &str) -> Result<Vec<VerificationResult>, String> {
    let file = super::parse_memory_source(source, "flow-unchecked-tests")?;
    flow_verify_file_or_mock(&file, String::new(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Helper: create a file AST from source (same as verify_source's parsing).
    fn parse_source(source: &str) -> Result<File, String> {
        crate::verifier::parse_memory_source(source, "flow-tests")
    }

    /// Assert the Flow state-machine driver and the two-wave legacy AST
    /// engine agree on (func_name, status) for every verified function.
    ///
    /// V-5 (audit 2026-08-05): the previous "equivalence test" called the
    /// SAME flow engine twice and compared the results with itself — a
    /// self-comparison that could never fail and caught nothing. This
    /// version compares two genuinely different schedulers:
    ///   1. `flow_verify_file_with_hashes` — Flow state-machine queue with
    ///      the V-4 fixpoint driver;
    ///   2. `Verifier::verify_file` — `VerifierCtx::verify_items`, the
    ///      two-wave AST engine with an independent call graph schedule.
    /// Any scheduler/status-propagation divergence (V-4-class source-order
    /// bugs included) fails the test.
    fn assert_verify_equivalent(source: &str) {
        let file = match parse_source(source) {
            Ok(f) => f,
            Err(_) => return,
        };
        if SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS).is_err() {
            return; // Z3 unavailable
        }
        // Engine 1: Flow state machine (fixpoint driver).
        let flow_results = match flow_verify_file_with_hashes(&file, String::new(), String::new()) {
            Ok(r) => r,
            Err(e) => panic!("flow verifier failed: {}", e),
        };
        // Engine 2: legacy two-wave AST engine.
        let mut verifier = match crate::verifier::ctx::Verifier::new() {
            Ok(v) => v,
            Err(_) => return,
        };
        let legacy_results = verifier.verify_file(&file);
        assert_eq!(
            flow_results.len(),
            legacy_results.len(),
            "result count mismatch for source:\n{}\nflow: {} results\nlegacy: {} results",
            source,
            flow_results.len(),
            legacy_results.len()
        );
        for (i, (flow, legacy)) in flow_results.iter().zip(legacy_results.iter()).enumerate() {
            assert_eq!(
                flow.func_name, legacy.func_name,
                "func_name mismatch at index {} for source:\n{}",
                i, source
            );
            assert_eq!(
                flow.status, legacy.status,
                "status mismatch for '{}' at index {} for source:\n{}\nflow: {}\nlegacy: {}",
                flow.func_name, i, source, flow.message, legacy.message
            );
        }
    }

    /// V-4 regression (audit 2026-08-05): verdicts must not depend on
    /// SOURCE ORDER. The chain C→B→A declared caller-first ([C,B,A]) used to
    /// lose C's callee axioms permanently — C stayed Disproven while the
    /// same program declared [A,B,C] verified everything. Both declarations
    /// must now yield identical verdicts, with C Proven.
    #[test]
    fn test_flow_source_order_independence() {
        if SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS).is_err() {
            return; // Z3 unavailable
        }
        let caller_first = "
            func c(x: int) -> int { requires: x >= 0; ensures: result == x; b(x) }
            func b(x: int) -> int { requires: x >= 0; ensures: result == x; a(x) }
            func a(x: int) -> int { requires: x >= 0; ensures: result == x; x }";
        let callee_first = "
            func a(x: int) -> int { requires: x >= 0; ensures: result == x; x }
            func b(x: int) -> int { requires: x >= 0; ensures: result == x; a(x) }
            func c(x: int) -> int { requires: x >= 0; ensures: result == x; b(x) }";

        let status_map = |source: &str| -> std::collections::BTreeMap<
            String,
            crate::verifier::ctx::VerifStatus,
        > {
            let results = flow_verify_source_unchecked(source)
                .unwrap_or_else(|e| panic!("verifier failed: {}", e));
            results
                .into_iter()
                .map(|r| (r.func_name, r.status))
                .collect()
        };

        let by_caller_first = status_map(caller_first);
        let by_callee_first = status_map(callee_first);
        assert_eq!(
            by_caller_first, by_callee_first,
            "verification verdicts differ between source orders (V-4):\n\
             caller-first: {:?}\ncallee-first: {:?}",
            by_caller_first, by_callee_first
        );
        assert_eq!(
            by_caller_first.get("c"),
            Some(&crate::verifier::ctx::VerifStatus::Proven),
            "chained caller c must be Proven regardless of declaration order"
        );
    }

    // ── Basic contract verification ──

    #[test]
    fn test_flow_simple_requires() {
        assert_verify_equivalent(
            "func add(x: int, y: int) -> int {
                requires: x + y < 1000
                x + y
            }",
        );
    }

    #[test]
    fn test_flow_simple_ensures() {
        assert_verify_equivalent(
            "func double(x: int) -> int {
                ensures: result == 2 * x
                x + x
            }",
        );
    }

    #[test]
    fn test_flow_requires_ensures() {
        assert_verify_equivalent(
            "func add_positive(x: int, y: int) -> int {
                requires: x > 0
                requires: y > 0
                ensures: result > 0
                x + y
            }",
        );
    }

    #[test]
    fn test_flow_no_contracts() {
        assert_verify_equivalent(
            "func add(x: int, y: int) -> int {
                x + y
            }",
        );
    }

    // ── Extern contracts ──

    #[test]
    fn test_flow_extern_contracts() {
        assert_verify_equivalent(
            "extern {
                func sqrt(x: f64) -> f64 {
                    requires: x >= 0.0
                    ensures: result >= 0.0
                }
            }",
        );
    }

    // ── Math constraints ──

    #[test]
    fn test_flow_math_constraints() {
        assert_verify_equivalent(
            "func identity(x: int) -> int {
                math: x == x
                ensures: result == x
                x
            }",
        );
    }

    // ── String contracts ──

    #[test]
    fn test_flow_string_contract() {
        assert_verify_equivalent(
            "func greet(name: string) -> string {
                requires: name != \"\"
                ensures: len(result) > 0
                \"Hello, \" + name
            }",
        );
    }

    // ── Call-site propagation ──

    #[test]
    fn test_flow_call_site_ensures() {
        assert_verify_equivalent(
            "func double(x: int) -> int {
                ensures: result == 2 * x
                x + x
            }
            func caller(y: int) -> int {
                ensures: result == 2 * y
                double(y)
            }",
        );
    }

    // ── Multiple functions ──

    #[test]
    fn test_flow_multiple_funcs() {
        assert_verify_equivalent(
            "func id(x: int) -> int {
                ensures: result == x
                x
            }
            func one(_: int) -> int {
                1
            }
            func neg(x: int) -> int {
                ensures: result == 0 - x
                0 - x
            }",
        );
    }

    // ── Unsat requires ──

    #[test]
    fn test_flow_unsat_requires() {
        assert_verify_equivalent(
            "func impossible(x: int) -> int {
                requires: x > 0
                requires: x < 0
                x
            }",
        );
    }

    // ── State machine API tests ──

    #[test]
    fn test_flow_state_stepping() {
        let source = "func a(x: int) -> int { requires: x > 0; ensures: result > 0; x }
                       func b(y: int) -> int { ensures: result == y; y }";
        let file = match parse_source(source) {
            Ok(f) => f,
            Err(_) => return,
        };
        if SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS).is_err() {
            return;
        }
        let state = VerifierState::new(&file).unwrap();
        assert!(!state.is_done());
        // 2 functions in queue → 2 Step transitions process them,
        // then a 3rd Step transitions to Done (empty queue → Done).
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(!state.is_done());
        let state = state.transition(FlowEvent::Step).unwrap();
        // After processing all functions, one more Step to reach Done.
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(state.is_done());
        let acc = state.into_output();
        assert_eq!(acc.results.len(), 2);
    }

    #[test]
    fn test_flow_state_step_after_done() {
        if SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS).is_err() {
            return;
        }
        let source = "func a(x: int) -> int { x }";
        let file = parse_source(source).unwrap();
        let state = VerifierState::new(&file).unwrap();
        let state = state.transition(FlowEvent::Step).unwrap();
        // Step after Done should stay Done
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(state.is_done());
    }
}
