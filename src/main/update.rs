use std::collections::HashSet;
use mimi::{lockfile, manifest, pkg_registry, pkg_resolve};

pub(crate) fn update() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found; run 'mimi init' first".into()),
    };

    let direct_deps = match &manifest.dependencies {
        Some(d) if !d.is_empty() => d.clone(),
        _ => {
            println!("No dependencies to update.");
            return Ok(());
        }
    };

    let reg = pkg_registry::registry_dir()?;
    let deps_dir = dir.join(".mimi").join("deps");
    std::fs::create_dir_all(&deps_dir)
        .map_err(|e| format!("failed to create deps dir: {}", e))?;

    let mut lock = lockfile::Lockfile::load(&dir)?
        .unwrap_or_else(lockfile::Lockfile::new);
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<manifest::Dependency> = direct_deps;
    let mut updated_count = 0;

    while let Some(dep) = queue.pop() {
        if !visited.insert(dep.name.clone()) {
            continue;
        }

        let dst = deps_dir.join(&dep.name);

        // Skip if checksum matches (no change needed)
        if let Some(old_entry) = lock.get_package(&dep.name) {
            if pkg_resolve::checksum_matches(&dst, old_entry.checksum.as_deref()) {
                println!("  = {} ({})", dep.name, old_entry.version);
                updated_count += 1;
                let sub_deps = pkg_resolve::read_transitive_deps(&dst, &visited);
                for sub_dep in sub_deps {
                    println!("    → {} (dependency of {})", sub_dep.name, dep.name);
                    queue.push(sub_dep);
                }
                continue;
            }
        }

        let old_version = lock.get_package(&dep.name).map(|p| p.version.clone());
        let resolved = pkg_resolve::resolve_single_dep(&dep, &dst, &reg)?;
        lock.add_package(&resolved.name, &resolved.version, resolved.source.as_deref(), resolved.checksum.as_deref());

        match old_version {
            Some(v) if v != resolved.version => println!("  ↑ {} ({} → {})", dep.name, v, resolved.version),
            Some(v) => println!("  = {} ({})", dep.name, v),
            None => println!("  ✓ {} (v{})", dep.name, resolved.version),
        }
        updated_count += 1;

        let sub_deps = pkg_resolve::read_transitive_deps(&dst, &visited);
        for sub_dep in sub_deps {
            println!("    → {} (dependency of {})", sub_dep.name, dep.name);
            queue.push(sub_dep);
        }
    }

    lock.save(&dir)?;
    println!("Updated {} package(s).", updated_count);
    Ok(())
}
