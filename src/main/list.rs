use mimi::manifest;

pub(crate) fn list() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (_dir, manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found".into()),
    };
    if let Some(deps) = &manifest.dependencies {
        if deps.is_empty() {
            println!("No dependencies.");
        } else {
            println!("Dependencies:");
            for dep in deps {
                let version = dep.version.as_deref().unwrap_or("*");
                let source = dep.path.as_deref().unwrap_or("registry");
                println!("  {} {} ({})", dep.name, version, source);
            }
        }
    } else {
        println!("No dependencies.");
    }
    Ok(())
}
