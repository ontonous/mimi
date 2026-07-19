//! Flow-based parser state machine — v0.29.0 prototype.
//!
//! Replaces the top-level parse loop with a pure state machine.
//! Each `transition(self, event) -> Result<(Self, Option<...>), Error>` call
//! consumes the old state and produces a new one. No `&mut self` at the top level.
//!
//! Internally creates temporary `Parser` instances (via `Parser::splice`) for
//! recursive descent sub-parsing. This is the "scoped &mut self" pattern.

#![allow(dead_code)]

use super::*;
use crate::span::SourceId;

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// Create an empty flow accumulator.
macro_rules! flow_acc {
    () => {
        FlowAcc {
            imports: Vec::new(),
            items: Vec::new(),
            errors: Vec::new(),
        }
    };
}

/// Create the initial `FlowState::Init`.
macro_rules! flow_init {
    ($recovery:expr) => {
        FlowState::Init {
            pos: 0,
            recovery: $recovery,
            acc: flow_acc!(),
        }
    };
}

/// Return `Ok((state, None))` — continue without producing output.
/// `$acc` and `$recovery` are explicit parameters due to macro hygiene.
macro_rules! state_continue {
    ($variant:ident { $($field:ident $(: $val:expr)?),+ $(,)? }, $acc:expr, $recovery:expr) => {
        Ok((FlowState::$variant {
            $($field $(: $val)?),+,
            acc: $acc,
            recovery: $recovery,
        }, None))
    };
}

/// Return `Ok((state, Some(output)))` — continue with a parsed unit.
macro_rules! state_yield {
    ($variant:ident { $($field:ident $(: $val:expr)?),+ $(,)? }, $acc:expr, $recovery:expr, $output:expr) => {
        Ok((FlowState::$variant {
            $($field $(: $val)?),+,
            acc: $acc,
            recovery: $recovery,
        }, Some($output)))
    };
}

/// Return `Ok((Done(file, errors), None))` — terminate successfully.
macro_rules! state_done {
    ($acc:expr) => {
        Ok((
            FlowState::Done(
                File {
                    sources: crate::span::SourceRegistry::default(),
                    imports: std::mem::take(&mut $acc.imports),
                    items: std::mem::take(&mut $acc.items),
                    implicit_single: false,
                },
                $acc.errors,
            ),
            None,
        ))
    };
}

