use std::collections::HashSet;
use mimi::{lockfile, manifest};

pub(crate) fn tree() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found".into()),
    };

    let pkg_name = manifest.package.as_ref()
        .map(|p| p.name.as_str())
        .unwrap_or("root");
    let pkg_version = manifest.package.as_ref()
        .and_then(|p| p.version.as_deref())
        .unwrap_or("0.0.0");
    println!("{} v{}", pkg_name, pkg_version);

    // Try to load lockfile for installed versions
    let lock = lockfile::Lockfile::load(&dir)?
        .unwrap_or_else(lockfile::Lockfile::new);

    let deps_dir = dir.join(".mimi").join("deps");

    if let Some(deps) = &manifest.dependencies {
        for (i, dep) in deps.iter().enumerate() {
            let is_last = i == deps.len() - 1;
            let prefix = if is_last { "└── " } else { "├── " };
            let version = dep.version.as_deref().unwrap_or("*");

            // Get resolved version from lockfile if available
            let resolved_version = lock.get_package(&dep.name)
                .map(|p| p.version.as_str())
                .unwrap_or(version);

            let source = if let Some(path) = &dep.path {
                format!("(path: {})", path)
            } else if let Some(git) = &dep.git {
                format!("(git: {})", git)
            } else {
                "(registry)".to_string()
            };

            println!("{}{} {} {}", prefix, dep.name, resolved_version, source);

            // Show transitive deps for this dep
            let dep_dir = deps_dir.join(&dep.name);
            let sub_prefix = if is_last { "    " } else { "│   " };
            print_transitive_tree(&dep_dir, &sub_prefix, &mut HashSet::new())?;
        }
    }

    // Also show any installed deps not in manifest (orphans)
    if !lock.package.is_empty() {
        let manifest_names: HashSet<String> = manifest.dependencies.as_ref()
            .map(|d| d.iter().map(|d| d.name.clone()).collect())
            .unwrap_or_default();
        let orphans: Vec<&lockfile::LockEntry> = lock.package.iter()
            .filter(|p| !manifest_names.contains(&p.name))
            .collect();
        if !orphans.is_empty() {
            println!("\n  (installed transitive dependencies)");
            for entry in &orphans {
                println!("  └── {} {}", entry.name, entry.version);
                let dep_dir = deps_dir.join(&entry.name);
                print_transitive_tree(&dep_dir, "      ", &mut HashSet::new())?;
            }
        }
    }

    Ok(())
}

fn print_transitive_tree(dep_dir: &std::path::Path, prefix: &str, visited: &mut HashSet<String>) -> Result<(), String> {
    let dep_manifest_path = dep_dir.join("mimi.toml");
    if !dep_manifest_path.exists() {
        return Ok(());
    }

    if let Ok(Some((_sub_dir, sub_manifest))) = manifest::Manifest::find(dep_dir) {
        if let Some(sub_deps) = &sub_manifest.dependencies {
            for (i, sub_dep) in sub_deps.iter().enumerate() {
                if !visited.insert(sub_dep.name.clone()) {
                    // Already shown in this tree path — skip to avoid cycles
                    println!("{}└── {} * (already listed above)", prefix, sub_dep.name);
                    continue;
                }

                let is_last = i == sub_deps.len() - 1;
                let connector = if is_last { "└── " } else { "├── " };
                let sub_prefix_ext = if is_last { "    " } else { "│   " };

                let sub_version = sub_dep.version.as_deref().unwrap_or("*");
                println!("{}{}{} {}", prefix, connector, sub_dep.name, sub_version);

                // Recurse into transitive sub-deps
                let sub_dep_dir = if sub_dep.path.is_some() || sub_dep.git.is_some() {
                    // For non-registry, can't easily locate — skip
                    continue;
                } else {
                    dep_dir.parent()
                        .map(|p| p.join(&sub_dep.name))
                        .unwrap_or_else(|| std::path::PathBuf::from(&sub_dep.name))
                };

                let child_prefix = format!("{}{}", prefix, sub_prefix_ext);
                print_transitive_tree(&sub_dep_dir, &child_prefix, visited)?;
            }
        }
    }
    Ok(())
}
