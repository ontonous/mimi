use std::fs;
use std::path::Path;

use crate::{count_commitments, lexer, parser, resolve_path};

pub(crate) fn stats(path: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    count_commitments(&file.items, &mut counts);

    let total: usize = counts.values().sum();
    if total == 0 {
        println!("No commitment suffixes found in {}", path.display());
        return Ok(());
    }

    println!("Commitment distribution for {}:", path.display());
    println!("  total items: {}", total);
    println!();

    let mut sorted: Vec<_> = counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));

    for (name, count) in &sorted {
        let pct = (**count as f64 / total as f64) * 100.0;
        let bar_len = (pct / 5.0) as usize;
        let bar: String = "█".repeat(bar_len);
        println!("  {:<20} {:>4} ({:>5.1}%) {}", name, count, pct, bar);
    }

    // Cognitive alignment assessment
    println!();
    let _unlocked = counts.get("None").copied().unwrap_or(0);
    let tentative = counts.get("?").copied().unwrap_or(0)
        + counts.get("??").copied().unwrap_or(0);
    let locked = counts.get("$").copied().unwrap_or(0)
        + counts.get("$$").copied().unwrap_or(0);

    if total > 0 {
        let tentative_pct = tentative as f64 / total as f64;
        let locked_pct = locked as f64 / total as f64;

        if tentative_pct > 0.3 {
            println!("⚠ High uncertainty: {:.0}% of items are tentative (?/??).", tentative_pct * 100.0);
            println!("  Consider reviewing uncertain designs before proceeding.");
        }
        if locked_pct > 0.5 {
            println!("⚠ High lock-in: {:.0}% of items are locked ($/$$).", locked_pct * 100.0);
            println!("  Consider whether this level of lock-in is appropriate.");
        }
        if tentative_pct < 0.1 && locked_pct > 0.3 {
            println!("✓ Good balance: low uncertainty with moderate lock-in.");
        }
    }

    Ok(())
}
