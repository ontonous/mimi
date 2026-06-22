use std::path::Path;

/// Get the local registry directory (~/.mimi/registry/)
pub fn registry_dir() -> Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| format!("cannot get HOME: {}", e))?;
    let reg_dir = std::path::PathBuf::from(home).join(".mimi").join("registry");
    std::fs::create_dir_all(&reg_dir)
        .map_err(|e| format!("failed to create registry dir: {}", e))?;
    Ok(reg_dir)
}

/// Recursively copy a directory
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst)
        .map_err(|e| format!("mkdir {}: {}", dst.display(), e))?;
    for entry in std::fs::read_dir(src)
        .map_err(|e| format!("read_dir {}: {}", src.display(), e))?
    {
        let entry = entry.map_err(|e| format!("read_dir entry: {}", e))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("copy {}: {}", src_path.display(), e))?;
        }
    }
    Ok(())
}
