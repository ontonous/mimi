use crate::install::copy_dir_recursive;
use mimi::manifest;
use crate::search::registry_dir;

pub(crate) fn publish(name: Option<&str>, version: Option<&str>) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (_dir, manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found; run 'mimi init' first".into()),
    };

    let pkg = manifest.package.as_ref()
        .ok_or("no [package] in mimi.toml")?;
    let pkg_name = name.unwrap_or(&pkg.name);
    let pkg_version = version
        .or(pkg.version.as_deref())
        .unwrap_or("0.1.0");

    let reg = registry_dir()?;
    let pkg_dir = reg.join(pkg_name).join(pkg_version);

    if pkg_dir.exists() {
        return Err(format!("package {} v{} already exists in registry", pkg_name, pkg_version));
    }

    copy_dir_recursive(&cwd, &pkg_dir)
        .map_err(|e| format!("failed to publish: {}", e))?;

    println!("✓ Published {} v{} to local registry", pkg_name, pkg_version);
    println!("  Location: {}", pkg_dir.display());
    Ok(())
}
