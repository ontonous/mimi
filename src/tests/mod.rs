pub(crate) mod basic_control_flow;
pub(crate) mod basic_let;
pub(crate) mod basic_functions;
pub(crate) mod basic_operators;
pub(crate) mod basic_literals;
pub(crate) mod basic_lists;
pub(crate) mod basic_tuples;
pub(crate) mod basic_other;
pub(crate) mod closures;

pub(crate) mod strings;
pub(crate) mod builtin_funcs;
pub(crate) mod typecheck;
pub(crate) mod error_handling;
pub(crate) mod visibility;
pub(crate) mod contracts;
pub(crate) mod comptime;
pub(crate) mod ownership;
pub(crate) mod actors;
pub(crate) mod capabilities;
pub(crate) mod generics;
pub(crate) mod extern_blocks;
pub(crate) mod comprehension;

pub(crate) mod v1_2_generics;
pub(crate) mod v1_2_traits;
pub(crate) mod v1_2_parasteps;
pub(crate) mod v1_2_mms;
pub(crate) mod v1_2_effects;
pub(crate) mod v1_2_contract_extract;
pub(crate) mod v1_2_verification;
pub(crate) mod v1_2_static;
pub(crate) mod v1_2_boundary;
pub(crate) mod v1_2_error_paths;
pub(crate) mod v1_2_modules;
pub(crate) mod v1_2_commitment;
pub(crate) mod v1_2_allocators;
pub(crate) mod v1_2_codegen;
pub(crate) mod v1_2_operators;
pub(crate) mod v1_2_generics_misc;
pub(crate) mod v1_2_traits_misc;
pub(crate) mod v1_2_type_def_misc;
pub(crate) mod v1_2_builtin_hof;
pub(crate) mod v1_2_infra;
pub(crate) mod v1_2_misc_remaining;

pub(crate) mod loader;
pub(crate) mod manifest;
pub(crate) mod lsp;
pub(crate) mod extern_calls;
pub(crate) mod actor_concurrent;
pub(crate) mod derive_methods;
pub(crate) mod builtin_extended;
pub(crate) mod cap_runtime;

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
    interp.verify_contracts = true;
    interp.run()
}

pub(crate) fn check_source(src: &str) -> Result<(), Vec<core::Diagnostic>> {
    let file = parse(src);
    core::check(&file)
}

pub(crate) fn check_source_strict(src: &str) -> Result<(), Vec<core::Diagnostic>> {
    let file = parse(src);
    core::check_strict(&file)
}
