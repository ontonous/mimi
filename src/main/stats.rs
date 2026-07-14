use std::path::Path;

use crate::resolve_path;
use mimi::{lexer, parser};

pub(crate) fn stats(path: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    let func_count = file
        .items
        .iter()
        .filter(|i| matches!(i, mimi::ast::Item::Func(_)))
        .count();
    let type_count = file
        .items
        .iter()
        .filter(|i| matches!(i, mimi::ast::Item::Type(_)))
        .count();
    let module_count = file
        .items
        .iter()
        .filter(|i| matches!(i, mimi::ast::Item::Module(_)))
        .count();
    let total = file.items.len();

    println!("Mimi source statistics for {}:", path.display());
    println!("  total items: {}", total);
    println!("  functions:   {}", func_count);
    println!("  types:       {}", type_count);
    println!("  modules:     {}", module_count);
    println!("  lines:       {}", source.lines().count());

    Ok(())
}
