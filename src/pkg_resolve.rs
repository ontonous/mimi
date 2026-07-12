use crate::{lockfile, manifest, pkg_registry};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct ResolvedDep {
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    pub checksum: Option<String>,
}

/// Resolve a single dependency: fetch from git/registry/path and install to dst.
pub fn resolve_single_dep(
    dep: &manifest::Dependency,
    dst: &Path,
    reg: &Path,
) -> Result<ResolvedDep, String> {
    if let Some(git_url) = &dep.git {
        resolve_git_dep(dep, git_url, dst)
    } else {
        let source = dep.path.as_deref().unwrap_or("registry");
        if source == "registry" {
            resolve_registry_dep(dep, dst, reg)
        } else {
            resolve_path_dep(dep, dst, source)
        }
    }
}

fn resolve_git_dep(
    dep: &manifest::Dependency,
    git_url: &str,
    dst: &Path,
) -> Result<ResolvedDep, String> {
    let tag_arg = dep.tag.as_deref().unwrap_or("main");

    // SEC-C2 (deep audit): validate git_url to prevent command injection.
    // - Reject URLs starting with '-' (git option injection)
    // - Reject ext:: protocol (arbitrary command execution)
    // - Only allow standard git URL prefixes
    fn validate_git_url(url: &str) -> Result<(), String> {
        if url.starts_with('-') {
            return Err(format!("invalid git URL: starts with '-' (possible option injection): {}", url));
        }
        if url.starts_with("ext::") {
            return Err(format!("invalid git URL: ext:: protocol is forbidden (RCE risk): {}", url));
        }
        let safe = url.starts_with("https://")
            || url.starts_with("http://")
            || url.starts_with("ssh://")
            || url.starts_with("git@")
            || url.starts_with("file://")
            || url.starts_with("git://");
        if !safe {
            return Err(format!("invalid git URL: must start with https://, http://, ssh://, git@, git://, or file://: {}", url));
        }
        Ok(())
    }
    validate_git_url(git_url)?;

    // Validate tag to prevent option injection
    if tag_arg.starts_with('-') {
        return Err(format!("invalid git tag: starts with '-': {}", tag_arg));
    }

    if dst.exists() {
        std::fs::remove_dir_all(dst)
            .map_err(|e| format!("failed to remove old {}: {}", dep.name, e))?;
    }

    let status = std::process::Command::new("git")
        .arg("-c")
        .arg("protocol.ext.allow=never")
        .arg("clone")
        .arg("--branch")
        .arg(tag_arg)
        .arg("--depth")
        .arg("1")
        .arg("--")
        .arg(git_url)
        .arg(dst)
        .status()
        .map_err(|e| format!("git clone failed for {}: {}", dep.name, e))?;
    if !status.success() {
        return Err(format!(
            "git clone failed for {} (url: {}, tag: {})",
            dep.name, git_url, tag_arg
        ));
    }

    let resolved_version = if let Ok(output) = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .current_dir(dst)
        .output()
    {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        tag_arg.to_string()
    };

    let checksum = pkg_registry::compute_dir_checksum(dst).ok();
    Ok(ResolvedDep {
        name: dep.name.clone(),
        version: resolved_version,
        source: Some(format!("git+{}", git_url)),
        checksum,
    })
}

fn resolve_registry_dep(
    dep: &manifest::Dependency,
    dst: &Path,
    reg: &Path,
) -> Result<ResolvedDep, String> {
    let pkg_dir = reg.join(&dep.name);
    if !pkg_dir.exists() {
        return Err(format!(
            "package '{}' not found in local registry (use 'mimi publish' first)",
            dep.name
        ));
    }

    let version = dep.version.as_deref().unwrap_or("*");
    let versions: Vec<String> = std::fs::read_dir(&pkg_dir)
        .map_err(|e| format!("failed to read registry: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();
    let version_refs: Vec<&str> = versions.iter().map(|s| s.as_str()).collect();
    let resolved = lockfile::Lockfile::resolve_version(version, &version_refs)
        .ok_or_else(|| format!("no matching version for '{}' {}", dep.name, version))?;

    let src = pkg_dir.join(&resolved);
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| format!("failed to remove old: {}", e))?;
    }
    pkg_registry::copy_dir_recursive(&src, dst)
        .map_err(|e| format!("failed to copy {}: {}", dep.name, e))?;

    let checksum = pkg_registry::compute_dir_checksum(dst).ok();
    Ok(ResolvedDep {
        name: dep.name.clone(),
        version: resolved,
        source: Some("registry".to_string()),
        checksum,
    })
}

fn resolve_path_dep(
    dep: &manifest::Dependency,
    dst: &Path,
    source: &str,
) -> Result<ResolvedDep, String> {
    // SEC-C7 (deep audit): validate path dependency to prevent arbitrary file exfiltration.
    // Only allow relative paths without '..' that escape the project root.
    let src = PathBuf::from(source);
    if src.is_absolute() {
        return Err(format!(
            "path dependency '{}' must be a relative path, got absolute: {}",
            dep.name, source
        ));
    }
    if source.contains("..") {
        return Err(format!(
            "path dependency '{}' must not contain '..': {}",
            dep.name, source
        ));
    }
    if !src.exists() {
        return Err(format!(
            "path dependency '{}' not found at {}",
            dep.name, source
        ));
    }
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| format!("failed to remove old: {}", e))?;
    }
    pkg_registry::copy_dir_recursive(&src, dst)
        .map_err(|e| format!("failed to copy {}: {}", dep.name, e))?;

    let checksum = pkg_registry::compute_dir_checksum(dst).ok();
    Ok(ResolvedDep {
        name: dep.name.clone(),
        version: "*".to_string(),
        source: Some(format!("path:{}", source)),
        checksum,
    })
}

/// Read transitive dependencies from an installed package's mimi.toml.
pub fn read_transitive_deps(dst: &Path, visited: &HashSet<String>) -> Vec<manifest::Dependency> {
    let dep_manifest_path = dst.join("mimi.toml");
    if !dep_manifest_path.exists() {
        return Vec::new();
    }
    if let Ok(Some((_sub_dir, sub_manifest))) = manifest::Manifest::find(dst) {
        if let Some(sub_deps) = &sub_manifest.dependencies {
            return sub_deps
                .iter()
                .filter(|d| !visited.contains(&d.name))
                .cloned()
                .collect();
        }
    }
    Vec::new()
}

/// Check if an already-installed dep's checksum matches the lockfile.
/// Returns `true` if the dep directory exists and its checksum matches.
pub fn checksum_matches(dst: &Path, expected: Option<&str>) -> bool {
    match expected {
        Some(cs) if dst.exists() => {
            pkg_registry::compute_dir_checksum(dst).ok().as_deref() == Some(cs)
        }
        _ => false,
    }
}
