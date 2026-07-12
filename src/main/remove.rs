use mimi::lockfile;
use mimi::manifest;
use mimi::path_safety;

/// `mimi remove <name>` — drop a dependency from mimi.toml, mimi.lock,
/// and `.mimi/deps/<name>`.
pub(crate) fn remove(name: &str) -> Result<(), String> {
    // B1: use unified path safety validation.
    path_safety::validate_package_name(name)?;
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, mut manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found".into()),
    };

    let mut removed = false;

    if manifest.remove_dependency(name) {
        manifest.save(&dir)?;
        println!("✓ Removed dependency '{}' from mimi.toml", name);
        removed = true;
    }

    // Always clean the lockfile entry, even if the dep was not in the manifest
    // (e.g. a transitive dep the user wants gone). This is idempotent.
    if let Ok(Some(mut lock)) = lockfile::Lockfile::load(&dir) {
        if lock.remove_package(name) {
            lock.save(&dir)?;
            println!("✓ Removed '{}' from mimi.lock", name);
            removed = true;
        }
    }

    // Clean the on-disk cache.
    let dst = dir.join(".mimi").join("deps").join(name);
    if dst.exists() {
        std::fs::remove_dir_all(&dst).map_err(|e| format!("failed to remove cached dep: {}", e))?;
        println!("✓ Removed cached directory {}", dst.display());
        removed = true;
    }

    if !removed {
        println!("Dependency '{}' not found", name);
    } else {
        println!("Done.");
    }
    Ok(())
}
