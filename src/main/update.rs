use mimi::{lockfile, manifest};
use mimi::pkg_registry;

pub(crate) fn update() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found; run 'mimi init' first".into()),
    };

    let deps = match &manifest.dependencies {
        Some(d) if !d.is_empty() => d.clone(),
        _ => {
            println!("No dependencies to update.");
            return Ok(());
        }
    };

    let reg = pkg_registry::registry_dir()?;
    let deps_dir = dir.join(".mimi").join("deps");

    let mut updated = 0;
    let mut lock = lockfile::Lockfile::load(&dir)?
        .unwrap_or_else(lockfile::Lockfile::new);

    for dep in &deps {
        if let Some(git_url) = &dep.git {
            let clone_dir = deps_dir.join(&dep.name);
            let tag_arg = dep.tag.as_deref().unwrap_or("main");

            if clone_dir.exists() {
                std::fs::remove_dir_all(&clone_dir)
                    .map_err(|e| format!("failed to remove old {}: {}", dep.name, e))?;
            }

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
            let resolved_version = if let Ok(output) = std::process::Command::new("git")
                .arg("rev-parse").arg("--short").arg("HEAD")
                .current_dir(&clone_dir)
                .output()
            {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                tag_arg.to_string()
            };

            let old_version = lock.get_package(&dep.name).map(|p| p.version.clone());
            lock.add_package(&dep.name, &resolved_version, Some(&format!("git+{}", git_url)), None);
            match old_version {
                Some(v) if v != resolved_version => println!("  ↑ {} ({} → {})", dep.name, v, resolved_version),
                Some(v) => println!("  = {} ({})", dep.name, v),
                None => println!("  ✓ {} (git: {} @ {})", dep.name, git_url, tag_arg),
            }
            updated += 1;
        } else {
            let source = dep.path.as_deref().unwrap_or("registry");

            if source == "registry" {
                let pkg_dir = reg.join(&dep.name);
                if !pkg_dir.exists() {
                    println!("  ⚠ Package '{}' not found in local registry", dep.name);
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
                        pkg_registry::copy_dir_recursive(&src, &dst)
                            .map_err(|e| format!("failed to copy {}: {}", dep.name, e))?;

                        let old_version = lock.get_package(&dep.name).map(|p| p.version.clone());
                        lock.add_package(&dep.name, &v, Some("registry"), None);
                        match old_version {
                            Some(ov) if ov != v => println!("  ↑ {} ({} → {})", dep.name, ov, v),
                            Some(_) => println!("  = {} ({})", dep.name, v),
                            None => println!("  ✓ {} v{}", dep.name, v),
                        }
                        updated += 1;
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
                pkg_registry::copy_dir_recursive(&src, &dst)
                    .map_err(|e| format!("failed to copy {}: {}", dep.name, e))?;
                println!("  ✓ {} (path: {})", dep.name, source);
                lock.add_package(&dep.name, "*", Some(&format!("path:{}", source)), None);
                updated += 1;
            }
        }
    }

    lock.save(&dir)?;
    println!("Updated {} package(s).", updated);
    Ok(())
}
