use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use mimi::fmt;

pub(crate) fn fmt_files(files: &[PathBuf], check: bool) -> Result<(), String> {
    let formatter = fmt::Formatter::new();
    let mut had_changes = false;

    let paths: Vec<PathBuf> = if files.is_empty() {
        discover_mimi_files()?
    } else if files.len() == 1 && files[0].as_os_str() == "-" {
        let mut source = String::new();
        // CL-H1: bound stdin the same way as file sources (100 MiB).
        let max = mimi::path_safety::MAX_SOURCE_BYTES as usize;
        let mut limited = std::io::stdin().take(max as u64 + 1);
        limited
            .read_to_string(&mut source)
            .map_err(|e| format!("failed to read stdin: {}", e))?;
        if source.len() > max {
            return Err(format!(
                "stdin too large (max {} bytes)",
                mimi::path_safety::MAX_SOURCE_BYTES
            ));
        }
        let formatted = formatter.format(&source);
        print!("{}", formatted);
        return Ok(());
    } else {
        files.to_vec()
    };

    if paths.is_empty() {
        println!("no .mimi files found");
        return Ok(());
    }

    for path in &paths {
        let source = mimi::path_safety::read_source_capped(path)?;
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

/// Discover .mimi files to format.
///
/// If the current working directory contains a `mimi.toml`, format the entry
/// file and all .mimi files in the same directory (non-recursive). Otherwise,
/// format all .mimi files in the current working directory.
fn discover_mimi_files() -> Result<Vec<PathBuf>, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;

    if let Some((dir, manifest)) = mimi::manifest::Manifest::find(&cwd)
        .map_err(|e| format!("failed to locate project manifest: {}", e))?
    {
        let entry = manifest.entry_path(&dir);
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(|e| format!("failed to read directory {}: {}", dir.display(), e))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().map(|ext| ext == "mimi").unwrap_or(false))
            .collect();
        if entry.exists() && !files.contains(&entry) {
            files.push(entry);
        }
        files.sort();
        return Ok(files);
    }

    Ok(list_mimi_files(&cwd))
}

fn list_mimi_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map(|ext| ext == "mimi").unwrap_or(false) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}
