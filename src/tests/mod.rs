pub(crate) mod basic;
pub(crate) mod closures;
pub(crate) mod builtins;
pub(crate) mod v1_2;

use crate::{core, interp, lexer, parser};

pub(crate) fn parse(src: &str) -> crate::ast::File {
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    parser::Parser::new(tokens).parse_file().unwrap()
}

pub(crate) fn run_source(src: &str) -> interp::Value {
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    interp.run().unwrap()
}

pub(crate) fn run_source_result(src: &str) -> Result<interp::Value, String> {
    let tokens = lexer::Lexer::new(src).tokenize().map_err(|e| e)?;
    let file = parser::Parser::new(tokens).parse_file().map_err(|e| e.message)?;
    let mut interp = interp::Interpreter::new(&file);
    interp.run()
}

pub(crate) fn check_source(src: &str) -> Result<(), Vec<core::Diagnostic>> {
    let file = parse(src);
    core::check(&file)
}
