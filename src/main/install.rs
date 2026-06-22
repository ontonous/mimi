use mimi::{lockfile, manifest};

pub(crate) fn install(_all: bool) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found; run 'mimi init' first".into()),
    };

    // Check for dependency conflicts
    let conflicts = manifest.check_conflicts();
    if !conflicts.is_empty() {
        for c in &conflicts {
            eprintln!("warning: {}", c);
        }
    }

    let deps = match &manifest.dependencies {
        Some(d) if !d.is_empty() => d.clone(),
        _ => {
            println!("No dependencies to install.");
            return Ok(());
        }
    };

    let reg = crate::search::registry_dir()?;
    let deps_dir = dir.join(".mimi").join("deps");
    std::fs::create_dir_all(&deps_dir)
        .map_err(|e| format!("failed to create deps dir: {}", e))?;

    let mut installed = 0;
    let mut lock = lockfile::Lockfile::load(&dir)?
        .unwrap_or_else(lockfile::Lockfile::new);
    for dep in &deps {
        if let Some(git_url) = &dep.git {
            let clone_dir = deps_dir.join(&dep.name);
            let tag_arg = dep.tag.as_deref().unwrap_or("main");

            // Try to fetch and checkout the git tag to resolve a stable version
            let status = std::process::Command::new("git")
                .arg("clone").arg("--branch").arg(tag_arg)
                .arg("--depth").arg("1")
                .arg(git_url).arg(&clone_dir)
                .status()
                .map_err(|e| format!("git clone failed: {}", e))?;
            if !status.success() {
                println!("  ⚠ git clone failed for {}", dep.name);
                continue;
            }
            // Resolve the actual commit hash as the "version" for pinning
            let resolved_version = if let Ok(output) = std::process::Command::new("git")
                .arg("rev-parse").arg("--short").arg("HEAD")
                .current_dir(&clone_dir)
                .output()
            {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                tag_arg.to_string()
            };
            println!("  ✓ {} (git: {} @ {} -> {})", dep.name, git_url, tag_arg, resolved_version);
            lock.add_package(&dep.name, &resolved_version, Some(&format!("git+{}", git_url)), None);
            installed += 1;
        } else {
            let source = dep.path.as_deref().unwrap_or("registry");

            if source == "registry" {
            let pkg_dir = reg.join(&dep.name);
            if !pkg_dir.exists() {
                println!("  ⚠ Package '{}' not found in local registry (use 'mimi publish' first)", dep.name);
                continue;
            }

            let version = dep.version.as_deref().unwrap_or("*");
            let versions: Vec<String> = std::fs::read_dir(&pkg_dir)
                .map_err(|e| format!("failed to read registry: {}", e))?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect();

            let version_refs: Vec<&str> = versions.iter().map(|s| s.as_str()).collect();
            let resolved = lockfile::Lockfile::resolve_version(version, &version_refs);

            match resolved {
                Some(v) => {
                    let src = pkg_dir.join(&v);
                    let dst = deps_dir.join(&dep.name);
                    if dst.exists() {
                        std::fs::remove_dir_all(&dst)
                            .map_err(|e| format!("failed to remove old: {}", e))?;
                    }
                    copy_dir_recursive(&src, &dst)
                        .map_err(|e| format!("failed to copy {}: {}", dep.name, e))?;
                    println!("  ✓ {} v{}", dep.name, v);
                    lock.add_package(&dep.name, &v, Some("registry"), None);
                    installed += 1;
                }
                None => {
                    println!("  ⚠ No matching version for '{}' {}", dep.name, version);
                }
            }
        } else {
            let src = std::path::PathBuf::from(source);
            if !src.exists() {
                println!("  ⚠ Path dependency '{}' not found at {}", dep.name, source);
                continue;
            }
            let dst = deps_dir.join(&dep.name);
            if dst.exists() {
                std::fs::remove_dir_all(&dst)
                    .map_err(|e| format!("failed to remove old: {}", e))?;
            }
            copy_dir_recursive(&src, &dst)
                .map_err(|e| format!("failed to copy {}: {}", dep.name, e))?;
            println!("  ✓ {} (path: {})", dep.name, source);
            lock.add_package(&dep.name, "*", Some(&format!("path:{}", source)), None);
            installed += 1;
        }
        }
    }

    lock.save(&dir)?;

    println!("Installed {} package(s).", installed);
    Ok(())
}

/// Recursively copy a directory
pub(crate) fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
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
