use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// mimi.toml package configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Manifest {
    pub package: Option<Package>,
    pub dependencies: Option<Vec<Dependency>>,
    pub registry: Option<Registry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Package {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub entry: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Dependency {
    pub name: String,
    pub version: Option<String>,
    pub path: Option<String>,
    pub git: Option<String>,
    pub tag: Option<String>,
}

/// Registry configuration for remote package downloads
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Registry {
    pub url: String,
}

impl Manifest {
    /// Load mimi.toml from a directory
    pub fn load(dir: &Path) -> Result<Option<Self>, String> {
        let toml_path = dir.join("mimi.toml");
        if !toml_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&toml_path)
            .map_err(|e| format!("failed to read {}: {}", toml_path.display(), e))?;
        let manifest: Self = toml::from_str(&content)
            .map_err(|e| format!("failed to parse {}: {}", toml_path.display(), e))?;
        Ok(Some(manifest))
    }

    /// Find mimi.toml by searching up from the given path
    pub fn find(start: &Path) -> Result<Option<(PathBuf, Self)>, String> {
        let mut dir = start.to_path_buf();
        if dir.is_file() {
            dir = dir.parent().unwrap_or(&dir).to_path_buf();
        }
        let max_depth = 64;
        for _ in 0..max_depth {
            // Check permission first to avoid false errors on inaccessible directories
            let toml_path = dir.join("mimi.toml");
            match std::fs::metadata(&toml_path) {
                Ok(_) => {
                    match Self::load(&dir) {
                        Ok(Some(manifest)) => return Ok(Some((dir, manifest))),
                        Ok(None) => {}
                        Err(e) => return Err(e),
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    // Permission error: skip this directory and continue upward
                }
                Err(_) => {
                    // File not found or other non-permission error: continue
                }
            }
            if !dir.pop() {
                return Ok(None);
            }
        }
        Err("max search depth exceeded while looking for mimi.toml".into())
    }

    /// Get the entry point file path
    pub fn entry_path(&self, base_dir: &Path) -> PathBuf {
        let entry = self.package.as_ref()
            .and_then(|p| p.entry.as_deref())
            .unwrap_or("main.mimi");
        base_dir.join(entry)
    }

    /// Add a dependency
    pub fn add_dependency(&mut self, name: &str, version: Option<&str>, path: Option<&str>) {
        let deps = self.dependencies.get_or_insert_with(Vec::new);
        // Remove existing dependency with same name
        deps.retain(|d| d.name != name);
        deps.push(Dependency {
            name: name.to_string(),
            version: version.map(|v| v.to_string()),
            path: path.map(|p| p.to_string()),
            git: None,
            tag: None,
        });
    }

    /// Remove a dependency
    pub fn remove_dependency(&mut self, name: &str) -> bool {
        if let Some(deps) = &mut self.dependencies {
            let len_before = deps.len();
            deps.retain(|d| d.name != name);
            deps.len() < len_before
        } else {
            false
        }
    }

    /// Save mimi.toml to a directory
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let toml_path = dir.join("mimi.toml");
        let content = toml::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize manifest: {}", e))?;
        std::fs::write(&toml_path, content)
            .map_err(|e| format!("failed to write {}: {}", toml_path.display(), e))?;
        Ok(())
    }

    /// Create a new empty manifest
    pub fn new(name: &str) -> Self {
        Manifest {
            package: Some(Package {
                name: name.to_string(),
                version: Some("0.1.0".to_string()),
                description: None,
                entry: Some("main.mimi".to_string()),
            }),
            dependencies: None,
            registry: None,
        }
    }

    /// Get the default registry URL
    pub fn registry_url(&self) -> &str {
        self.registry.as_ref()
            .map(|r| r.url.as_str())
            .unwrap_or("https://registry.mimi-lang.org")
    }

    /// Check for dependency conflicts: two deps requiring different versions of the same package
    pub fn check_conflicts(&self) -> Vec<String> {
        let mut conflicts = Vec::new();
        if let Some(deps) = &self.dependencies {
            let mut seen: std::collections::HashMap<String, Vec<&str>> = std::collections::HashMap::new();
            for dep in deps {
                let ver = dep.version.as_deref().unwrap_or("*");
                seen.entry(dep.name.clone()).or_default().push(ver);
            }
            for (name, versions) in &seen {
                if versions.len() > 1 {
                    conflicts.push(format!(
                        "dependency '{}' has conflicting version requirements: {:?}",
                        name, versions
                    ));
                }
            }
        }
        conflicts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_conflict_detection() {
        let mut manifest = Manifest::new("test");
        // Manually add duplicate deps to simulate conflict
        manifest.dependencies = Some(vec![
            Dependency { name: "foo".into(), version: Some("^1.0".into()), path: None, git: None, tag: None },
            Dependency { name: "foo".into(), version: Some("^2.0".into()), path: None, git: None, tag: None },
        ]);
        let conflicts = manifest.check_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("foo"));
    }

    #[test]
    fn manifest_no_conflicts() {
        let mut manifest = Manifest::new("test");
        manifest.add_dependency("foo", Some("^1.0"), None);
        manifest.add_dependency("bar", Some("^2.0"), None);
        let conflicts = manifest.check_conflicts();
        assert!(conflicts.is_empty());
    }

    #[test]
    fn manifest_registry_url() {
        let manifest = Manifest::new("test");
        assert_eq!(manifest.registry_url(), "https://registry.mimi-lang.org");

        let mut manifest = Manifest::new("test");
        manifest.registry = Some(Registry { url: "https://custom.registry.com".into() });
        assert_eq!(manifest.registry_url(), "https://custom.registry.com");
    }
}
