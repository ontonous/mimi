use mimi::manifest;
use mimi::path_safety;
use std::path::Path;

pub(crate) fn init(base_dir: &Path, name: Option<&str>) -> Result<(), String> {
    // P1-12: always initialize in the current directory, regardless of
    // whether a package name is given. The name only sets the `name`
    // field in mimi.toml, making the behavior consistent with `cargo init`.
    let project_dir = base_dir.to_path_buf();

    if !project_dir.exists() {
        return Err(format!(
            "directory '{}' does not exist",
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
    // B1: validate package name to prevent path traversal.
    path_safety::validate_package_name(pkg_name)?;
    let manifest = manifest::Manifest::new(pkg_name);
    manifest.save(&project_dir)?;
    println!("✓ Created mimi.toml for package '{}'", pkg_name);

    // Create main.mimi if it doesn't exist
    let entry_path = manifest.entry_path(&project_dir);
    if !entry_path.exists() {
        std::fs::write(&entry_path, "func main() {\n    println(\"Hello, Mimi!\");\n}\n")
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
    fn init_creates_project_in_current_directory() {
        let base = temp_dir();
        init(&base, Some("myapp")).expect("init should succeed");

        assert!(
            base.join("mimi.toml").exists(),
            "mimi.toml should exist in base dir"
        );
        assert!(
            base.join("main.mimi").exists(),
            "main.mimi should exist in base dir"
        );
        let content = std::fs::read_to_string(base.join("mimi.toml")).expect("read toml");
        assert!(
            content.contains(r#"name = "myapp""#),
            "toml should contain given package name"
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
    fn init_with_given_name_writes_correct_toml() {
        let base = temp_dir();
        init(&base, Some("myapp")).expect("init should succeed");
        let content = std::fs::read_to_string(base.join("mimi.toml")).expect("read toml");
        assert!(
            content.contains(r#"name = "myapp""#),
            "toml should use the given package name"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
