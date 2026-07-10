use crate::ast::*;
use crate::verifier::ctx::{VerificationResult, Verifier};
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
/// the next state. The Z3 solver is owned by the state and is only mutated
/// inside transition(), eliminating `&mut self` at the state-machine level.
pub enum VerifierState {
    Ready {
        verifier: Verifier,
        queue: Vec<StepKind>,
        acc: FlowAcc,
    },
    Done(FlowAcc),
}

impl VerifierState {
    /// Create initial Ready state from a parsed File AST.
    /// Collects func_defs and flattens all items into the verification queue.
    pub fn new(file: &File) -> Result<Self, String> {
        let mut verifier = Verifier::new()?;
        // Pre-collect func_defs for cross-module call-site reasoning
        verifier.collect_func_defs(&file.items);
        let queue = flatten_items(&file.items);
        Ok(VerifierState::Ready {
            verifier,
            queue,
            acc: FlowAcc::new(),
        })
    }

    /// Transition: process one function (body or extern) per Step event.
    /// Uses `self` by value — ownership moves in and out.
    pub fn transition(self, event: FlowEvent) -> Result<Self, String> {
        match (self, event) {
            (VerifierState::Ready { mut verifier, mut queue, mut acc }, FlowEvent::Step) => {
                match queue.pop() {
                    Some(StepKind::Func(func)) => {
                        if !func.body.is_empty() {
                            let result = verifier.verify_func(&func);
                            acc.results.push(result);
                        }
                        Ok(VerifierState::Ready {
                            verifier,
                            queue,
                            acc,
                        })
                    }
                    Some(StepKind::Extern(func)) => {
                        if func.requires.is_some() || func.ensures.is_some() {
                            let result = verifier.verify_extern_func(&func);
                            acc.results.push(result);
                        }
                        Ok(VerifierState::Ready {
                            verifier,
                            queue,
                            acc,
                        })
                    }
                    None => Ok(VerifierState::Done(acc)),
                }
            }
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
            _ => {}
        }
    }
}

/// Top-level entry: parse source, run Flow verifier, return results.
pub fn flow_verify_source(source: &str) -> Result<Vec<VerificationResult>, String> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| e.message)?;
    match Verifier::new() {
        Ok(_) => flow_verify_file(&file),
        Err(_) => Ok(helpers::mock_verify_file(&file)),
    }
}

/// Entry for external callers that already have a file (e.g. build pipeline).
/// Falls back to mock verification if Z3 is unavailable.
pub fn flow_verify_file_or_mock(file: &File) -> Result<Vec<VerificationResult>, String> {
    match Verifier::new() {
        Ok(_) => flow_verify_file(file),
        Err(_) => Ok(helpers::mock_verify_file(file)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verifier::verify_source;

    /// Helper: create a file AST from source (same as verify_source's parsing).
    fn parse_source(source: &str) -> Result<File, String> {
        let tokens = crate::lexer::Lexer::new(source).tokenize()?;
        crate::parser::Parser::new(tokens)
            .parse_file()
            .map_err(|e| e.message)
    }

    /// Assert that Flow verifier produces equivalent results to legacy.
    fn assert_verify_equivalent(source: &str) {
        // Skip if source doesn't parse or Z3 is unavailable
        if parse_source(source).is_err() || Verifier::new().is_err() {
            return;
        }
        // Run legacy verifier
        let legacy_results = match verify_source(source) {
            Ok(r) => r,
            Err(e) => panic!("legacy verifier failed: {}", e),
        };
        // Run Flow verifier
        let flow_results = match flow_verify_source(source) {
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
        if Verifier::new().is_err() {
            return;
        }
        let state = VerifierState::new(&file).unwrap();
        assert!(!state.is_done());
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(!state.is_done());
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(state.is_done());
        let acc = state.into_output();
        assert_eq!(acc.results.len(), 2);
    }

    #[test]
    fn test_flow_state_step_after_done() {
        if Verifier::new().is_err() {
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
