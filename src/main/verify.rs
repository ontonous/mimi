use std::fs;
use std::path::Path;

use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::{lexer, loader, parser};
use mimi::verifier::{VerifStatus, Verifier};
use crate::resolve_path;

pub(crate) fn verify(path: Option<&Path>, show_stats: bool, dump_z3: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    let merged_file = if !file.imports.is_empty() {
        let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main(&path)?;
        loader.merge_all()?
    } else {
        file
    };

    let mut verifier = Verifier::new()?;

    // Dump Z3 assertions before verification if --dump-z3 is set
    if dump_z3 {
        // Run a lightweight no-op check to ensure solver is initialized,
        // then dump debug info
        eprintln!("; Z3 SMT-LIB2 dump for {}", path.display());
        eprintln!("; (verification will proceed after dump)");
    }

    let results = verifier.verify_file(&merged_file);

    // Dump Z3 assertions after population if --dump-z3 is set
    if dump_z3 {
        if let Some(smt2) = verifier.dump_smt2() {
            eprintln!("{}", smt2);
        } else {
            eprintln!("; (no Z3 assertions)");
        }
    }

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
            println!("{:30} {:>10} {:>12} {:>8}", "Function", "Status", "Constraints", "Time");
            println!("{}", "-".repeat(64));
        }

        for r in &results {
            let icon = match r.status {
                VerifStatus::Verified => "\x1b[32m✓\x1b[0m",
                VerifStatus::Failed => "\x1b[31m✗\x1b[0m",
                VerifStatus::Unknown => "\x1b[33m?\x1b[0m",
            };
            total_duration_us += r.duration_us;
            total_constraints += r.constraint_count;

            if show_stats {
                let time_str = if r.duration_us > 1000 {
                    format!("{:.1}ms", r.duration_us as f64 / 1000.0)
                } else {
                    format!("{}µs", r.duration_us)
                };
                let status_str = match r.status {
                    VerifStatus::Verified => "✓ pass",
                    VerifStatus::Failed => "✗ fail",
                    VerifStatus::Unknown => "? unknown",
                };
                println!("{:30} {:>10} {:>12} {:>8}", r.func_name, status_str, r.constraint_count, time_str);
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
                println!("  {} {}: {} [{} constraints]{}", icon, r.func_name, r.message, r.constraint_count, time_str);
            }

            if r.status == VerifStatus::Failed {
                all_passed = false;
            }
        }

        let verified = results.iter().filter(|r| r.status == VerifStatus::Verified).count();
        let total_time_ms = total_duration_us as f64 / 1000.0;
        println!("\n{}/{} verified in {:.1}ms ({} total constraints)",
            verified, results.len(), total_time_ms, total_constraints);

        if show_stats && !results.is_empty() {
            let max_constraints = results.iter().map(|r| r.constraint_count).max().unwrap_or(0);
            let min_constraints = results.iter().map(|r| r.constraint_count).min().unwrap_or(0);
            let avg_time = total_duration_us as f64 / results.len() as f64;
            println!("  (constraint range: {}-{}, avg time: {:.1}µs)",
                min_constraints, max_constraints, avg_time);
        }

        if !all_passed {
            return Err("verification failed".into());
        }
    }
    Ok(())
}
