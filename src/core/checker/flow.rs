use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashSet;

use super::Checker;

/// Flow event driving the checker state machine.
#[derive(Debug, Clone, Copy)]
pub enum FlowEvent {
    /// Advance to the next phase or next item.
    Step,
}

/// Accumulated output from the checker flow.
#[derive(Debug, Clone)]
pub struct FlowAcc {
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

/// Checker state machine — 宽松 Flow.
///
/// Two explicit phases: Collecting (collect_decls) → Checking (check_item per item).
/// Each `Step` advances one phase or processes one top-level item.
/// The checker struct is owned by the state and transferred by value through
/// transitions, eliminating `&mut self` at the state-machine level.
pub enum CheckerState<'a> {
    Collecting {
        checker: Checker<'a>,
    },
    Checking {
        checker: Checker<'a>,
        index: usize,
        total: usize,
    },
    Done(FlowAcc),
}

impl<'a> CheckerState<'a> {
    /// Create initial Collecting state from a parsed File AST.
    pub fn new(file: &'a File) -> Self {
        CheckerState::Collecting {
            checker: Checker::new(file),
        }
    }

    /// Transition: process one phase or one item per Step event.
    pub fn transition(self, _event: FlowEvent) -> Result<Self, String> {
        match self {
            CheckerState::Collecting { mut checker } => {
                checker.collect_decls();
                let total = checker.file.items.len();
                Ok(CheckerState::Checking {
                    checker,
                    index: 0,
                    total,
                })
            }

            CheckerState::Checking {
                mut checker,
                index,
                total,
            } => {
                if index < total {
                    let item = checker.file.items[index].clone();
                    checker.check_item(&item);
                    Ok(CheckerState::Checking {
                        checker,
                        index: index + 1,
                        total,
                    })
                } else {
                    let acc = extract_acc(&mut checker);
                    Ok(CheckerState::Done(acc))
                }
            }

            done @ CheckerState::Done(_) => Ok(done),
        }
    }

    /// True if the machine has reached terminal state.
    pub fn is_done(&self) -> bool {
        matches!(self, CheckerState::Done(_))
    }

    /// Consume and extract final accumulator.
    pub fn into_output(self) -> FlowAcc {
        match self {
            CheckerState::Done(acc) => acc,
            CheckerState::Collecting { mut checker }
            | CheckerState::Checking { mut checker, .. } => extract_acc(&mut checker),
        }
    }
}

/// Extract deduplicated errors and warnings from the checker.
fn extract_acc(checker: &mut Checker) -> FlowAcc {
    let mut seen: HashSet<(Option<String>, String)> = HashSet::new();
    let mut deduped: Vec<Diagnostic> = Vec::with_capacity(checker.errors.len());
    for e in std::mem::take(&mut checker.errors) {
        let key = (e.code.clone(), e.message.clone());
        if seen.insert(key) {
            deduped.push(e);
        }
    }
    FlowAcc {
        errors: deduped,
        warnings: std::mem::take(&mut checker.warnings),
    }
}

/// Driver: run the checker state machine to completion.
fn run_to_done<'a>(mut state: CheckerState<'a>) -> Result<CheckerState<'a>, String> {
    loop {
        state = state.transition(FlowEvent::Step)?;
        if state.is_done() {
            break;
        }
    }
    Ok(state)
}

