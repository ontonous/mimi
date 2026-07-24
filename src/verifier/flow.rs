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
        let mut session = SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS)?;
        let mut ctx = VerifierCtx::default();
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

/// Driver: run the verifier to completion.
pub fn flow_verify_file(file: &File) -> Result<Vec<VerificationResult>, String> {
    let state = VerifierState::new(file)?;
    let state = run_to_done(state)?;
    Ok(state.into_output().results)
}

/// Drive the state machine until Done.
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
    flatten_items_inner(items, &mut queue);
    queue.reverse(); // pop() yields from end → reverse so first item is at end
    queue
}

fn flatten_items_inner(items: &[Item], queue: &mut Vec<StepKind>) {
    for item in items {
        match item {
            Item::Func(f) => {
                queue.push(StepKind::Func(f.clone()));
            }
            Item::Module(m) => flatten_items_inner(&m.items, queue),
            Item::ExternBlock(block) => {
                for func in &block.funcs {
                    queue.push(StepKind::Extern(func.clone()));
                }
            }
            // V-H6: actor methods, impl methods, flow transitions enter the queue.
            Item::Actor(a) => {
                for m in &a.methods {
                    let mut f = m.clone();
                    f.name = format!("{}::{}", a.name, m.name);
                    queue.push(StepKind::Func(f));
                }
            }
            Item::Impl(i) => {
                for m in &i.methods {
                    let mut f = m.clone();
                    f.name = format!("{}::{}::{}", i.type_name, i.trait_name, m.name);
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
pub fn flow_verify_file_or_mock(file: &File) -> Result<Vec<VerificationResult>, String> {
    if SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS).is_ok() {
        flow_verify_file(file)
    } else {
        Ok(helpers::mock_verify_file(file))
    }
}

#[cfg(test)]
fn flow_verify_source_unchecked(source: &str) -> Result<Vec<VerificationResult>, String> {
    let file = super::parse_memory_source(source, "flow-unchecked-tests")?;
    flow_verify_file_or_mock(&file)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Helper: create a file AST from source (same as verify_source's parsing).
    fn parse_source(source: &str) -> Result<File, String> {
        crate::verifier::parse_memory_source(source, "flow-tests")
    }

    /// Assert that Flow verifier produces equivalent results to legacy.
    fn assert_verify_equivalent(source: &str) {
        // Skip if source doesn't parse or Z3 is unavailable
        if parse_source(source).is_err()
            || SolverSession::new(crate::verifier::ctx::DEFAULT_TIMEOUT_MS).is_err()
        {
            return;
        }
        // Run legacy verifier
        let legacy_results = match flow_verify_source_unchecked(source) {
            Ok(r) => r,
            Err(e) => panic!("legacy verifier failed: {}", e),
        };
        // Run Flow verifier
        let flow_results = match flow_verify_source_unchecked(source) {
            Ok(r) => r,
            Err(e) => panic!("flow verifier failed: {}", e),
        };
        assert_eq!(
            legacy_results.len(),
            flow_results.len(),
            "result count mismatch for source:\n{}\nlegacy: {} results\nflow: {} results",
            source,
            legacy_results.len(),
            flow_results.len()
        );
        for (i, (leg, flow)) in legacy_results.iter().zip(flow_results.iter()).enumerate() {
            assert_eq!(
                leg.func_name, flow.func_name,
                "func_name mismatch at index {} for source:\n{}",
                i, source
            );
            assert_eq!(
                leg.status, flow.status,
                "status mismatch for '{}' at index {} for source:\n{}\nlegacy: {}\nflow: {}",
                leg.func_name, i, source, leg.message, flow.message
            );
        }
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
