//! `mimi module` inspection command.
//!
//! The command deliberately inspects the loader snapshot before the legacy
//! file-merge view is consumed.  This gives tooling a stable module identity,
//! export summary, and dependency edge list while `mimi check` and existing
//! builds continue to use their compatibility merge path.

use std::path::Path;

use crate::{is_production, resolve_path};
use mimi::lexer;

pub(crate) fn run(path: Option<&Path>, interface: bool, deps: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    if !is_production(&path) {
        return Err(format!(
            "module inspection expects a .mimi production file, got {}",
            path.display()
        ));
    }
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let (file, parse_errors) =
        mimi::loader::parser_for_path(tokens, &path)?.parse_file_with_recovery();
    if !parse_errors.is_empty() {
        return Err(format!(
            "{} parse error(s) in {}",
            parse_errors.len(),
            path.display()
        ));
    }
    let base_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut loader = mimi::loader::ModuleLoader::new(base_dir);
    loader
        .load_main_with_file(&path, file)
        .map_err(|error| format!("failed to load imports: {error}"))?;
    let graph = mimi::module_system::ModuleGraph::from_loader(&loader)
        .map_err(|error| format!("module graph rejected: {error}"))?;

    if interface {
        for node in graph.modules.values() {
            println!("module {} hash={}", node.id, node.interface.hash);
            if node.interface.unknown {
                println!("  unknown=true");
            }
            for symbol in &node.interface.exports {
                println!("  {}", symbol.canonical_text());
            }
        }
    } else if deps {
        for edge in &graph.edges {
            println!("{} -> {} as {}", edge.from, edge.to, edge.alias);
        }
    } else {
        println!("{}", graph.canonical_text());
    }
    Ok(())
}
