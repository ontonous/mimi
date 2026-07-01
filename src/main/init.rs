use mimi::manifest;
use std::path::Path;

pub(crate) fn init(base_dir: &Path, name: Option<&str>) -> Result<(), String> {
    let project_dir = match name {
        Some(n) => base_dir.join(n),
        None => base_dir.to_path_buf(),
    };

    if name.is_some() && project_dir.exists() {
        return Err(format!(
            "directory '{}' already exists",
            project_dir.display()
        ));
    }

    std::fs::create_dir_all(&project_dir)
        .map_err(|e| format!("failed to create project directory: {}", e))?;

    let toml_path = project_dir.join("mimi.toml");
    if toml_path.exists() {
        return Err("mimi.toml already exists".into());
    }

    let pkg_name = name.unwrap_or("my-package");
    let manifest = manifest::Manifest::new(pkg_name);
    manifest.save(&project_dir)?;
    println!("✓ Created mimi.toml for package '{}'", pkg_name);

    // Create main.mimi if it doesn't exist
    let entry_path = manifest.entry_path(&project_dir);
    if !entry_path.exists() {
        std::fs::write(&entry_path, "func main() -> i32 {\n    42\n}\n")
            .map_err(|e| format!("failed to create {}: {}", entry_path.display(), e))?;
        println!("✓ Created {}", entry_path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("mimi_init_test_{}_{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn init_creates_project_in_named_subdirectory() {
        let base = temp_dir();
        init(&base, Some("myapp")).expect("init should succeed");

        let project_dir = base.join("myapp");
        assert!(
            project_dir.exists(),
            "project subdirectory should be created"
        );
        assert!(
            project_dir.join("mimi.toml").exists(),
            "mimi.toml should exist"
        );
        assert!(
            project_dir.join("main.mimi").exists(),
            "main.mimi should exist"
        );

        // Cleanup
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn init_without_name_uses_current_directory() {
        let base = temp_dir();
        init(&base, None).expect("init should succeed");

        assert!(
            base.join("mimi.toml").exists(),
            "mimi.toml should exist in base dir"
        );
        assert!(
            base.join("main.mimi").exists(),
            "main.mimi should exist in base dir"
        );

        // Cleanup
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn init_refuses_existing_subdirectory() {
        let base = temp_dir();
        let existing = base.join("exists");
        std::fs::create_dir(&existing).unwrap();

        let result = init(&base, Some("exists"));
        assert!(
            result.is_err(),
            "init should fail when target subdirectory exists"
        );

        // Cleanup
        std::fs::remove_dir_all(&base).ok();
    }
}
