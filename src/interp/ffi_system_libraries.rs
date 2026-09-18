//! Single source of the scalar FFI system-library discovery contract.
//!
//! Both FFI runtimes (the compatibility `FfiRuntime` and the canonical MIR
//! `CanonicalMirFfiRuntime`) must agree on which system libraries are
//! discovered when `MIMI_FFI_LIB` is absent and on how an explicit binding is
//! read from the environment. Native binaries link libc and libm directly, so
//! any runtime that cannot resolve the same symbols diverges from the native
//! backend for identical programs (the 0.39.136 libc parity fix, extended to
//! libm and to explicit-binding strictness here).

/// Candidate system libc paths for the `MIMI_FFI_LIB`-less default.
/// Absolute multiarch paths first; loader sonames are appended by
/// [`default_system_library_candidates`] as the final fallback.
pub(crate) fn default_libc_candidates() -> Vec<&'static str> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "linux")]
    candidates.extend([
        "/lib/x86_64-linux-gnu/libc.so.6",
        "/usr/lib/x86_64-linux-gnu/libc.so.6",
        "/lib64/libc.so.6",
        "/usr/lib/libc.so.6",
        "/lib/libc.so.6",
    ]);
    #[cfg(target_os = "macos")]
    candidates.extend(["/usr/lib/libSystem.B.dylib"]);
    #[cfg(target_os = "windows")]
    candidates.extend(["ucrtbase.dll"]);
    #[cfg(target_os = "android")]
    candidates.extend(["/system/lib64/libc.so", "/system/lib/libc.so"]);
    candidates
}

/// Candidate system libraries for the no-configuration scalar FFI profile.
/// Native builds already link both libc and libm; the VM runtimes must search
/// the same system surface when `MIMI_FFI_LIB` is absent.
///
/// Loader sonames stay at the very end: cross-target Linux installs (for
/// example aarch64) use multiarch directories that are not knowable from this
/// host's absolute path table, so the loader's own search is the last resort.
pub(crate) fn default_system_library_candidates() -> Vec<&'static str> {
    let mut candidates = default_libc_candidates();
    #[cfg(target_os = "linux")]
    candidates.extend([
        "/lib/x86_64-linux-gnu/libm.so.6",
        "/usr/lib/x86_64-linux-gnu/libm.so.6",
        "/lib64/libm.so.6",
        "/usr/lib64/libm.so.6",
        "/usr/lib/libm.so.6",
    ]);
    #[cfg(target_os = "linux")]
    candidates.extend(["libc.so.6", "libm.so.6"]);
    #[cfg(target_os = "macos")]
    candidates.extend(["libSystem.B.dylib", "/usr/lib/libm.dylib", "libm.dylib"]);
    #[cfg(target_os = "android")]
    candidates.extend([
        "libc.so",
        "/system/lib64/libm.so",
        "/system/lib/libm.so",
        "libm.so",
    ]);
    #[cfg(target_os = "windows")]
    candidates.extend(["msvcrt.dll"]);
    candidates
}

/// Admission gate for no-environment candidates: absolute paths must exist as
/// regular files and relative paths are restricted to the platform loader
/// sonames, so discovery can only yield libraries the platform loader actually
/// provides.
pub(crate) fn is_discoverable_system_library_candidate(candidate: &str) -> bool {
    let path = std::path::Path::new(candidate);
    if path.is_absolute() {
        return path.is_file();
    }
    #[cfg(target_os = "linux")]
    {
        return matches!(candidate, "libc.so.6" | "libm.so.6");
    }
    #[cfg(target_os = "macos")]
    {
        return matches!(candidate, "libSystem.B.dylib" | "libm.dylib");
    }
    #[cfg(target_os = "windows")]
    {
        return matches!(candidate, "ucrtbase.dll" | "msvcrt.dll");
    }
    #[cfg(target_os = "android")]
    {
        return matches!(candidate, "libc.so" | "libm.so");
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows",
        target_os = "android"
    )))]
    {
        let _ = path;
        false
    }
}

/// No-environment discovery used by both runtimes: the shared candidate table
/// filtered through the shared admission gate.
pub(crate) fn discover_no_env_candidates() -> Vec<String> {
    default_system_library_candidates()
        .into_iter()
        .filter(|candidate| is_discoverable_system_library_candidate(candidate))
        .map(str::to_owned)
        .collect()
}

/// Read the explicit `MIMI_FFI_LIB` host binding.
///
/// A non-UTF-8 binding fails closed instead of silently falling back to
/// system-library discovery: the program asked for one specific library, and
/// running against a different one would violate the explicit-binding
/// contract. The error text is part of the pinned contract
/// (`scalar_ffi_environment_non_utf8_path_fails_without_cache_pollution`).
pub(crate) fn resolve_explicit_binding() -> Result<Option<String>, String> {
    binding_from_os(std::env::var_os("MIMI_FFI_LIB").as_deref())
}

fn binding_from_os(binding: Option<&std::ffi::OsStr>) -> Result<Option<String>, String> {
    match binding {
        None => Ok(None),
        Some(path) => path
            .to_str()
            .map(|path| Some(path.to_owned()))
            .ok_or_else(|| "failed to load 'MIMI_FFI_LIB': path is not valid UTF-8".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_candidates_are_unique() {
        let candidates = default_system_library_candidates();
        let unique = candidates.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(
            unique.len(),
            candidates.len(),
            "system FFI candidates duplicated"
        );
    }

    #[test]
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows",
        target_os = "android"
    ))]
    fn no_env_discovery_never_degenerates_on_supported_targets() {
        let discovered = discover_no_env_candidates();
        assert!(
            !discovered.is_empty(),
            "the {OS} loader always provides at least one allowlisted soname",
            OS = std::env::consts::OS,
        );
        for candidate in &discovered {
            assert!(
                is_discoverable_system_library_candidate(candidate),
                "discovery must only emit allowlisted candidates: {candidate}"
            );
        }
    }

    #[test]
    fn explicit_binding_passes_unset_and_valid_paths_through() {
        assert_eq!(binding_from_os(None), Ok(None));
        assert_eq!(
            binding_from_os(Some(std::ffi::OsStr::new("/tmp/example.so"))),
            Ok(Some("/tmp/example.so".to_owned()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_binding_rejects_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let malformed = std::ffi::OsStr::from_bytes(&[b'/', b't', b'm', b'p', 0xff]);
        let error =
            binding_from_os(Some(malformed)).expect_err("non-UTF-8 bindings must fail closed");
        assert!(
            error.contains("not valid UTF-8"),
            "pinned diagnostic text must survive the shared helper: {error}"
        );
    }
}
