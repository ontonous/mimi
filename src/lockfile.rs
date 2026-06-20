use serde::{Deserialize, Serialize};
use std::path::Path;

/// Lock file entry for a resolved dependency
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LockEntry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub checksum: Option<String>,
}

/// mimi.lock file structure
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Lockfile {
    pub package: Vec<LockEntry>,
}

impl Lockfile {
    /// Load mimi.lock from a directory
    pub fn load(dir: &Path) -> Result<Option<Self>, String> {
        let lock_path = dir.join("mimi.lock");
        if !lock_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&lock_path)
            .map_err(|e| format!("failed to read {}: {}", lock_path.display(), e))?;
        let lockfile: Self = toml::from_str(&content)
            .map_err(|e| format!("failed to parse {}: {}", lock_path.display(), e))?;
        Ok(Some(lockfile))
    }

    /// Save mimi.lock to a directory
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let lock_path = dir.join("mimi.lock");
        let content = toml::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize lockfile: {}", e))?;
        std::fs::write(&lock_path, content)
            .map_err(|e| format!("failed to write {}: {}", lock_path.display(), e))?;
        Ok(())
    }

    /// Create a new empty lockfile
    pub fn new() -> Self {
        Lockfile { package: Vec::new() }
    }

    /// Add or update a package entry
    pub fn add_package(&mut self, name: &str, version: &str, source: Option<&str>, checksum: Option<&str>) {
        self.package.retain(|p| p.name != name);
        self.package.push(LockEntry {
            name: name.to_string(),
            version: version.to_string(),
            source: source.map(|s| s.to_string()),
            checksum: checksum.map(|c| c.to_string()),
        });
    }

    /// Remove a package entry
    pub fn remove_package(&mut self, name: &str) -> bool {
        let len_before = self.package.len();
        self.package.retain(|p| p.name != name);
        self.package.len() < len_before
    }

    /// Get a package entry by name
    pub fn get_package(&self, name: &str) -> Option<&LockEntry> {
        self.package.iter().find(|p| p.name == name)
    }

    /// Resolve version constraint against available versions
    pub fn resolve_version(constraint: &str, available: &[&str]) -> Option<String> {
        if constraint == "*" || constraint.is_empty() {
            return available.last().map(|s| s.to_string());
        }

        // Try to parse as semver constraint
        if let Ok(req) = semver::VersionReq::parse(constraint) {
            let mut best: Option<semver::Version> = None;
            for ver_str in available {
                if let Ok(ver) = semver::Version::parse(ver_str) {
                    if req.matches(&ver) {
                        match &best {
                            Some(current) if ver > *current => best = Some(ver),
                            None => best = Some(ver),
                            _ => {}
                        }
                    }
                }
            }
            return best.map(|v| v.to_string());
        }

        // Fallback: exact match
        available.iter()
            .find(|&&v| v == constraint)
            .map(|s| s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfile_new() {
        let lf = Lockfile::new();
        assert!(lf.package.is_empty());
    }

    #[test]
    fn lockfile_add_remove() {
        let mut lf = Lockfile::new();
        lf.add_package("foo", "1.0.0", Some("git+https://example.com"), None);
        assert_eq!(lf.package.len(), 1);
        assert!(lf.get_package("foo").is_some());

        lf.add_package("foo", "2.0.0", None, None);
        assert_eq!(lf.package.len(), 1);
        assert_eq!(lf.get_package("foo").unwrap().version, "2.0.0");

        assert!(lf.remove_package("foo"));
        assert!(!lf.remove_package("foo"));
    }

    #[test]
    fn resolve_version_exact() {
        let available = ["0.1.0", "0.2.0", "1.0.0"];
        assert_eq!(Lockfile::resolve_version("=1.0.0", &available), Some("1.0.0".into()));
    }

    #[test]
    fn resolve_version_exact_fallback() {
        let available = ["0.1.0", "0.2.0", "1.0.0"];
        // Bare "1.0.0" is not a valid semver requirement, so it falls through to exact match
        assert_eq!(Lockfile::resolve_version("1.0.0", &available), Some("1.0.0".into()));
        // Should return None if no exact match
        assert_eq!(Lockfile::resolve_version("9.9.9", &available), None);
    }

    #[test]
    fn resolve_version_caret() {
        let available = ["0.1.0", "0.2.0", "1.0.0", "1.1.0", "2.0.0"];
        assert_eq!(Lockfile::resolve_version("^1.0", &available), Some("1.1.0".into()));
    }

    #[test]
    fn resolve_version_wildcard() {
        let available = ["0.1.0", "0.2.0", "1.0.0"];
        assert_eq!(Lockfile::resolve_version("*", &available), Some("1.0.0".into()));
    }

    #[test]
    fn resolve_version_range() {
        let available = ["0.1.0", "0.5.0", "1.0.0", "1.5.0", "2.0.0"];
        assert_eq!(Lockfile::resolve_version(">=0.5, <2.0", &available), Some("1.5.0".into()));
    }
}
