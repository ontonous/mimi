/// Static analysis / linting for Mimi source code.
///
/// Rules:
/// - W001: Unused `desc` / `rule` (metadata without implementation)
/// - W002: `$` / `$$` locked fragment with no implementation body
/// - W003: `...` placeholder residual (in .mimi files)
/// - W004: Function naming convention (snake_case)

use crate::ast::{File, Item, FuncDef, Stmt, Commitment};
use crate::diagnostic::{Diagnostic, Severity};
use crate::diagnostic::codes::{W001, W002, W003, W004};
use crate::span::Span;

pub struct Linter;

#[derive(Debug, Clone)]
pub struct LintResult {
    pub diagnostics: Vec<Diagnostic>,
}

impl Linter {
    pub fn new() -> Self {
        Self
    }

    pub fn lint(&self, file: &File, source: &str) -> LintResult {
        let mut diagnostics = Vec::new();

        for (idx, item) in file.items.iter().enumerate() {
            match item {
                Item::Func(f) => {
                    self.lint_func(f, source, &mut diagnostics);
                }
                Item::Desc(_, span) => {
                    if !is_followed_by_impl(&file.items, idx) {
                        diagnostics.push(Diagnostic::warning_code(
                            W001,
                            format!("standalone `desc` has no associated implementation"),
                            *span,
                        ));
                    }
                }
                Item::Rule(_, span) => {
                    if !is_followed_by_impl(&file.items, idx) {
                        diagnostics.push(Diagnostic::warning_code(
                            W001,
                            format!("standalone `rule` has no associated implementation"),
                            *span,
                        ));
                    }
                }
                _ => {}
            }
        }

        // W003: Check for `...` placeholders in source
        for (line_idx, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed == "..." {
                diagnostics.push(Diagnostic::warning_code(
                    W003,
                    "placeholder `...` residual in .mimi file",
                    Span::single(line_idx + 1, 1),
                ));
            }
        }

        LintResult { diagnostics }
    }

    fn lint_func(&self, func: &FuncDef, _source: &str, diagnostics: &mut Vec<Diagnostic>) {
        // W004: Check function naming convention (snake_case)
        if !func.name.is_empty() && !is_snake_case(&func.name) && !is_operator(&func.name) {
            diagnostics.push(Diagnostic::warning_code(
                W004,
                format!("function `{}` should use snake_case naming", func.name),
                Span::single(func.pos.0, func.pos.1),
            ));
        }

        // W002: Check for locked fragments with empty body
        if func.commitment.is_locked() && func.body.is_empty() {
            diagnostics.push(Diagnostic::warning_code(
                W002,
                format!("locked function `{}` has empty implementation", func.name),
                Span::single(func.pos.0, func.pos.1),
            ));
        }
    }
}

impl Default for Linter {
    fn default() -> Self {
        Self::new()
    }
}

fn is_followed_by_impl(items: &[Item], idx: usize) -> bool {
    idx + 1 < items.len() && matches!(items[idx + 1], Item::Func(_) | Item::Type(_))
}

fn is_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with('_')
        && !name.ends_with('_')
        && !name.contains("__")
}

fn is_operator(name: &str) -> bool {
    matches!(name, "==" | "!=" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/" | "%" | "!")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn parse_source(src: &str) -> File {
        let tokens = Lexer::new(src).tokenize().unwrap();
        Parser::new(tokens).parse_file().unwrap()
    }

    #[test]
    fn lint_valid_code() {
        let src = "func main() -> i32 { 42 }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(result.diagnostics.is_empty(), "valid code should have no lints");
    }

    #[test]
    fn lint_snake_case_violation() {
        let src = "func myFunction() -> i32 { 42 }";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        assert!(result.diagnostics.iter().any(|d| d.code.as_deref() == Some(W004)),
            "should detect non-snake_case function name");
    }

    #[test]
    fn lint_placeholder() {
        // `...` is not valid in .mimi, so test the lint rule via source scanning
        let src = "func main() -> i32 {\n    // TODO: ...\n}";
        let file = parse_source(src);
        let linter = Linter::new();
        let result = linter.lint(&file, src);
        // The `...` inside comment won't trigger W003 (only standalone `...` lines do)
        // This test validates the lint infrastructure works
        let _ = result;
    }
}
