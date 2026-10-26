//! CLI entry point for inspecting canonical MIR.
//!
//! The command is intentionally a checking/debugging surface. It does not
//! compile or execute the source file and it never falls back to a backend
//! emitter when MIR lowering is incomplete.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::{is_production, resolve_path};
use mimi::diagnostic::format::{format_diagnostic_with_registry, strip_ansi};

const CHECKER_INSPECTION_MANIFEST_HEADER: &str = "mimi-checker-inspection-manifest-v1";
const CHECKER_INSPECTION_RECEIPT_SCHEMA: &str = "mimi-checker-inspection-receipt-v1";
const CHECKER_INSPECTION_RECEIPT_PROFILE: &str = "cli-checker-inspection-v1";
const INCOMPLETE_MAP_ANY_V2_TYPE_CATALOG_ERROR: &str =
    "ordinary Map/Any owner-v2 TypeCatalog candidate has incomplete or conflicting coverage";
const CHECKER_INSPECTION_RECEIPT_FIELDS: &[&str] = &[
    "schema",
    "profile",
    "snapshot_kind",
    "mir_materialized",
    "source_digest",
    "map_any_owner_v2_admission",
    "root_owners",
];

pub(crate) fn mir(path: Option<&Path>, all: bool, receipt: bool) -> Result<(), String> {
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

    let (mut file, imported_source_texts) = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let mut loader = mimi::loader::ModuleLoader::new(base_dir);
        let imported_source_texts = if all && receipt {
            loader
                .load_main_with_file_and_source_texts(&path, file)
                .map_err(|error| format!("failed to load imports: {error}"))?
                .1
        } else {
            loader
                .load_main_with_file(&path, file)
                .map_err(|error| format!("failed to load imports: {error}"))?;
            HashMap::new()
        };
        let merged = loader
            .merge_all()
            .map_err(|error| format!("failed to merge imports: {error}"))?;
        (merged, imported_source_texts)
    } else {
        (file, HashMap::new())
    };
    let prelude_source_text = mimi::loader::merge_prelude_into_with_source_text(&mut file);
    let checker_source_snapshot = (all && receipt).then(|| {
        checker_inspection_source_snapshot(
            &file.sources.records()[..],
            &path,
            &source,
            &imported_source_texts,
            prelude_source_text.as_deref(),
        )
    });

    let checked = match mimi::core::check_program(&file) {
        Ok(checked) => checked,
        Err(diagnostics) => {
            let fallback_filename = path.display().to_string();
            let messages = diagnostics
                .iter()
                .map(|diagnostic| {
                    strip_ansi(&format_diagnostic_with_registry(
                        diagnostic,
                        &file.sources,
                        Some(&source),
                        &fallback_filename,
                    ))
                })
                .collect::<String>();
            return Err(format!("MIR input failed type checking:\n{messages}"));
        }
    };

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
    let program =
        match crate::canonical_dispatch::build_canonical_program_for_sources_with_diagnostics(
            &checked,
            &file,
            source_ids.as_ref(),
        ) {
            Ok(program) => program,
            Err(mimi::core::mir::reference::MirProgramBuildError::Types(errors))
                if all
                    && receipt
                    && errors.len() == 1
                    && errors[0] == INCOMPLETE_MAP_ANY_V2_TYPE_CATALOG_ERROR
                    && mimi::core::mir::classify_canonical_mir_route_admission(&checked)
                        .ordinary_map_owned_any_v2
                        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage =>
            {
                // This is a checker-only diagnostic snapshot. We reach it only
                // after canonical MIR construction failed for the exact V2
                // TypeCatalog coverage error; no lowering or validation failure
                // is converted into a receipt, and no MIR identity is claimed.
                let source_snapshot = checker_source_snapshot
                    .as_ref()
                    .ok_or_else(|| {
                        "Checker inspection source snapshot was not captured".to_string()
                    })?
                    .as_ref()
                    .map_err(Clone::clone)?;
                let manifest = checker_inspection_receipt_manifest(&checked, source_snapshot)?;
                print!("{manifest}");
                eprintln!(
                "✓ {} emitted a read-only Checker inspection receipt for incomplete Map/Any owner-v2 coverage; MIR was not materialized",
                path.display()
            );
                return Ok(());
            }
            // Keep the typed build failure intact until this CLI presentation
            // boundary. Other direct consumers can inspect `to_diagnostics()` and
            // `diagnostic_codes()` without parsing the aggregate display text.
            Err(error) => return Err(format!("MIR inspection input rejected: {error}")),
        };

    if receipt {
        let manifest = route_receipt_manifest(&program)?;
        print!("{manifest}");
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
fn route_receipt_manifest(
    program: &mimi::core::mir::reference::MirProgram,
) -> Result<String, String> {
    let receipt = program.route_receipt("cli-mir-v1");
    receipt.manifest_text()
}

type CheckerInspectionSourceSnapshot = Vec<(String, mimi::span::SourceId, Vec<u8>)>;

/// Capture source bytes already read by the CLI, module loader, or prelude
/// merger before checking begins. These snapshots are passed through the
/// fallback path so it never reopens files after the CheckedProgram exists.
fn checker_inspection_source_snapshot(
    source_records: &[mimi::span::SourceRecord],
    main_path: &Path,
    main_source: &str,
    imported_source_texts: &HashMap<std::path::PathBuf, String>,
    prelude_source_text: Option<&str>,
) -> Result<CheckerInspectionSourceSnapshot, String> {
    const PRELUDE_SOURCE_KEY: &str = "stdlib:prelude.mimi";
    let canonical_main_path = main_path
        .canonicalize()
        .unwrap_or_else(|_| main_path.to_path_buf());
    let mut sources = Vec::with_capacity(source_records.len());
    for record in source_records {
        let source_text = if record.key.as_str() == PRELUDE_SOURCE_KEY {
            prelude_source_text.ok_or_else(|| {
                "Checker inspection prelude source text was not captured".to_string()
            })?
        } else {
            let disk_path = record.disk_path.as_ref().ok_or_else(|| {
                format!(
                    "Checker inspection source {} has no retained source text",
                    record.key.as_str()
                )
            })?;
            if disk_path
                .canonicalize()
                .is_ok_and(|candidate| candidate == canonical_main_path)
            {
                main_source
            } else {
                imported_source_texts
                    .get(disk_path)
                    .map(String::as_str)
                    .ok_or_else(|| {
                        format!(
                            "Checker inspection source {} was not retained by the module loader",
                            record.key.as_str()
                        )
                    })?
            }
        };
        sources.push((
            record.key.as_str().to_owned(),
            record.id,
            source_text.as_bytes().to_vec(),
        ));
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(sources)
}

/// Render a distinct, non-route receipt for the exact checked input sources
/// behind an incomplete Map/Any owner-v2 TypeCatalog rejection. This hashes
/// sorted source bytes only; it deliberately has no MIR digest, materialized
/// Checker-program digest, or executable-route fields.
fn checker_inspection_receipt_manifest(
    checked: &mimi::core::CheckedProgram,
    sources: &[(String, mimi::span::SourceId, Vec<u8>)],
) -> Result<String, String> {
    let admission =
        mimi::core::mir::classify_canonical_mir_route_admission(checked).ordinary_map_owned_any_v2;
    if admission != mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage {
        return Err(
            "Checker inspection receipt requires incomplete ordinary Map/Any owner-v2 coverage"
                .into(),
        );
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mimi-checker-inspection-source-digest-v1");
    for (key, _, source_bytes) in sources {
        let key_len = u64::try_from(key.len())
            .map_err(|_| "inspection source key length does not fit u64".to_string())?;
        let source_len = u64::try_from(source_bytes.len())
            .map_err(|_| "inspection source length does not fit u64".to_string())?;
        hasher.update(&key_len.to_be_bytes());
        hasher.update(key.as_bytes());
        hasher.update(&source_len.to_be_bytes());
        hasher.update(source_bytes);
    }
    let source_digest = hasher.finalize().to_hex().to_string();
    let included_sources = sources
        .iter()
        .filter(|(key, _, _)| key.as_str() != "stdlib:prelude.mimi")
        .map(|(_, source_id, _)| *source_id)
        .collect::<HashSet<_>>();
    let root_owners = checked
        .callables()
        .values()
        .filter(|callable| {
            included_sources.contains(&callable.body.root.origin.user_span().source_id)
        })
        .map(|callable| callable.owner.0.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let values = [
        CHECKER_INSPECTION_RECEIPT_SCHEMA,
        CHECKER_INSPECTION_RECEIPT_PROFILE,
        "checked-program-only",
        "false",
        source_digest.as_str(),
        "incomplete_coverage",
        root_owners.as_str(),
    ];
    let mut manifest = String::from(CHECKER_INSPECTION_MANIFEST_HEADER);
    manifest.push('\n');
    for (field, value) in CHECKER_INSPECTION_RECEIPT_FIELDS.iter().zip(values) {
        use std::fmt::Write as _;
        writeln!(manifest, "{field}={value}")
            .map_err(|_| "failed to render Checker inspection receipt".to_string())?;
    }
    Ok(manifest)
}
