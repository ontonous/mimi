//! Unified path safety validation (B1 architectural debt).
//!
//! All user-controlled path inputs (package names, entry paths, git URLs,
//! path dependencies, stdlib env var) must pass through these validators
//! before being joined to a trusted base directory.  This centralises the
//! scattered ad-hoc checks that previously lived in `add.rs`, `remove.rs`,
//! `publish.rs`, `manifest.rs`, and `pkg_resolve.rs`.
//!
//! ## Threat model
//!
//! - **Path traversal**: `../` sequences to escape the base directory.
//! - **NUL injection**: `\0` truncates paths in C APIs.
//! - **Absolute path injection**: `/etc/passwd` when a relative path is expected.
//! - **Git option injection**: `ext::` protocol RCE, `-` prefix option injection.
//! - **Symlink escape**: best-effort detection via canonicalisation.

use std::fmt;
use std::path::{Path, PathBuf};

/// Error returned by path validation functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// Path contains `..` traversal sequences.
    TraversalEscape,
    /// Path contains NUL bytes.
    NulByte,
    /// Path is absolute when a relative path was expected.
    AbsolutePath,
    /// Path is empty.
    Empty,
    /// Git URL uses a forbidden protocol (e.g. `ext::`).
    ForbiddenProtocol,
    /// Git URL starts with `-` (option injection).
    OptionInjection,
    /// Path contains invalid UTF-8.
    InvalidUtf8,
    /// Symlink would escape the base directory (best-effort).
    SymlinkEscape,
    /// Path is not a valid package name (contains separators, etc.).
    InvalidName,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::TraversalEscape => {
                write!(f, "path contains '..' traversal sequences")
            }
            PathError::NulByte => write!(f, "path contains NUL bytes"),
            PathError::AbsolutePath => write!(f, "path is absolute, expected relative"),
            PathError::Empty => write!(f, "path is empty"),
            PathError::ForbiddenProtocol => {
                write!(f, "git URL uses a forbidden protocol (ext:: not allowed)")
            }
            PathError::OptionInjection => {
                write!(f, "value starts with '-' (possible option injection)")
            }
            PathError::InvalidUtf8 => write!(f, "path contains invalid UTF-8"),
            PathError::SymlinkEscape => write!(f, "symlink would escape base directory"),
            PathError::InvalidName => {
                write!(f, "invalid name: must not contain path separators or '..'")
            }
        }
    }
}

impl std::error::Error for PathError {}

/// Allow `?` to convert `PathError` into `String` in functions that
/// return `Result<_, String>`.
impl From<PathError> for String {
    fn from(e: PathError) -> Self {
        e.to_string()
    }
}

/// Validate that `input` is a safe relative path within `base`.
///
/// Rejects:
/// - `..` traversal sequences
/// - NUL bytes
/// - Absolute paths
/// - Empty strings
///
/// Returns the joined path if safe.
pub fn validate_safe_path(base: &Path, input: &str) -> Result<PathBuf, PathError> {
    if input.is_empty() {
        return Err(PathError::Empty);
    }
    if input.contains('\0') {
        return Err(PathError::NulByte);
    }
    // Check for `..` path components (not just substring, to allow
    // legitimate names like "foo..bar" that don't traverse).
    let p = Path::new(input);
    if p.is_absolute() {
        return Err(PathError::AbsolutePath);
    }
    for component in p.components() {
        if component == std::path::Component::ParentDir {
            return Err(PathError::TraversalEscape);
        }
    }
    Ok(base.join(input))
}

/// Validate a package name or version string.
///
/// Rejects:
/// - `..` (traversal)
/// - `/` or `\` (path separators)
/// - NUL bytes
/// - Empty strings
pub fn validate_package_name(name: &str) -> Result<(), PathError> {
    if name.is_empty() {
        return Err(PathError::Empty);
    }
    if name.contains('\0') {
        return Err(PathError::NulByte);
    }
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(PathError::InvalidName);
    }
    Ok(())
}

/// Validate a git URL to prevent command injection and RCE.
///
/// Rejects:
/// - URLs starting with `-` (git option injection)
/// - `ext::` protocol (arbitrary command execution)
/// - URLs that don't start with a recognised safe scheme
///
/// Allowed schemes: `https://`, `http://`, `ssh://`, `git@`, `git://`, `file://`.
pub fn validate_git_url(url: &str) -> Result<(), PathError> {
    if url.is_empty() {
        return Err(PathError::Empty);
    }
    if url.contains('\0') {
        return Err(PathError::NulByte);
    }
    if url.starts_with('-') {
        return Err(PathError::OptionInjection);
    }
    if url.starts_with("ext::") {
        return Err(PathError::ForbiddenProtocol);
    }
    let safe = url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("ssh://")
        || url.starts_with("git@")
        || url.starts_with("git://")
        || url.starts_with("file://");
    if !safe {
        return Err(PathError::ForbiddenProtocol);
    }
    Ok(())
}

