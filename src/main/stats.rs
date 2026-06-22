use std::fs;
use std::path::Path;

use crate::{lexer, parser, resolve_path};

pub(crate) fn stats(path: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    let func_count = file.items.iter().filter(|i| matches!(i, crate::ast::Item::Func(_))).count();
    let type_count = file.items.iter().filter(|i| matches!(i, crate::ast::Item::Type(_))).count();
    let module_count = file.items.iter().filter(|i| matches!(i, crate::ast::Item::Module(_))).count();
    let total = file.items.len();

    println!("Mimi source statistics for {}:", path.display());
    println!("  total items: {}", total);
    println!("  functions:   {}", func_count);
    println!("  types:       {}", type_count);
    println!("  modules:     {}", module_count);
    println!("  lines:       {}", source.lines().count());

    Ok(())
}
