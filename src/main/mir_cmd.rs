//! CLI entry point for inspecting canonical MIR.
//!
//! The command is intentionally a checking/debugging surface. It does not
//! compile or execute the source file and it never falls back to a backend
//! emitter when MIR lowering is incomplete.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use crate::{is_production, resolve_path};

pub(crate) fn mir(
    path: Option<&Path>,
    strict: bool,
    all: bool,
    receipt: bool,
) -> Result<(), String> {
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

    if receipt {
        print!("{}", route_receipt_manifest(&program));
    } else {
        print!("{}", program.type_catalog().canonical_text());
        for transition in program.transitions().values() {
            print!("{}", transition.canonical_text());
        }
        for function in program.functions().values() {
            print!("{}", function.canonical_text());
        }
    }
    eprintln!(
        "✓ {} lowered {} callable(s) to canonical MIR",
        path.display(),
        program.functions().len()
    );
    Ok(())
}

/// Render the route receipt as a stable, line-oriented evidence manifest.
///
/// The manifest deliberately contains only checker/MIR-owned identities. It
/// is suitable for matrix snapshots and remains independent of backend output
/// or source paths.
fn route_receipt_manifest(program: &mimi::core::mir::reference::MirProgram) -> String {
    let receipt = program.route_receipt("cli-mir-v1");
    let mut text = String::from("mimi-mir-route-manifest-v1\n");
    writeln!(text, "schema={}", receipt.schema).expect("String write");
    writeln!(text, "profile={}", receipt.profile).expect("String write");
    writeln!(text, "mir_digest={}", receipt.mir_digest).expect("String write");
    writeln!(text, "type_desc_digest={}", receipt.type_desc_digest).expect("String write");
    writeln!(text, "abi_digest={}", receipt.abi_digest).expect("String write");
    writeln!(text, "ffi_digest={}", receipt.ffi_digest).expect("String write");
    writeln!(text, "ownership_digest={}", receipt.ownership_digest).expect("String write");
    writeln!(
        text,
        "flow_transition_digest={}",
        receipt.flow_transition_digest
    )
    .expect("String write");
    text.push_str("root_owners=");
    for (index, owner) in receipt.root_owners.iter().enumerate() {
        if index != 0 {
            text.push(',');
        }
        text.push_str(owner.0.as_str());
    }
    text.push('\n');
    text
}
