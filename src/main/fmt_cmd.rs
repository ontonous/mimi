use std::fs;
use std::path::PathBuf;

use crate::fmt;

pub(crate) fn fmt_files(files: &[PathBuf], check: bool) -> Result<(), String> {
    let formatter = fmt::Formatter::new();
    let mut had_changes = false;

    if files.is_empty() {
        return Err("no files specified".into());
    }

    for path in files {
        let source = fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        let mut formatted = source.clone();
        let changed = formatter.format_in_place(&mut formatted);

        if check && changed {
            eprintln!("would format: {}", path.display());
            had_changes = true;
        } else if !check && changed {
            fs::write(path, &formatted)
                .map_err(|e| format!("failed to write {}: {}", path.display(), e))?;
            println!("formatted: {}", path.display());
        } else if !check {
            println!("already formatted: {}", path.display());
        }
    }

    if check && had_changes {
        std::process::exit(1);
    }
    Ok(())
}