/// Run the Flow checker on a file. Returns Ok(()) or Err(diagnostics) — same
/// interface as `core::check`.
pub fn flow_check(file: &File) -> Result<(), Vec<Diagnostic>> {
    let state = CheckerState::new(file);
    let state = match run_to_done(state) {
        Ok(s) => s,
        Err(e) => return Err(vec![Diagnostic::error(e, Span::single(0, 0))]),
    };
    let acc = state.into_output();
    if acc.errors.is_empty() {
        Ok(())
    } else {
        Err(acc.errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::check;

    /// Helper: count type errors in source (positive check).
    fn count_errors(source: &str) -> usize {
        parse_and_count(source, false)
    }

    /// Compile source and count errors from flow checker.
    fn parse_and_count(source: &str, expected_ok: bool) -> usize {
        let tokens = match crate::lexer::Lexer::new(source).tokenize() {
            Ok(t) => t,
            Err(_) => return if expected_ok { 1 } else { 0 },
        };
        let file = match crate::parser::Parser::new(tokens).parse_file() {
            Ok(f) => f,
            Err(_) => return if expected_ok { 1 } else { 0 },
        };
        match flow_check(&file) {
            Ok(()) => 0,
            Err(errors) => errors.len(),
        }
    }

    /// Assert that Flow checker produces equivalent results to legacy.
    fn assert_check_equivalent(source: &str) {
        let tokens = match crate::lexer::Lexer::new(source).tokenize() {
            Ok(t) => t,
            Err(_) => return, // skip parse errors
        };
        let file = match crate::parser::Parser::new(tokens).parse_file() {
            Ok(f) => f,
            Err(_) => return,
        };

        let legacy_ok = check(&file).is_ok();
        let legacy_err_count = match check(&file) {
            Ok(()) => 0,
            Err(e) => e.len(),
        };
        let flow_ok = flow_check(&file).is_ok();
        let flow_err_count = match flow_check(&file) {
            Ok(()) => 0,
            Err(e) => e.len(),
        };

        assert_eq!(
            legacy_ok, flow_ok,
            "check pass/fail mismatch\nsource: {}",
            source
        );
        assert_eq!(
            legacy_err_count, flow_err_count,
            "error count mismatch\nsource: {}",
            source
        );
    }

    // ── Basic type checking ──

    #[test]
    fn test_flow_valid_func() {
        assert_check_equivalent("func add(x: int, y: int) -> int { x + y }");
    }

    #[test]
    fn test_flow_type_mismatch() {
        assert_check_equivalent(
            "func add(x: int, y: int) -> int { x + y }
             func wrong() -> int { add(true, 1) }",
        );
    }

    #[test]
    fn test_flow_missing_return() {
        assert_check_equivalent("func missing() -> int { }");
    }

    #[test]
    fn test_flow_return_type_mismatch() {
        assert_check_equivalent("func bad() -> int { true }");
    }

    #[test]
    fn test_flow_unknown_var() {
        assert_check_equivalent("func foo() -> int { undefined_var }");
    }

    // ── Let bindings and locals ──

    #[test]
    fn test_flow_let_inference() {
        assert_check_equivalent(
            "func id(x: int) -> int {
                let y = x + 1;
                y
            }",
        );
    }

    // ── If expressions ──

    #[test]
    fn test_flow_if_types() {
        assert_check_equivalent(
            "func max(a: int, b: int) -> int {
                if a > b { a } else { b }
            }",
        );
    }

    // ── While loops ──

    #[test]
    fn test_flow_while() {
        assert_check_equivalent(
            "func countdown(n: int) -> int {
                let mut x = n;
                while x > 0 {
                    x = x - 1;
                };
                x
            }",
        );
    }

    // ── Nested functions ──

    #[test]
    fn test_flow_nested_func() {
        assert_check_equivalent(
            "func outer(x: int) -> int {
                func inner(y: int) -> int { y + 1 };
                inner(x)
            }",
        );
    }

    // ── State machine API tests ──

    #[test]
    fn test_flow_state_phases() {
        let source = "func add(x: int, y: int) -> int { x + y }
                       func sub(x: int, y: int) -> int { x - y }";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let file = crate::parser::Parser::new(tokens).parse_file().unwrap();

        let state = CheckerState::new(&file);
        assert!(matches!(state, CheckerState::Collecting { .. }));

        // Phase 1: collect
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(matches!(state, CheckerState::Checking { .. }));

        // Phase 2: item 0 (consumes 1 step)
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(matches!(state, CheckerState::Checking { index: 1, total: 2, .. }));

        // Phase 2: item 1 (consumes 1 step)
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(matches!(state, CheckerState::Checking { index: 2, total: 2, .. }));

        // One more step to transition to Done
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(state.is_done());
    }

    #[test]
    fn test_flow_empty_file() {
        use crate::ast::Import;
        let file = File {
            imports: Vec::new(),
            items: Vec::new(),
        };
        let state = CheckerState::new(&file);
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(matches!(state, CheckerState::Checking { total: 0, .. }));
        let state = state.transition(FlowEvent::Step).unwrap();
        assert!(state.is_done());
    }
}
