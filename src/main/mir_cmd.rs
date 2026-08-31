//! CLI entry point for inspecting canonical MIR.
//!
//! The command is intentionally a checking/debugging surface. It does not
//! compile or execute the source file and it never falls back to a backend
//! emitter when MIR lowering is incomplete.

use std::collections::{BTreeMap, HashSet};
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
    let type_catalog = mimi::core::mir::types::MirTypeCatalog::from_checked_program(&checked)
        .map_err(|errors| {
            format!(
                "MIR type catalog construction failed:\n{}",
                errors.join("\n")
            )
        })?;
    let mut functions = BTreeMap::new();
    let mut lowering_errors = Vec::new();
    for (owner, callable) in checked.callables() {
        if source_ids
            .as_ref()
            .is_some_and(|ids| !ids.contains(&callable.body.root.origin.user_span().source_id))
        {
            continue;
        }
        match mimi::core::mir::lower::lower_callable_with_type_catalog(callable, &type_catalog) {
            Ok(function) => {
                functions.insert(owner.clone(), function);
            }
            Err(mut errors) => lowering_errors.append(&mut errors),
        }
    }
    if !lowering_errors.is_empty() {
        let messages = lowering_errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "MIR lowering is incomplete for {}:\n{messages}",
            path.display()
        ));
    }
    if functions.is_empty() {
        return Err(format!(
            "no callable from {} was selected for MIR inspection",
            path.display()
        ));
    }
    let program =
        mimi::core::mir::reference::MirProgram::with_type_catalog(functions, type_catalog)
            .map_err(|errors| {
                let messages = errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("MIR validation failed:\n{messages}")
            })?;

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
