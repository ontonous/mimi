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
    resolve_single_dep_in(dep, dst, reg, None)
}

/// Resolve a dependency with an optional base directory for relative path deps.
/// When `base_dir` is set (manifest directory), path deps resolve relative to it
/// rather than the process cwd (P-H10).
pub fn resolve_single_dep_in(
    dep: &manifest::Dependency,
    dst: &Path,
    reg: &Path,
    base_dir: Option<&Path>,
) -> Result<ResolvedDep, String> {
    if let Some(git_url) = &dep.git {
        resolve_git_dep(dep, git_url, dst)
    } else {
        let source = dep.path.as_deref().unwrap_or("registry");
        if source == "registry" {
            resolve_registry_dep(dep, dst, reg)
        } else {
            resolve_path_dep(dep, dst, source, base_dir)
        }
    }
}

fn resolve_git_dep(
    dep: &manifest::Dependency,
    git_url: &str,
    dst: &Path,
) -> Result<ResolvedDep, String> {
    let tag_arg = dep.tag.as_deref().unwrap_or("main");

    // B1: use unified path safety validation.
    crate::path_safety::validate_git_url(git_url)?;

    // Validate tag to prevent option injection
    if tag_arg.starts_with('-') {
        return Err(format!("invalid git tag: starts with '-': {}", tag_arg));
    }

    // P-H5: clone into a temp dir first so failures leave the old install intact.
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(".{}.git-tmp-{}", dep.name, std::process::id()));
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(&tmp);
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
        .arg(&tmp)
        .status()
        .map_err(|e| format!("git clone failed for {}: {}", dep.name, e))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "git clone failed for {} (url: {}, tag: {})",
            dep.name, git_url, tag_arg
        ));
    }

    let resolved_version = if let Ok(output) = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .current_dir(&tmp)
        .output()
    {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        tag_arg.to_string()
    };

    install_dir_atomic(&tmp, dst).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        format!("failed to install git dep {}: {}", dep.name, e)
    })?;
    let _ = std::fs::remove_dir_all(&tmp);

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
    // AU-C3: reject path-traversal package names before joining into the registry.
    crate::path_safety::validate_package_name(&dep.name)
        .map_err(|e| format!("invalid package name '{}': {}", dep.name, e))?;
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
    install_dir_atomic(&src, dst).map_err(|e| format!("failed to install {}: {}", dep.name, e))?;

    let checksum = pkg_registry::compute_dir_checksum(dst).ok();
    Ok(ResolvedDep {
        name: dep.name.clone(),
        version: resolved,
        source: Some("registry".to_string()),
        checksum,
    })
}

/// P-H5: copy into a temporary sibling directory, then swap into place so a
/// failed install never deletes a previously working cache entry first.
fn install_dir_atomic(src: &Path, dst: &Path) -> Result<(), String> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        dst.file_name().and_then(|s| s.to_str()).unwrap_or("dep"),
        std::process::id()
    ));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    pkg_registry::copy_dir_recursive(src, &tmp)
        .map_err(|e| format!("failed to stage install: {}", e))?;
    if dst.exists() {
        // Prefer rename swap; fall back to remove+rename.
        let backup = parent.join(format!(
            ".{}.bak-{}",
            dst.file_name().and_then(|s| s.to_str()).unwrap_or("dep"),
            std::process::id()
        ));
        let _ = std::fs::rename(dst, &backup);
        if let Err(e) = std::fs::rename(&tmp, dst) {
            // Restore backup if swap failed.
            let _ = std::fs::rename(&backup, dst);
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("failed to finalize install: {}", e));
        }
        let _ = std::fs::remove_dir_all(&backup);
    } else if let Err(e) = std::fs::rename(&tmp, dst) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("failed to finalize install: {}", e));
    }
    Ok(())
}

fn resolve_path_dep(
    dep: &manifest::Dependency,
    dst: &Path,
    source: &str,
    base_dir: Option<&Path>,
) -> Result<ResolvedDep, String> {
    // B1: use unified path safety validation.
    crate::path_safety::validate_path_dep(source)?;
    // P-H10: relative path deps resolve against the manifest directory.
    let src = {
        let p = PathBuf::from(source);
        if p.is_absolute() {
            p
        } else if let Some(base) = base_dir {
            base.join(p)
        } else {
            p
        }
    };
    if !src.exists() {
        return Err(format!(
            "path dependency '{}' not found at {}",
            dep.name,
            src.display()
        ));
    }
    install_dir_atomic(&src, dst).map_err(|e| format!("failed to install {}: {}", dep.name, e))?;

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
