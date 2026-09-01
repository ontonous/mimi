//! CLI entry point for inspecting canonical MIR.
//!
//! The command is intentionally a checking/debugging surface. It does not
//! compile or execute the source file and it never falls back to a backend
//! emitter when MIR lowering is incomplete.

use std::collections::HashSet;
use std::path::Path;

use crate::{is_production, resolve_path};

pub(crate) fn mir(path: Option<&Path>, strict: bool, all: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    if !is_production(&path) {
        return Err(format!(
            "expected .mimi production file for MIR inspection, got {}",
            path.display()
        ));
    }
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = mimi::lexer::Lexer::new(&source).tokenize()?;
    let file = mimi::loader::parser_for_path(tokens, &path)?.parse_file()?;

    let mut file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let mut loader = mimi::loader::ModuleLoader::new(base_dir);
        loader
            .load_main_with_file(&path, file)
            .map_err(|error| format!("failed to load imports: {error}"))?;
        loader
            .merge_all()
            .map_err(|error| format!("failed to merge imports: {error}"))?
    } else {
        file
    };
    mimi::loader::merge_prelude_into(&mut file);

    let checked = if strict {
        mimi::core::check_program_strict(&file)
    } else {
        mimi::core::check_program(&file)
    }
    .map_err(|diagnostics| {
        let messages = diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        format!("MIR input failed type checking:\n{messages}")
    })?;

    let source_ids = if all {
        None
    } else {
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        let ids = file
            .sources
            .records()
            .iter()
            .filter(|record| {
                record.disk_path.as_ref().is_some_and(|disk_path| {
                    disk_path == &path
                        || disk_path
                            .canonicalize()
                            .is_ok_and(|candidate| candidate == canonical_path)
                })
            })
            .map(|record| record.id)
            .collect::<HashSet<_>>();
        Some(ids)
    };
    // Keep inspection on the production canonical constructor.  The old
    // command-local lowering omitted generic instance materialization and
    // therefore disagreed with `run/build --mir` for imported typed facades.
    // `None` means the complete user/import graph; `Some` preserves the
    // historical source-only inspection mode.
    let program = crate::canonical_dispatch::build_canonical_program_for_sources(
        &checked,
        &file,
        source_ids.as_ref(),
    )
    .map_err(|error| format!("MIR inspection input rejected: {error}"))?;

    print!("{}", program.type_catalog().canonical_text());
    for function in program.functions().values() {
        print!("{}", function.canonical_text());
    }
    eprintln!(
        "✓ {} lowered {} callable(s) to canonical MIR",
        path.display(),
        program.functions().len()
    );
    Ok(())
}
