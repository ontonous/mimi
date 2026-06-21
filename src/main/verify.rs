use std::fs;
use std::path::Path;

use crate::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use crate::{lexer, loader, parser, resolve_path, verifier};

pub(crate) fn verify(path: Option<&Path>) -> Result<(), String> {
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

    let results = verifier::verify_file(&merged_file)?;
    if results.is_empty() {
        println!("No contracts to verify in {}", path.display());
    } else {
        let use_color = colors_enabled();
        let src_ref = Some(source.as_str());
        let filename = &path.display().to_string();
        let mut all_passed = true;
        let mut total_duration_us: u64 = 0;
        let mut total_constraints: usize = 0;
        for r in &results {
            let icon = match r.status {
                verifier::VerifStatus::Verified => "\x1b[32m✓\x1b[0m",
                verifier::VerifStatus::Failed => "\x1b[31m✗\x1b[0m",
                verifier::VerifStatus::Unknown => "\x1b[33m?\x1b[0m",
            };
            total_duration_us += r.duration_us;
            total_constraints += r.constraint_count;
            if let Some(diag) = &r.diagnostic {
                let formatted = format_diagnostic(diag, src_ref, filename);
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            } else {
                let time_str = if r.duration_us > 1000 {
                    format!(" ({:.1}ms)", r.duration_us as f64 / 1000.0)
                } else {
                    format!(" ({}µs)", r.duration_us)
                };
                println!("  {} {}: {} [{} constraints]{}", icon, r.func_name, r.message, r.constraint_count, time_str);
            }
            if r.status == verifier::VerifStatus::Failed {
                all_passed = false;
            }
        }
        let verified = results.iter().filter(|r| r.status == verifier::VerifStatus::Verified).count();
        let total_time_ms = total_duration_us as f64 / 1000.0;
        println!("\n{}/{} verified in {:.1}ms ({} total constraints)",
            verified, results.len(), total_time_ms, total_constraints);
        if !all_passed {
            return Err("verification failed".into());
        }
    }
    Ok(())
}