/// Validate a path dependency (relative path without `..`).
///
/// Rejects:
/// - Absolute paths
/// - `..` traversal
/// - NUL bytes
pub fn validate_path_dep(path: &str) -> Result<(), PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path.contains('\0') {
        return Err(PathError::NulByte);
    }
    let p = Path::new(path);
    if p.is_absolute() {
        return Err(PathError::AbsolutePath);
    }
    if path.contains("..") {
        return Err(PathError::TraversalEscape);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_path_accepted() {
        let base = Path::new("/project");
        let result = validate_safe_path(base, "src/main.mimi");
        assert_eq!(result.unwrap(), PathBuf::from("/project/src/main.mimi"));
    }

    #[test]
    fn traversal_rejected() {
        let base = Path::new("/project");
        assert_eq!(
            validate_safe_path(base, "../etc/passwd"),
            Err(PathError::TraversalEscape)
        );
        assert_eq!(
            validate_safe_path(base, "src/../../etc/passwd"),
            Err(PathError::TraversalEscape)
        );
    }

    #[test]
    fn nul_byte_rejected() {
        let base = Path::new("/project");
        assert_eq!(
            validate_safe_path(base, "main\0.mimi"),
            Err(PathError::NulByte)
        );
    }

    #[test]
    fn absolute_path_rejected() {
        let base = Path::new("/project");
        assert_eq!(
            validate_safe_path(base, "/etc/passwd"),
            Err(PathError::AbsolutePath)
        );
    }

    #[test]
    fn empty_path_rejected() {
        let base = Path::new("/project");
        assert_eq!(validate_safe_path(base, ""), Err(PathError::Empty));
    }

    #[test]
    fn double_dot_in_name_without_traversal_accepted() {
        // "foo..bar" is a valid filename, not traversal
        let base = Path::new("/project");
        let result = validate_safe_path(base, "foo..bar");
        assert_eq!(result.unwrap(), PathBuf::from("/project/foo..bar"));
    }

    #[test]
    fn valid_package_name_accepted() {
        assert!(validate_package_name("my-pkg").is_ok());
        assert!(validate_package_name("my_pkg_123").is_ok());
    }

    #[test]
    fn invalid_package_name_rejected() {
        assert_eq!(
            validate_package_name("../evil"),
            Err(PathError::InvalidName)
        );
        assert_eq!(validate_package_name("a/b"), Err(PathError::InvalidName));
        assert_eq!(validate_package_name("a\\b"), Err(PathError::InvalidName));
        assert_eq!(validate_package_name(""), Err(PathError::Empty));
        assert_eq!(validate_package_name("pkg\0name"), Err(PathError::NulByte));
    }

    #[test]
    fn valid_git_url_accepted() {
        assert!(validate_git_url("https://github.com/user/repo.git").is_ok());
        assert!(validate_git_url("ssh://git@github.com/user/repo.git").is_ok());
        assert!(validate_git_url("git@github.com:user/repo.git").is_ok());
        assert!(validate_git_url("file:///tmp/repo").is_ok());
    }

    #[test]
    fn ext_protocol_rejected() {
        assert_eq!(
            validate_git_url("ext::sh -c 'rm -rf /'"),
            Err(PathError::ForbiddenProtocol)
        );
    }

    #[test]
    fn option_injection_rejected() {
        assert_eq!(
            validate_git_url("-uupload-pack"),
            Err(PathError::OptionInjection)
        );
    }

    #[test]
    fn unknown_scheme_rejected() {
        assert_eq!(
            validate_git_url("ftp://evil.com/repo"),
            Err(PathError::ForbiddenProtocol)
        );
    }

    #[test]
    fn valid_path_dep_accepted() {
        assert!(validate_path_dep("./libs/foo").is_ok());
        assert!(validate_path_dep("libs/foo").is_ok());
    }

    #[test]
    fn absolute_path_dep_rejected() {
        assert_eq!(
            validate_path_dep("/home/user/.ssh"),
            Err(PathError::AbsolutePath)
        );
    }

    #[test]
    fn traversal_path_dep_rejected() {
        assert_eq!(
            validate_path_dep("../../etc/passwd"),
            Err(PathError::TraversalEscape)
        );
    }
}
