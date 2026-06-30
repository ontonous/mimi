use mimi::{lockfile, manifest, pkg_registry, pkg_resolve};
use std::collections::HashSet;

/// Install all dependencies declared in `mimi.toml`.
///
/// `frozen` — if true, refuse to update the lockfile (CI mode).
/// `offline` — if true, only use cached `.mimi/deps`; skip network/git/registry fetches.
pub(crate) fn install(frozen: bool, offline: bool) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found; run 'mimi init' first".into()),
    };

    let conflicts = manifest.check_conflicts();
    for c in &conflicts {
        eprintln!("warning: {}", c);
    }

    let direct_deps = match &manifest.dependencies {
        Some(d) if !d.is_empty() => d.clone(),
        _ => {
            println!("No dependencies to install.");
            return Ok(());
        }
    };

    let reg = pkg_registry::registry_dir()?;
    let deps_dir = dir.join(".mimi").join("deps");
    std::fs::create_dir_all(&deps_dir).map_err(|e| format!("failed to create deps dir: {}", e))?;

    let mut lock = lockfile::Lockfile::load(&dir)?.unwrap_or_else(lockfile::Lockfile::new);
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<manifest::Dependency> = direct_deps;
    let mut installed = 0;
    let mut skipped = 0;

    while let Some(dep) = queue.pop() {
        if !visited.insert(dep.name.clone()) {
            continue;
        }

        let dst = deps_dir.join(&dep.name);

        // Idempotency: if lockfile already has this dep and the on-disk
        // checksum matches, skip re-resolution.
        if let Some(entry) = lock.get_package(&dep.name).cloned() {
            if pkg_resolve::checksum_matches(&dst, entry.checksum.as_deref()) {
                println!("  = {} ({})", dep.name, entry.version);
                skipped += 1;
                let sub_deps = pkg_resolve::read_transitive_deps(&dst, &visited);
                for sub_dep in sub_deps {
                    queue.push(sub_dep);
                }
                continue;
            }
        }

        // Offline: only allow already-cached deps. Otherwise error out.
        if offline {
            if !dst.exists() {
                return Err(format!(
                    "offline: '{}' not in cache; run 'mimi install' once with network",
                    dep.name
                ));
            }
            // Use the version that was previously resolved (if known)
            if let Some(entry) = lock.get_package(&dep.name) {
                println!("  = {} ({}, cached)", dep.name, entry.version);
                skipped += 1;
                let sub_deps = pkg_resolve::read_transitive_deps(&dst, &visited);
                for sub_dep in sub_deps {
                    queue.push(sub_dep);
                }
                continue;
            }
            return Err(format!(
                "offline: '{}' not in lockfile and not in cache",
                dep.name
            ));
        }

        // Frozen: do not fetch, do not update lockfile. Refuse if missing.
        if frozen {
            if let Some(entry) = lock.get_package(&dep.name) {
                if dst.exists() {
                    println!("  = {} ({}, frozen)", dep.name, entry.version);
                    skipped += 1;
                    let sub_deps = pkg_resolve::read_transitive_deps(&dst, &visited);
                    for sub_dep in sub_deps {
                        queue.push(sub_dep);
                    }
                    continue;
                }
            }
            return Err(format!(
                "frozen: '{}' missing from cache; cannot update",
                dep.name
            ));
        }

        let resolved = pkg_resolve::resolve_single_dep(&dep, &dst, &reg)?;
        println!("  ✓ {} (v{})", resolved.name, resolved.version);
        lock.add_package(
            &resolved.name,
            &resolved.version,
            resolved.source.as_deref(),
            resolved.checksum.as_deref(),
        );
        installed += 1;

        let sub_deps = pkg_resolve::read_transitive_deps(&dst, &visited);
        for sub_dep in sub_deps {
            println!("    → {} (dependency of {})", sub_dep.name, dep.name);
            queue.push(sub_dep);
        }
    }

    lock.save(&dir)?;
    if installed == 0 {
        println!("All {} package(s) up to date.", skipped);
    } else {
        println!("Installed {} package(s) ({} cached).", installed, skipped);
    }
    Ok(())
}
