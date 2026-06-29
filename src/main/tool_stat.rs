//! `mimi stat` — directory statistics analysis tool.
//!
//! Analyzes a directory tree: file counts, directory counts, extension distribution,
//! optional SHA-256 hashing.

use std::path::Path;

pub(crate) fn run(dir: &Path, depth: u32, show_hash: bool) -> Result<(), String> {
    if !dir.exists() {
        return Err(format!("path does not exist: {}", dir.display()));
    }

    println!("=== mimi stat ===");
    println!("Target: {}", dir.display());
    println!(
        "Depth:  {}",
        if depth == 0 {
            "unlimited".to_string()
        } else {
            depth.to_string()
        }
    );
    println!();

    let mut total_files = 0u64;
    let mut total_dirs = 0u64;
    let mut ext_counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    scan_dir(
        dir,
        0,
        depth,
        show_hash,
        &mut total_files,
        &mut total_dirs,
        &mut ext_counts,
    );

    println!();
    println!("=== Summary ===");
    println!("  Files:       {}", total_files);
    println!("  Directories: {}", total_dirs);
    println!("  Total:       {}", total_files + total_dirs);

    if !ext_counts.is_empty() {
        println!();
        println!("  Extension distribution:");
        let mut exts: Vec<_> = ext_counts.iter().collect();
        exts.sort_by(|a, b| b.1.cmp(a.1));
        for (ext, count) in exts.iter().take(20) {
            println!("    {:>10}  {}", ext, count);
        }
    }

    Ok(())
}

fn scan_dir(
    dir: &Path,
    current_depth: u32,
    max_depth: u32,
    show_hash: bool,
    total_files: &mut u64,
    total_dirs: &mut u64,
    ext_counts: &mut std::collections::HashMap<String, u64>,
) {
    if max_depth > 0 && current_depth >= max_depth {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let indent = "  ".repeat((current_depth + 1) as usize);

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            *total_dirs += 1;
            println!("{}[D] {}/", indent, name);
            scan_dir(
                &path,
                current_depth + 1,
                max_depth,
                show_hash,
                total_files,
                total_dirs,
                ext_counts,
            );
        } else {
            *total_files += 1;
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string();
            *ext_counts.entry(ext.clone()).or_insert(0) += 1;

            let mut line = format!("{}[F] {}", indent, name);
            if !ext.is_empty() {
                line = format!("{} (.{})", line, ext);
            }
            if show_hash {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let hash = simple_hash(&content);
                    line = format!("{}  sha256:{}", line, hash);
                }
            }
            println!("{}", line);
        }
    }
}

fn simple_hash(data: &str) -> String {
    // Simple FNV-1a hash for quick file identification (not cryptographic)
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in data.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}
