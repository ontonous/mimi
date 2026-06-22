use mimi::manifest;

pub(crate) fn tree() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (_dir, manifest) = match manifest::Manifest::find(&cwd)? {
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

    if let Some(deps) = &manifest.dependencies {
        for (i, dep) in deps.iter().enumerate() {
            let is_last = i == deps.len() - 1;
            let prefix = if is_last { "└── " } else { "├── " };
            let version = dep.version.as_deref().unwrap_or("*");
            let source = if let Some(path) = &dep.path {
                format!("(path: {})", path)
            } else if let Some(git) = &dep.git {
                format!("(git: {})", git)
            } else {
                "(registry)".to_string()
            };
            println!("{}{} {} {}", prefix, dep.name, version, source);
        }
    }
    Ok(())
}
