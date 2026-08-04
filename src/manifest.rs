use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Atomic text-file write: stage the content in a unique temp file in the
/// same directory, then `rename` it over the target.
///
/// Full audit 2026-08-05 §13: fixed temp names (`mimi.toml.tmp` /
/// `mimi.lock.tmp`) race between concurrent installs, and direct
/// `fs::write` can leave a user file truncated/corrupted on a crash or
/// concurrent access mid-write. A rename within the same directory is
/// atomic on POSIX, so readers observe either the old or the new content,
/// never a partial write.
///
/// Temp-name pattern follows `pkg_resolve::install_dir_atomic` (per-pid),
/// extended with a per-process atomic counter so same-process writers
/// cannot collide either.
pub fn write_text_atomic(path: &Path, content: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    let tmp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        file_name,
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    std::fs::write(&tmp_path, content)
        .map_err(|e| format!("failed to write {}: {}", tmp_path.display(), e))?;
    std::fs::rename(&tmp_path, path).map_err(|e| {
        // Best-effort cleanup of the staged temp file; the rename error is propagated.
        let _ = std::fs::remove_file(&tmp_path);
        format!(
            "failed to rename {} to {}: {}",
            tmp_path.display(),
            path.display(),
            e
        )
    })
}

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
        let content = crate::path_safety::read_source_capped(&toml_path)?;
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
                Ok(_) => match Self::load(&dir) {
                    Ok(Some(manifest)) => return Ok(Some((dir, manifest))),
                    Ok(None) => {}
                    Err(e) => return Err(e),
                },
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

    /// Get the entry point file path.
    ///
    /// AU-H7: invalid entry paths fall back to `main.mimi` but emit a stderr
    /// warning so silent path errors are visible (LSP/CLI can also re-validate).
    pub fn entry_path(&self, base_dir: &Path) -> PathBuf {
        let entry = self
            .package
            .as_ref()
            .and_then(|p| p.entry.as_deref())
            .unwrap_or("main.mimi");
        // B1: use unified path safety validation.
        if crate::path_safety::validate_safe_path(base_dir, entry).is_err() {
            eprintln!(
                "[mimi] WARN: package entry '{}' is unsafe (path traversal); falling back to main.mimi",
                entry
            );
            return base_dir.join("main.mimi");
        }
        base_dir.join(entry)
    }

    /// Add a dependency
    pub fn add_dependency(
        &mut self,
        name: &str,
        version: Option<&str>,
        path: Option<&str>,
        git: Option<&str>,
        tag: Option<&str>,
    ) {
        let deps = self.dependencies.get_or_insert_with(Vec::new);
        // Remove existing dependency with same name
        deps.retain(|d| d.name != name);
        deps.push(Dependency {
            name: name.to_string(),
            version: version.map(|v| v.to_string()),
            path: path.map(|p| p.to_string()),
            git: git.map(|g| g.to_string()),
            tag: tag.map(|t| t.to_string()),
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

    /// Save mimi.toml to a directory (atomic write via temp+rename)
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let toml_path = dir.join("mimi.toml");
        let content = toml::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize manifest: {}", e))?;
        // M42 (deep audit) + full audit 2026-08-05 §13: atomic write with a
        // per-pid unique temp name (the fixed `mimi.toml.tmp` raced between
        // concurrent installs).
        write_text_atomic(&toml_path, &content)
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
        self.registry
            .as_ref()
            .map(|r| r.url.as_str())
            .unwrap_or("https://registry.mimi-lang.org")
    }

    /// Check for dependency conflicts: two deps requiring different versions of the same package
    pub fn check_conflicts(&self) -> Vec<String> {
        let mut conflicts = Vec::new();
        if let Some(deps) = &self.dependencies {
            let mut seen: std::collections::HashMap<String, Vec<&str>> =
                std::collections::HashMap::new();
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
            Dependency {
                name: "foo".into(),
                version: Some("^1.0".into()),
                path: None,
                git: None,
                tag: None,
            },
            Dependency {
                name: "foo".into(),
                version: Some("^2.0".into()),
                path: None,
                git: None,
                tag: None,
            },
        ]);
        let conflicts = manifest.check_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("foo"));
    }

    #[test]
    fn manifest_no_conflicts() {
        let mut manifest = Manifest::new("test");
        manifest.add_dependency("foo", Some("^1.0"), None, None, None);
        manifest.add_dependency("bar", Some("^2.0"), None, None, None);
        let conflicts = manifest.check_conflicts();
        assert!(conflicts.is_empty());
    }

    #[test]
    fn manifest_registry_url() {
        let manifest = Manifest::new("test");
        assert_eq!(manifest.registry_url(), "https://registry.mimi-lang.org");

        let mut manifest = Manifest::new("test");
        manifest.registry = Some(Registry {
            url: "https://custom.registry.com".into(),
        });
        assert_eq!(manifest.registry_url(), "https://custom.registry.com");
    }
}
