use crate::manifest;

pub(crate) fn remove(name: &str) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, mut manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found".into()),
    };
    if manifest.remove_dependency(name) {
        manifest.save(&dir)?;
        println!("✓ Removed dependency '{}'", name);
    } else {
        println!("Dependency '{}' not found", name);
    }
    Ok(())
}
