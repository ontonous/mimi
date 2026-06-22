use mimi::manifest;

pub(crate) fn init(name: Option<&str>) -> Result<(), String> {
    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let toml_path = dir.join("mimi.toml");
    if toml_path.exists() {
        return Err("mimi.toml already exists".into());
    }
    let pkg_name = name.unwrap_or("my-package");
    let manifest = manifest::Manifest::new(pkg_name);
    manifest.save(&dir)?;
    println!("✓ Created mimi.toml for package '{}'", pkg_name);

    // Create main.mimi if it doesn't exist
    let entry_path = manifest.entry_path(&dir);
    if !entry_path.exists() {
        std::fs::write(&entry_path, "func main() -> i32 {\n    42\n}\n")
            .map_err(|e| format!("failed to create {}: {}", entry_path.display(), e))?;
        println!("✓ Created {}", entry_path.display());
    }
    Ok(())
}
