//! CLI presentation for the checker/MIR query service.
//!
//! The command intentionally loads the same production pipeline as check and
//! mir: source loading, imports, prelude merge, checker, and (when possible)
//! canonical MIR. A query never reconstructs facts from raw syntax after the
//! checked boundary.

use std::path::{Path, PathBuf};

use clap::Subcommand;

use crate::{is_production, resolve_path};
use mimi::core::mir::reference::MirProgram;
use mimi::diagnostic::Diagnostic;
use mimi::query::{diagnostics_response, QueryService};

#[derive(Subcommand, Debug)]
pub(crate) enum QueryAction {
    /// Show a declaration and its resolved references.
    Symbol {
        path: PathBuf,
        /// Qualified or short declaration name (also accepted as --name).
        target: Option<String>,
        #[arg(long = "name")]
        name: Option<String>,
    },
    /// Show all resolved references to a declaration.
    References {
        path: PathBuf,
        target: Option<String>,
        #[arg(long = "name")]
        name: Option<String>,
    },
    /// Show resolved call sites, optionally restricted to one owner.
    Calls {
        path: PathBuf,
        #[arg(long)]
        owner: Option<String>,
    },
    /// Show Flow states and legal transition edges.
    Flow {
        path: PathBuf,
        flow: String,
        #[arg(long)]
        event: Option<String>,
        #[arg(long)]
        source: Option<String>,
    },
    /// Explain checker ownership obligations for one callable.
    Ownership { path: PathBuf, function: String },
    /// Explain declared and inferred effects for one callable.
    Effects { path: PathBuf, function: String },
    /// Explain canonical MIR cost events for one callable.
    Cost { path: PathBuf, function: String },
    /// Explain source impact and conservative rebuild boundaries.
    Impact { path: PathBuf, symbol: String },
    /// Show explicit experimental type-hole/unfinished-obligation status.
    Holes { path: PathBuf },
    /// Normalize checker diagnostics into the machine-facing query schema.
    Diagnostics { path: PathBuf },
    /// Generic alias for the named query above.
    Explain {
        path: PathBuf,
        topic: String,
        target: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CostAction {
    /// Explain canonical MIR cost events for one callable.
    Explain { path: PathBuf, function: String },
}

struct LoadedProgram {
    path: PathBuf,
    checked: mimi::core::resolved::CheckedProgram,
    mir: Option<MirProgram>,
}

pub(crate) fn run(action: QueryAction) -> Result<(), String> {
    if let QueryAction::Diagnostics { path } = &action {
        return run_diagnostics(path);
    }
    let loaded = load(action_path(&action))?;
    let source_name = loaded.path.display().to_string();
    let service = loaded
        .mir
        .as_ref()
        .map(|mir| QueryService::with_mir(&loaded.checked, mir))
        .unwrap_or_else(|| QueryService::new(&loaded.checked))
        .with_source(source_name);
    let result = match action {
        QueryAction::Symbol { target, name, .. } => service.symbol(&required_name(target, name)?),
        QueryAction::References { target, name, .. } => {
            service.references(&required_name(target, name)?)
        }
        QueryAction::Calls { owner, .. } => service.calls(owner.as_deref()),
        QueryAction::Flow {
            flow,
            event,
            source,
            ..
        } => service.flow(&flow, event.as_deref(), source.as_deref()),
        QueryAction::Ownership { function, .. } => service.ownership(&function),
        QueryAction::Effects { function, .. } => service.effects(&function),
        QueryAction::Cost { function, .. } => service.cost(&function),
        QueryAction::Impact { symbol, .. } => service.impact(&symbol),
        QueryAction::Holes { .. } => service.holes(),
        QueryAction::Explain { topic, target, .. } => explain(&service, &topic, target.as_deref())?,
        QueryAction::Diagnostics { .. } => unreachable!("diagnostics handled before loading"),
    };
    print_json(result)
}

pub(crate) fn run_cost(action: CostAction) -> Result<(), String> {
    match action {
        CostAction::Explain { path, function } => {
            let loaded = load(&path)?;
            let source_name = loaded.path.display().to_string();
            let result = loaded
                .mir
                .as_ref()
                .map(|mir| QueryService::with_mir(&loaded.checked, mir))
                .unwrap_or_else(|| QueryService::new(&loaded.checked))
                .with_source(source_name)
                .cost(&function);
            print_json(result)
        }
    }
}

fn action_path(action: &QueryAction) -> &Path {
    match action {
        QueryAction::Symbol { path, .. }
        | QueryAction::References { path, .. }
        | QueryAction::Calls { path, .. }
        | QueryAction::Flow { path, .. }
        | QueryAction::Ownership { path, .. }
        | QueryAction::Effects { path, .. }
        | QueryAction::Cost { path, .. }
        | QueryAction::Impact { path, .. }
        | QueryAction::Holes { path }
        | QueryAction::Diagnostics { path }
        | QueryAction::Explain { path, .. } => path,
    }
}

fn required_name(name: Option<String>, name_flag: Option<String>) -> Result<String, String> {
    name.or(name_flag)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| "query requires a declaration name".to_string())
}

fn explain(
    service: &QueryService<'_>,
    topic: &str,
    target: Option<&str>,
) -> Result<serde_json::Value, String> {
    let target = target.ok_or_else(|| format!("query explain {topic} requires a target"))?;
    Ok(match topic {
        "symbol" => service.symbol(target),
        "references" | "refs" => service.references(target),
        "calls" => service.calls(Some(target)),
        "ownership" | "owner" => service.ownership(target),
        "effects" | "effect" => service.effects(target),
        "cost" => service.cost(target),
        "impact" => service.impact(target),
        "flow" => service.flow(target, None, None),
        "holes" => service.holes(),
        _ => return Err(format!("unknown query topic {topic}")),
    })
}

fn load(path_arg: &Path) -> Result<LoadedProgram, String> {
    let path = resolve_path(Some(path_arg))?;
    if !is_production(&path) {
        return Err(format!(
            "query expects a .mimi production file, got {}",
            path.display()
        ));
    }
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = mimi::lexer::Lexer::new(&source).tokenize()?;
    let file = mimi::loader::parser_for_path(tokens, &path)?.parse_file()?;
    let mut file = if file.imports.is_empty() {
        file
    } else {
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
    };
    mimi::loader::merge_prelude_into(&mut file);
    let checked = mimi::core::check_program(&file).map_err(|diagnostics| {
        format!(
            "type checking failed ({} diagnostic(s)); use mimi query diagnostics {}",
            diagnostics.len(),
            path.display()
        )
    })?;
    // MIR is an optional evidence route. Queries remain useful for checked
    // programs outside a currently materialized island and report that fact.
    let mir = MirProgram::from_checked_program(&checked).ok();
    Ok(LoadedProgram { path, checked, mir })
}

fn run_diagnostics(path_arg: &Path) -> Result<(), String> {
    let path = resolve_path(Some(path_arg))?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let mut diagnostics = Vec::<Diagnostic>::new();
    let tokens = match mimi::lexer::Lexer::new(&source).tokenize() {
        Ok(tokens) => tokens,
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                error.to_string(),
                mimi::span::Span::UNKNOWN,
            ));
            return print_json(diagnostics_response(
                &diagnostics,
                Some(&path.display().to_string()),
            ));
        }
    };
    let (mut file, parse_errors) =
        mimi::loader::parser_for_path(tokens, &path)?.parse_file_with_recovery();
    diagnostics.extend(parse_errors.iter().map(|error| error.to_diagnostic()));
    if diagnostics.is_empty() {
        if !file.imports.is_empty() {
            let base_dir = path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            let mut loader = mimi::loader::ModuleLoader::new(base_dir);
            loader
                .load_main_with_file(&path, file)
                .map_err(|error| format!("failed to load imports: {error}"))?;
            file = loader
                .merge_all()
                .map_err(|error| format!("failed to merge imports: {error}"))?;
        }
        mimi::loader::merge_prelude_into(&mut file);
        if let Err(errors) = mimi::core::check_program(&file) {
            diagnostics.extend(errors);
        }
    }
    print_json(diagnostics_response(
        &diagnostics,
        Some(&path.display().to_string()),
    ))
}

fn print_json(value: serde_json::Value) -> Result<(), String> {
    let rendered = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    println!("{rendered}");
    Ok(())
}
