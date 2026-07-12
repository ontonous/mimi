use std::io::Read;
use std::path::Path;

/// Compute a deterministic content-based checksum for a directory.
/// Walks all files (sorted by path), hashes path + content with FNV1a.
///
/// audit (MEDIUM — FNV hash collision):
/// FNV-1a is a non-cryptographic hash.  It is suitable for file
/// identification and change detection (its intended use here), but is
/// NOT collision-resistant against adversarial input.  An attacker who
/// controls file contents can craft collisions that produce the same
/// checksum.  Do not use this checksum for integrity verification,
/// tamper detection, or any security-sensitive purpose — use SHA-256
/// instead (see `crypto.mimi` / `mimi_sha256`).
pub fn compute_dir_checksum(dir: &Path) -> Result<String, String> {
    // FNV-1a offset basis and prime (64-bit).
    // Non-cryptographic: see function-level audit comment above.
    let mut entries: Vec<_> = Vec::new();
    collect_files(dir, dir, &mut entries).map_err(|e| format!("failed to read dir: {}", e))?;
    entries.sort();

    let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis (64-bit)
    for path in &entries {
        // Mix in relative path
        let rel = path.strip_prefix(dir).unwrap_or(path);
        for b in rel.to_string_lossy().as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3); // FNV prime (64-bit)
        }
        // Mix in file content
        if let Ok(mut f) = std::fs::File::open(path) {
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).is_ok() {
                for b in &buf {
                    hash ^= *b as u64;
                    hash = hash.wrapping_mul(0x100000001b3);
                }
            }
        }
    }

    Ok(format!("{:016x}", hash))
}

fn collect_files(
    _base: &Path,
    dir: &Path,
    entries: &mut Vec<std::path::PathBuf>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(_base, &path, entries)?;
        } else {
            entries.push(path);
        }
    }
    Ok(())
}

/// Get the local registry directory (~/.mimi/registry/)
pub fn registry_dir() -> Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| format!("cannot get HOME: {}", e))?;
    let reg_dir = std::path::PathBuf::from(home)
        .join(".mimi")
        .join("registry");
    std::fs::create_dir_all(&reg_dir)
        .map_err(|e| format!("failed to create registry dir: {}", e))?;
    Ok(reg_dir)
}

/// Recursively copy a directory, skipping entries rejected by `filter`.
/// `filter` receives entry file names (not full paths); return true to skip.
///
/// B1/SEC-C8: Symlinks are skipped to prevent escaping the source tree.
pub fn copy_dir_recursive_filtered(
    src: &Path,
    dst: &Path,
    filter: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {}", dst.display(), e))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("read_dir {}: {}", src.display(), e))? {
        let entry = entry.map_err(|e| format!("read_dir entry: {}", e))?;
        let fname = entry.file_name();
        let fname_str = fname.to_string_lossy();
        if filter(&fname_str) {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(fname);
        let file_type = entry.file_type().map_err(|e| format!("file_type: {}", e))?;
        if file_type.is_symlink() {
            // B1/SEC-C8: Skip symlinks to prevent directory traversal attacks.
            continue;
        }
        if file_type.is_dir() {
            copy_dir_recursive_filtered(&src_path, &dst_path, filter)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("copy {}: {}", src_path.display(), e))?;
        }
    }
    Ok(())
}

/// Recursively copy a directory.
///
/// B1/SEC-C8: Symlinks are skipped to prevent escaping the source tree.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {}", dst.display(), e))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("read_dir {}: {}", src.display(), e))? {
        let entry = entry.map_err(|e| format!("read_dir entry: {}", e))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type().map_err(|e| format!("file_type: {}", e))?;
        if file_type.is_symlink() {
            // B1/SEC-C8: Skip symlinks to prevent directory traversal attacks.
            continue;
        }
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("copy {}: {}", src_path.display(), e))?;
        }
    }
    Ok(())
}
