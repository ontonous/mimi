pub(crate) fn search(query: &str) -> Result<(), String> {
    let reg = registry_dir()?;
    if !reg.exists() {
        println!("Registry is empty. Use 'mimi publish' to add packages.");
        return Ok(());
    }

    let mut found = 0;
    for entry in std::fs::read_dir(&reg)
        .map_err(|e| format!("failed to read registry: {}", e))?
    {
        let entry = entry.map_err(|e| format!("read entry: {}", e))?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let pkg_name = entry.file_name();
        let pkg_name_str = pkg_name.to_string_lossy();

        if !query.is_empty() && !pkg_name_str.contains(query) {
            continue;
        }

        let pkg_dir = entry.path();
        let versions: Vec<String> = std::fs::read_dir(&pkg_dir)
            .map_err(|e| format!("read versions: {}", e))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect();

        if versions.is_empty() {
            continue;
        }

        println!("{} ({})", pkg_name_str, versions.join(", "));
        found += 1;
    }

    if found == 0 {
        if query.is_empty() {
            println!("Registry is empty. Use 'mimi publish' to add packages.");
        } else {
            println!("No packages found matching '{}'.", query);
        }
    }

    Ok(())
}

/// Get the local registry directory (~/.mimi/registry/)
pub(crate) fn registry_dir() -> Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| format!("cannot get HOME: {}", e))?;
    let reg_dir = std::path::PathBuf::from(home).join(".mimi").join("registry");
    std::fs::create_dir_all(&reg_dir)
        .map_err(|e| format!("failed to create registry dir: {}", e))?;
    Ok(reg_dir)
}
