use std::path::Path;

use crate::resolve_path;
use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::verifier::{TrustedSubsetDomain, VerifStatus};
use mimi::{lexer, loader};

fn verification_blocks_success(
    status: &VerifStatus,
    constraint_count: usize,
    message: &str,
    domain: Option<TrustedSubsetDomain>,
) -> bool {
    // NoObligations never blocks — no proof was attempted.
    if *status == VerifStatus::NoObligations {
        return false;
    }
    let no_contracts = status.is_inconclusive()
        && constraint_count == 0
        && matches!(message, "no contracts" | "no contracts to verify");
    // v0.31.25 验证域隔离：
    // - Contract-level NotInTrustedSubset → hard error (blocks)
    // - Body-level NotInTrustedSubset → doesn't block (treated as SolverUnknown)
    let body_level_not_in_subset = *status == VerifStatus::NotInTrustedSubset
        && matches!(domain, Some(TrustedSubsetDomain::Body));
    *status == VerifStatus::Disproven
        || (status.is_inconclusive() && !no_contracts && !body_level_not_in_subset)
}

pub(crate) fn verify(
    path: Option<&Path>,
    show_stats: bool,
    dump_z3: bool,
    mir: bool,
) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = loader::parser_for_path(tokens, &path)?.parse_file()?;

    let merged_file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main_with_file(&path, file)?;
        loader.merge_all()?
    } else {
        file
    };

    // V-H8: typecheck before Z3 so ill-typed sources cannot produce
    // meaningless positive verification results.
    let checked_program = match mimi::core::check_program(&merged_file) {
        Ok(program) => program,
        Err(diags) => {
            let use_color = colors_enabled();
            let src_ref = Some(source.as_str());
            let filename = path.display().to_string();
            for d in &diags {
                let formatted = format_diagnostic(d, src_ref, &filename);
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            }
            return Err(format!(
                "typecheck failed before verify ({} diagnostic(s))",
                diags.len()
            ));
        }
    };

    // P1-24: compute source hash for ProofArtifact tamper detection.
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let canonical = if mir {
        if dump_z3 {
            return Err("--dump-z3 is not available with --mir".into());
        }
        Some(
            crate::canonical_dispatch::build_canonical_program(&checked_program, &merged_file)
                .map_err(|error| format!("canonical MIR verifier input rejected: {error}"))?,
        )
    } else if dump_z3 {
        None
    } else {
        match crate::canonical_dispatch::select_default_route(&checked_program, &merged_file) {
            crate::canonical_dispatch::DefaultMirRoute::Canonical(canonical) => Some(canonical),
            crate::canonical_dispatch::DefaultMirRoute::Legacy(reason) => {
                crate::canonical_dispatch::report_legacy_route(reason);
                None
            }
            crate::canonical_dispatch::DefaultMirRoute::Rejected(reason) => {
                return Err(format!("default Canonical MIR route rejected: {reason}"));
            }
        }
    };

    let results = if let Some(canonical) = canonical {
        // The default route is selected only after the shared dispatcher has
        // preflighted every consumer.  The verifier still validates its own
        // input at the final consumer boundary and never falls back.
        mimi::verifier::verify_mir(&canonical, source_hash)?
    } else if dump_z3 {
        // --dump-z3 needs access to Verifier::dump_smt2 after verification,
        // which the Flow state machine doesn't expose. Keep direct for this case.
        let mut verifier = mimi::verifier::Verifier::new()?;
        verifier.set_source_hash(source_hash);
        eprintln!("; Z3 SMT-LIB2 dump for {}", path.display());
        eprintln!("; (verification will proceed after dump)");
        let results = verifier.verify_checked(&checked_program);
        if let Some(smt2) = verifier.dump_smt2() {
            eprintln!("{}", smt2);
        } else {
            eprintln!("; (no Z3 assertions)");
        }
        results
    } else {
        // 0.34.44 (ADR-008 §3): the CLI main judgment is DUAL-engine —
        // resolved (primary) + flow/VIR (demoted math: channel) with
        // fail-closed divergence (E0439). Neither engine is trusted alone
        // when their verdict classes disagree.
        mimi::verifier::verify_checked_dual(&checked_program, source_hash)?
    };

    if results.is_empty() {
        println!("No contracts to verify in {}", path.display());
    } else {
        let use_color = colors_enabled();
        let src_ref = Some(source.as_str());
        let filename = &path.display().to_string();
        let mut all_passed = true;
        let mut total_duration_us: u64 = 0;
        let mut total_constraints: usize = 0;

        // Show per-function stats table if --stats is set
        if show_stats {
            println!(
                "{:30} {:>10} {:>12} {:>8}",
                "Function", "Status", "Constraints", "Time"
            );
            println!("{}", "-".repeat(64));
        }

        for r in &results {
            let icon = if r.status == VerifStatus::Proven {
                "\x1b[32m✓\x1b[0m"
            } else if r.status == VerifStatus::Disproven {
                "\x1b[31m✗\x1b[0m"
            } else {
                "\x1b[33m?\x1b[0m"
            };
            total_duration_us += r.duration_us;
            total_constraints += r.constraint_count;

            if show_stats {
                let time_str = if r.duration_us > 1000 {
                    format!("{:.1}ms", r.duration_us as f64 / 1000.0)
                } else {
                    format!("{}µs", r.duration_us)
                };
                let status_str = if r.status == VerifStatus::Proven {
                    "✓ pass"
                } else if r.status == VerifStatus::Disproven {
                    "✗ fail"
                } else {
                    "? unknown"
                };
                println!(
                    "{:30} {:>10} {:>12} {:>8}",
                    r.func_name, status_str, r.constraint_count, time_str
                );
            }

            if let Some(diag) = &r.diagnostic {
                let formatted = format_diagnostic(diag, src_ref, filename);
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            } else if !show_stats {
                let time_str = if r.duration_us > 1000 {
                    format!(" ({:.1}ms)", r.duration_us as f64 / 1000.0)
                } else {
                    format!(" ({}µs)", r.duration_us)
                };
                println!(
                    "  {} {}: {} [{} constraints]{}",
                    icon, r.func_name, r.message, r.constraint_count, time_str
                );
            }

            if verification_blocks_success(
                &r.status,
                r.constraint_count,
                &r.message,
                r.trusted_subset_domain,
            ) {
                all_passed = false;
            }
        }

        let verified = results
            .iter()
            .filter(|r| r.status == VerifStatus::Verified)
            .count();
        let total_time_ms = total_duration_us as f64 / 1000.0;
        println!(
            "\n{}/{} verified in {:.1}ms ({} total constraints)",
            verified,
            results.len(),
            total_time_ms,
            total_constraints
        );

        if show_stats && !results.is_empty() {
            let max_constraints = results
                .iter()
                .map(|r| r.constraint_count)
                .max()
                .unwrap_or(0);
            let min_constraints = results
                .iter()
                .map(|r| r.constraint_count)
                .min()
                .unwrap_or(0);
            let avg_time = total_duration_us as f64 / results.len() as f64;
            println!(
                "  (constraint range: {}-{}, avg time: {:.1}µs)",
                min_constraints, max_constraints, avg_time
            );
        }

        if !all_passed {
            return Err("verification failed or was inconclusive".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::verification_blocks_success;
    use mimi::verifier::{TrustedSubsetDomain, VerifStatus};

    #[test]
    fn genuine_unknown_blocks_cli_success() {
        assert!(verification_blocks_success(
            &VerifStatus::SolverUnknown,
            1,
            "could not encode ensures",
            None,
        ));
        assert!(verification_blocks_success(
            &VerifStatus::InfrastructureError,
            0,
            "Z3 solver not available",
            None,
        ));
    }

    #[test]
    fn no_contract_result_is_neutral() {
        assert!(!verification_blocks_success(
            &VerifStatus::NoObligations,
            0,
            "no contracts to verify",
            None,
        ));
    }

    #[test]
    fn contract_level_not_in_trusted_subset_blocks() {
        // v0.31.25: contract-level NotInTrustedSubset is a hard error.
        assert!(verification_blocks_success(
            &VerifStatus::NotInTrustedSubset,
            1,
            "could not encode extern requires for Z3",
            Some(TrustedSubsetDomain::Contract),
        ));
    }

    #[test]
    fn body_level_not_in_trusted_subset_does_not_block() {
        // v0.31.25: body-level NotInTrustedSubset doesn't block mimi verify.
        assert!(!verification_blocks_success(
            &VerifStatus::NotInTrustedSubset,
            1,
            "body contains unsupported constructs",
            Some(TrustedSubsetDomain::Body),
        ));
    }
}
