use crate::manifest;

pub(crate) fn add(name: &str, version: Option<&str>, path: Option<&str>) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, mut manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found; run 'mimi init' first".into()),
    };
    manifest.add_dependency(name, version, path);
    manifest.save(&dir)?;
    println!("✓ Added dependency '{}'", name);
    Ok(())
}
