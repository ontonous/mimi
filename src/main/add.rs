use mimi::{lockfile, manifest, pkg_registry};

pub(crate) fn add(
    name: &str,
    version: Option<&str>,
    path: Option<&str>,
    git: Option<&str>,
    tag: Option<&str>,
    dry_run: bool,
) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, mut manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found; run 'mimi init' first".into()),
    };

    // Dry-run: report what would be written, then exit without touching the manifest.
    if dry_run {
        let kind = if git.is_some() {
            format!("git+{}@{}", git.unwrap(), tag.unwrap_or("main"))
        } else if let Some(p) = path {
            format!("path:{}", p)
        } else {
            format!("registry:{}", version.unwrap_or("*"))
        };
        println!("[dry-run] would add: {} ({})", name, kind);
        return Ok(());
    }

    // Detect a duplicate and report it clearly.
    let already_present = manifest
        .dependencies
        .as_ref()
        .map(|d| d.iter().any(|x| x.name == name))
        .unwrap_or(false);

    manifest.add_dependency(name, version, path, git, tag);

    // If registry dep, try to resolve a concrete version and merge into lockfile
    // so that subsequent `mimi install` is a no-op for this package.
    if git.is_none() && path.is_none() {
        let reg = pkg_registry::registry_dir()?;
        let pkg_dir = reg.join(name);
        if pkg_dir.exists() {
            let versions: Vec<String> = std::fs::read_dir(&pkg_dir)
                .map_err(|e| format!("failed to read registry: {}", e))?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect();
            let version_refs: Vec<&str> = versions.iter().map(|s| s.as_str()).collect();
            let constraint = version.unwrap_or("*");
            if let Some(picked) =
                lockfile::Lockfile::resolve_version(constraint, &version_refs)
            {
                let mut lock =
                    lockfile::Lockfile::load(&dir)?.unwrap_or_else(lockfile::Lockfile::new);
                lock.add_package(name, &picked, Some("registry"), None);
                lock.save(&dir)?;
                if already_present {
                    println!(
                        "✓ Updated dependency '{}' -> {} (resolved v{})",
                        name, constraint, picked
                    );
                } else {
                    println!(
                        "✓ Added dependency '{}' (resolved v{})",
                        name, picked
                    );
                }
                manifest.save(&dir)?;
                return Ok(());
            }
        }
    }

    manifest.save(&dir)?;
    if already_present {
        println!("✓ Updated dependency '{}'", name);
    } else {
        println!("✓ Added dependency '{}'", name);
    }
    Ok(())
}
