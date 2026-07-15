use std::io::Read;
use std::path::Path;

/// Compute a deterministic content-based checksum for a directory.
/// Walks all files (sorted by path), hashes path + content with SHA-256 (P-H9).
pub fn compute_dir_checksum(dir: &Path) -> Result<String, String> {
    let mut entries: Vec<_> = Vec::new();
    collect_files(dir, dir, &mut entries).map_err(|e| format!("failed to read dir: {}", e))?;
    entries.sort();

    // Domain-separated SHA-256 over sorted (relpath, content) pairs.
    let mut stream = Vec::new();
    for path in &entries {
        let rel = path.strip_prefix(dir).unwrap_or(path);
        let rel_bytes = rel.to_string_lossy();
        stream.extend_from_slice(b"PATH\0");
        stream.extend_from_slice(rel_bytes.as_bytes());
        stream.extend_from_slice(b"\0DATA\0");
        match std::fs::File::open(path) {
            Ok(mut f) => {
                let mut buf = Vec::new();
                if let Err(e) = f.read_to_end(&mut buf) {
                    eprintln!(
                        "[mimi] warning: checksum skipping unreadable file {}: {}",
                        path.display(),
                        e
                    );
                } else {
                    let len = (buf.len() as u64).to_le_bytes();
                    stream.extend_from_slice(&len);
                    stream.extend_from_slice(&buf);
                }
            }
            Err(e) => {
                eprintln!(
                    "[mimi] warning: checksum skipping unopenable file {}: {}",
                    path.display(),
                    e
                );
            }
        }
        stream.extend_from_slice(b"\0END\0");
    }

    let digest = crate::runtime::sha256_bytes(&stream);
    Ok(digest.iter().map(|b| format!("{:02x}", b)).collect())
}

fn collect_files(
    _base: &Path,
    dir: &Path,
    entries: &mut Vec<std::path::PathBuf>,
) -> std::io::Result<()> {
    // AU-C4: do not follow symlinks (checksum must not include external trees).
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            collect_files(_base, &path, entries)?;
        } else if ft.is_file() {
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