/// Drive the flow state machine to completion.
///
/// Usage:
/// ```ignore
/// let result: Result<File, ParseError> = run_flow!(state, mode, tokens);
/// let (file, errors): (File, Vec<ParseError>) = run_flow!(recovery state, mode, tokens);
/// ```
macro_rules! run_flow {
    // Strict mode: first error fails
    ($state:expr, $mode:expr, $tokens:expr, $source_id:expr) => {{
        let mut __state = $state;
        loop {
            match __state {
                FlowState::Done(file, _errors) => break Ok(file),
                __s => {
                    let (new_state, _) =
                        __s.transition(&FlowEvent::Step, $mode, $tokens, $source_id)?;
                    __state = new_state;
                }
            }
        }
    }};
    // Recovery mode: collect errors and preserve partial AST.
    // PR-C2: on a hard transition error, keep already-parsed imports/items
    // instead of returning an empty File + single error.
    (recovery $state:expr, $mode:expr, $tokens:expr, $source_id:expr) => {{
        let mut __state = $state;
        loop {
            match __state {
                FlowState::Done(file, errors) => break (file, errors),
                __s => {
                    // Snapshot partial results before moving state into transition.
                    let (imports, items, mut errors) = match &__s {
                        FlowState::Init { acc, .. }
                        | FlowState::Imports { acc, .. }
                        | FlowState::Items { acc, .. } => {
                            (acc.imports.clone(), acc.items.clone(), acc.errors.clone())
                        }
                        FlowState::Done(file, errors) => {
                            (file.imports.clone(), file.items.clone(), errors.clone())
                        }
                    };
                    match __s.transition(&FlowEvent::Step, $mode, $tokens, $source_id) {
                        Ok((new_state, _)) => __state = new_state,
                        Err(e) => {
                            errors.push(e);
                            break (
                                File {
                                    sources: crate::span::SourceRegistry::default(),
                                    imports,
                                    items,
                                    implicit_single: false,
                                },
                                errors,
                            );
                        }
                    }
                }
            }
        }
    }};
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Events that drive the parser state machine.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FlowEvent {
    /// Advance to the next parsing unit (import, item, or EOF).
    Step,
    /// Finalize and produce the output.
    Complete,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Accumulated parse results shared across states.
#[derive(Debug, Clone)]
pub struct FlowAcc {
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
    pub errors: Vec<ParseError>,
}

/// Pure parse state — each variant represents a distinct parser phase.
/// No `&mut self` methods — only `self -> Result<Self, _>` transitions.
///
/// The `pos`, `recovery`, `acc` fields are present in every active variant
/// and are propagated automatically by the `state_continue!` / `state_yield!` macros.
#[derive(Debug, Clone)]
pub enum FlowState {
    Init {
        pos: usize,
        recovery: bool,
        acc: FlowAcc,
    },
    Imports {
        pos: usize,
        recovery: bool,
        acc: FlowAcc,
    },
    Items {
        pos: usize,
        recovery: bool,
        acc: FlowAcc,
    },
    Done(File, Vec<ParseError>),
}

// ---------------------------------------------------------------------------
// Helpers (pure functions on slices)
// ---------------------------------------------------------------------------

/// Skip newlines in a token slice starting from `pos`, return new position.
fn skip_newlines_slice(tokens: &[Token], mut pos: usize) -> usize {
    while pos < tokens.len() && matches!(tokens[pos].kind, TokenKind::Newline) {
        pos += 1;
    }
    pos
}

/// Peek at the token at `pos`, returning EOF if out of bounds.
fn peek_slice(tokens: &[Token], pos: usize) -> &Token {
    if pos >= tokens.len() {
        static EOF: Token = Token {
            kind: TokenKind::Eof,
            line: 0,
            col: 0,
            end_line: 0,
            end_col: 0,
        };
        &EOF
    } else {
        &tokens[pos]
    }
}

/// Check if the token at `pos` matches a kind.
fn at_slice(tokens: &[Token], pos: usize, kind: &TokenKind) -> bool {
    peek_slice(tokens, pos).kind == *kind
}

// ---------------------------------------------------------------------------
// Sub-parser creation
// ---------------------------------------------------------------------------

/// Create a temporary Parser at the given position within a token slice.
/// This is the bridge between Flow state and the existing recursive descent
/// parser. The Vec<Token> is cloned (cheap: ~24 bytes per token).
fn sub_parser(
    tokens: &[Token],
    pos: usize,
    mode: ParseMode,
    recovery: bool,
    source_id: SourceId,
) -> Parser {
    Parser::splice(tokens, pos, mode, recovery, source_id)
}

// ---------------------------------------------------------------------------
// Parse one item or import (stateless helpers that consume a Parser)
// ---------------------------------------------------------------------------
// Returns (result, statement-level errors collected during recovery, final position).

fn parse_one_import(
    tokens: &[Token],
    pos: usize,
    mode: ParseMode,
    recovery: bool,
    source_id: SourceId,
) -> (Result<Import, ParseError>, Vec<ParseError>, usize) {
    let mut p = sub_parser(tokens, pos, mode, recovery, source_id);
    let result = p
        .parse_import()
        .map_err(|error| error.with_source(source_id));
    let stmt_errors = if recovery {
        std::mem::take(&mut p.errors)
            .into_iter()
            .map(|error| error.with_source(source_id))
            .collect()
    } else {
        Vec::new()
    };
    (result, stmt_errors, p.pos)
}

fn parse_one_item(
    tokens: &[Token],
    pos: usize,
    mode: ParseMode,
    recovery: bool,
    source_id: SourceId,
) -> (Result<Item, ParseError>, Vec<ParseError>, usize) {
    let mut p = sub_parser(tokens, pos, mode, recovery, source_id);
    let result = p.parse_item().map_err(|error| error.with_source(source_id));
    let stmt_errors = if recovery {
        std::mem::take(&mut p.errors)
            .into_iter()
            .map(|error| error.with_source(source_id))
            .collect()
    } else {
        Vec::new()
    };
    (result, stmt_errors, p.pos)
}

// ---------------------------------------------------------------------------
// Recovery: skip to sync token
// ---------------------------------------------------------------------------

fn recover_to_sync_slice(tokens: &[Token], mut pos: usize) -> usize {
    const SYNC: &[TokenKind] = &[
        TokenKind::Func,
        TokenKind::Type,
        TokenKind::Module,
        TokenKind::Actor,
        TokenKind::Cap,
        TokenKind::Trait,
        TokenKind::Impl,
        TokenKind::Extern,
        TokenKind::Use,
        // HIGH fix: add Flow/Protocol/Session to sync tokens so recovery
        // correctly resumes at these top-level declarations.
        TokenKind::Flow,
        TokenKind::Protocol,
        TokenKind::Session,
        TokenKind::RBrace,
        TokenKind::Eof,
    ];
    // F-H4: if we failed *on* a declaration starter, skip it so recovery
    // does not re-enter the same broken item and cascade.
    if pos < tokens.len() && SYNC.iter().any(|k| tokens[pos].kind == *k) {
        pos += 1;
    }
    let max_skip = 100;
    let mut skipped = 0;
    while pos < tokens.len() && skipped < max_skip {
        if SYNC.iter().any(|k| tokens[pos].kind == *k) {
            return pos;
        }
        pos += 1;
        skipped += 1;
    }
    pos
}

// ---------------------------------------------------------------------------
// Transition function — the heart of the Flow state machine
// ---------------------------------------------------------------------------

impl FlowState {
    /// Pure state transition: consumes `self`, returns new state + optional output.
    /// - No `&mut self` — state is passed by value (ownership transfer).
    /// - Errors are either fatal (return Err) or recovery events.
    /// - Internal sub-parsing uses scoped `&mut` via temporary `Parser::splice`.
    pub fn transition(
        self,
        event: &FlowEvent,
        mode: ParseMode,
        tokens: &[Token],
        source_id: SourceId,
    ) -> Result<(Self, Option<FlowOutput>), ParseError> {
        match (self, event) {
            // ── Init → Imports or Items (or Done if empty) ─────────
            (
                FlowState::Init {
                    pos,
                    recovery,
                    mut acc,
                },
                FlowEvent::Step,
            ) => {
                let pos = skip_newlines_slice(tokens, pos);
                if pos >= tokens.len() || at_slice(tokens, pos, &TokenKind::Eof) {
                    state_done!(acc)
                } else if at_slice(tokens, pos, &TokenKind::Use) {
                    state_continue!(Imports { pos }, acc, recovery)
                } else {
                    state_continue!(Items { pos }, acc, recovery)
                }
            }

            // ── Imports → Imports (more) or Items (done) ──────────
            (FlowState::Imports { pos, recovery, acc }, FlowEvent::Step) => {
                let pos = skip_newlines_slice(tokens, pos);
                if pos >= tokens.len() || !at_slice(tokens, pos, &TokenKind::Use) {
                    state_continue!(Items { pos }, acc, recovery)
                } else {
                    let (result, stmt_errors, new_pos) =
                        parse_one_import(tokens, pos, mode, recovery, source_id);
                    let mut acc = acc;
                    if recovery {
                        acc.errors.extend(stmt_errors);
                    }
                    match result {
                        Ok(import) => {
                            acc.imports.push(import);
                            let new_pos = skip_newlines_slice(tokens, new_pos);
                            state_yield!(
                                Imports { pos: new_pos },
                                acc,
                                recovery,
                                FlowOutput::Import
                            )
                        }
                        Err(e) => {
                            if recovery {
                                acc.errors.push(e);
                                let new_pos = recover_to_sync_slice(tokens, pos);
                                let new_pos = if new_pos == pos { pos + 1 } else { new_pos };
                                state_yield!(
                                    Imports { pos: new_pos },
                                    acc,
                                    recovery,
                                    FlowOutput::Error
                                )
                            } else {
                                Err(e)
                            }
                        }
                    }
                }
            }

            // ── Items → Items (more) or Done (EOF) ────────────────
            (
                FlowState::Items {
                    pos,
                    recovery,
                    mut acc,
                },
                FlowEvent::Step,
            ) => {
                let pos = skip_newlines_slice(tokens, pos);
                if pos >= tokens.len() || at_slice(tokens, pos, &TokenKind::Eof) {
                    state_done!(acc)
                } else {
                    let (result, stmt_errors, new_pos) =
                        parse_one_item(tokens, pos, mode, recovery, source_id);
                    if recovery {
                        acc.errors.extend(stmt_errors);
                    }
                    match result {
                        Ok(item) => {
                            acc.items.push(item);
                            state_yield!(Items { pos: new_pos }, acc, recovery, FlowOutput::Item)
                        }
                        Err(e) => {
                            if recovery {
                                acc.errors.push(e);
                                let new_pos = recover_to_sync_slice(tokens, pos);
                                let new_pos = if new_pos == pos { pos + 1 } else { new_pos };
                                state_yield!(
                                    Items { pos: new_pos },
                                    acc,
                                    recovery,
                                    FlowOutput::Error
                                )
                            } else {
                                Err(e)
                            }
                        }
                    }
                }
            }

            // ── Complete (force finalize) ─────────────────────────
            (state, FlowEvent::Complete) => match state {
                FlowState::Init { mut acc, .. }
                | FlowState::Imports { mut acc, .. }
                | FlowState::Items { mut acc, .. } => state_done!(acc),
                done @ FlowState::Done(..) => Ok((done, None)),
            },

            // ── Terminal state ────────────────────────────────────
            (done @ FlowState::Done(..), _) => Ok((done, None)),
        }
    }
}

// ---------------------------------------------------------------------------
// Events passed out of transitions
// ---------------------------------------------------------------------------

/// Lightweight output type — signals what was produced without carrying data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowOutput {
    Import,
    Item,
    Error,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Flow-based parser: strict mode (first error fails).
/// Semantically equivalent to `Parser::parse_file()`.
/// After parsing, expands the flow transfer matrix (+1 Fault fallback).
pub fn flow_parse(
    tokens: Vec<Token>,
    mode: ParseMode,
    source_id: SourceId,
    source_registry: crate::span::SourceRegistry,
) -> Result<File, ParseError> {
    let mut file = run_flow!(flow_init!(false), mode, &tokens, source_id)?;
    file.sources = source_registry;
    // v0.29.22: progressive Typestate — inject implicit Main/Single for scripts.
    crate::progressive::apply_progressive_typestate(&mut file);
    crate::flow_matrix::expand_file(&mut file);
    Ok(file)
}

/// Flow-based parser: recovery mode (collects all errors).
/// Semantically equivalent to `Parser::parse_file_with_recovery()`.
/// After parsing, expands the flow transfer matrix (+1 Fault fallback).
pub fn flow_parse_with_recovery(
    tokens: Vec<Token>,
    mode: ParseMode,
    source_id: SourceId,
    source_registry: crate::span::SourceRegistry,
) -> (File, Vec<ParseError>) {
    let (mut file, errors) = run_flow!(recovery flow_init!(true), mode, &tokens, source_id);
    file.sources = source_registry;
    crate::progressive::apply_progressive_typestate(&mut file);
    crate::flow_matrix::expand_file(&mut file);
    (file, errors)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn tokenize(src: &str) -> Vec<Token> {
        Lexer::new(src).tokenize().expect("lex failed")
    }

    #[test]
    fn parser_attaches_source_id_to_statement_spans() {
        let source_id = SourceId::new(9);
        let file = Parser::new_with_source(
            tokenize("func main() -> i32 { requires: true\n return 0 }"),
            source_id,
        )
        .parse_file()
        .expect("parse");
        let main = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main function");
        let Stmt::Requires(_, span) = main.body[0].unlocated() else {
            panic!("expected requires statement")
        };
        assert_eq!(span.source_id, source_id);
    }

    fn assert_parse_equivalent(src: &str) {
        let tokens = tokenize(src);
        let old_result = Parser::new(tokens.clone()).legacy_parse_file();
        let flow_result = flow_parse(
            tokens,
            ParseMode::Production,
            SourceId::UNKNOWN,
            SourceRegistry::default(),
        );
        match (&old_result, &flow_result) {
            (Ok(old_file), Ok(flow_file)) => assert_eq!(
                format!("{old_file:?}"), format!("{flow_file:?}"),
                "AST mismatch for source: {src}"
            ),
            (Err(old_err), Err(flow_err)) => assert_eq!(
                old_err.to_string(), flow_err.to_string(),
                "Error mismatch for source: {src}"
            ),
            _ => panic!("Parser result mismatch for source: {src}\n  old: {old_result:?}\n  flow: {flow_result:?}"),
        }
    }

    fn assert_recovery_equivalent(src: &str) {
        let tokens = tokenize(src);
        let (old_file, old_errors) =
            Parser::new_with_recovery(tokens.clone()).legacy_parse_file_with_recovery();
        let (flow_file, flow_errors) = flow_parse_with_recovery(
            tokens,
            ParseMode::Production,
            SourceId::UNKNOWN,
            SourceRegistry::default(),
        );
        assert_eq!(
            format!("{old_file:?}"),
            format!("{flow_file:?}"),
            "Recovery AST mismatch"
        );
        assert_eq!(
            old_errors.len(),
            flow_errors.len(),
            "Error count mismatch for: {src}"
        );
    }

    fn real_world_files() -> Vec<std::path::PathBuf> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("real_world");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "mimi"))
            .map(|e| e.path())
            .collect();
        files.sort();
        files
    }

    // ── Basic ──────────────────────────────────────────────────

    #[test]
    fn test_empty_file() {
        assert_parse_equivalent("");
    }
    #[test]
    fn test_only_newlines() {
        assert_parse_equivalent("\n\n\n");
    }
    #[test]
    fn test_single_import() {
        assert_parse_equivalent("use std::io;");
    }
    #[test]
    fn test_multiple_imports() {
        assert_parse_equivalent("use std::io;\nuse std::fs;\nuse std::collections;");
    }
    #[test]
    fn test_simple_func() {
        assert_parse_equivalent("func main() -> i32 { 42 }");
    }
    #[test]
    fn test_func_with_import() {
        assert_parse_equivalent("use std::io;\n\nfunc main() -> i32 { 42 }");
    }
    #[test]
    fn test_multiple_items() {
        assert_parse_equivalent(
            "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1, 2) }\n",
        );
    }
    #[test]
    fn test_with_comments_and_newlines() {
        assert_parse_equivalent("// A comment\nuse std::io\n\nfunc greet(name: string) { print_line(\"Hello \" + name) }\n");
    }
    #[test]
    fn test_only_imports() {
        assert_parse_equivalent("use a;\nuse b::c;\nuse d::e::f;");
    }
    #[test]
    fn test_only_item() {
        assert_parse_equivalent("type Foo = i32;");
    }
    #[test]
    fn test_multiline_params() {
        assert_parse_equivalent("func foo(\n    a: i32,\n    b: string,\n) -> i32 { a }\n");
    }

    // ── Recovery ───────────────────────────────────────────────

    #[test]
    fn test_recovery_empty() {
        assert_recovery_equivalent("");
    }
    #[test]
    fn test_recovery_simple() {
        assert_recovery_equivalent("func main() -> i32 { 42 }");
    }

    // ── Real-world equivalence ─────────────────────────────────

    #[test]
    fn test_flow_parser_matches_legacy_all_real_world() {
        let files = real_world_files();
        assert!(!files.is_empty(), "No real_world .mimi files found");
        for path in &files {
            let src = std::fs::read_to_string(path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let tokens = tokenize(&src);
            let old = Parser::new(tokens.clone()).legacy_parse_file();
            let flow = flow_parse(
                tokens,
                ParseMode::Production,
                SourceId::UNKNOWN,
                SourceRegistry::default(),
            );
            match (&old, &flow) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a.imports.len(), b.imports.len(), "import count: {name}");
                    assert_eq!(a.items.len(), b.items.len(), "item count: {name}");
                }
                (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "error: {name}"),
                _ => panic!("mismatch: {name}"),
            }
        }
    }

    #[test]
    fn test_flow_recovery_matches_legacy_all_real_world() {
        for path in &real_world_files() {
            let src = std::fs::read_to_string(path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let tokens = tokenize(&src);
            let (old_f, old_e) =
                Parser::new_with_recovery(tokens.clone()).legacy_parse_file_with_recovery();
            let (flow_f, flow_e) = flow_parse_with_recovery(
                tokens,
                ParseMode::Production,
                SourceId::UNKNOWN,
                SourceRegistry::default(),
            );
            assert_eq!(old_f.imports.len(), flow_f.imports.len(), "import: {name}");
            assert_eq!(old_f.items.len(), flow_f.items.len(), "item: {name}");
            assert_eq!(old_e.len(), flow_e.len(), "errors: {name}");
        }
    }

    /// Regression test for the mms{} block parser.
    /// Previously `first_col.unwrap_or(0)` masked an invariant — we want
    /// the mms body to be preserved verbatim through the parser.
    #[test]
    fn mms_block_nested_braces_preserved() {
        let src = r#"func test() -> i32 {
            mms{
                desc {
                    page 10
                }
            }
            return 0
        }"#;
        let tokens = Lexer::new(src).tokenize().expect("lex");
        let file = flow_parse(
            tokens,
            ParseMode::Production,
            SourceId::UNKNOWN,
            SourceRegistry::default(),
        )
        .expect("parse");
        let func_body = file
            .items
            .first()
            .and_then(|i| match i {
                crate::ast::Item::Func(f) => Some(&f.body),
                _ => None,
            })
            .expect("expected first item to be a function");
        let mms = func_body
            .iter()
            .find_map(|s| match s.unlocated() {
                crate::ast::Stmt::MmsBlock { content, .. } => Some(content),
                _ => None,
            })
            .expect("expected MmsBlock statement in function body");
        assert!(
            mms.contains("desc"),
            "outer content should keep 'desc' marker: {mms:?}"
        );
        assert!(
            mms.contains("page 10"),
            "outer content should keep nested page: {mms:?}"
        );
    }
}
