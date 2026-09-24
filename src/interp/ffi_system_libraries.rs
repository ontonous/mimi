//! Single source of the scalar FFI system-library discovery and selection
//! contract.
//!
//! The canonical MIR FFI runtime shares one contract with native binaries:
//! system-library discovery when `MIMI_FFI_LIB` is absent, explicit host
//! binding resolution, and candidate selection (strict versus best-effort,
//! with symbol-miss diagnostics preferred over later load failures). Native
//! binaries link libc and libm directly, so the MIR runtime must resolve the
//! same symbols for identical programs.

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
/// Native builds already link both libc and libm; the MIR runtime must search
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

/// No-environment discovery used by the canonical MIR FFI runtime: the shared
/// candidate table filtered through the shared admission gate.
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

/// Per-runtime wording for the selector's diagnostic shapes.
///
/// Both runtimes make structurally identical selection decisions, but their
/// diagnostic texts are user-visible and pinned independently by tests, so
/// the shared policy formats every message through this dialect instead of
/// hard-coding either wording.
pub(crate) struct SelectorDialect {
    /// Fatal message for an empty or whitespace-only candidate path.
    pub empty_path: &'static str,
    /// Message for one candidate's load failure; best-effort recording and
    /// strict-stop surfacing use the same text.
    pub load_failure: fn(path: &str, error: &libloading::Error) -> String,
    /// Message for one candidate's missing symbol.
    pub symbol_miss: fn(symbol: &str, error: &libloading::Error) -> String,
    /// Defensive message when a cache index points outside the store.
    pub cache_index_out_of_range: fn(index: usize) -> String,
}

/// Append-only `(path, handle)` store with stable indices, owned by one FFI
/// runtime. The shared selector only grows the cache — it never shrinks,
/// reorders, or evicts — so indices stay valid for the runtime's lifetime.
pub(crate) trait SelectorLibraryCache {
    fn loaded_libs(&self) -> &Vec<(String, libloading::Library)>;
    fn loaded_libs_mut(&mut self) -> &mut Vec<(String, libloading::Library)>;
}

/// Why the shared selector returned without a selected index.
#[derive(Debug)]
pub(crate) enum SelectorRejection {
    /// A candidate path was empty/whitespace; render `dialect.empty_path`.
    EmptyPath,
    /// Strict-mode load failure; the dialect-formatted message is the error.
    LoadFailed(String),
    /// Cache bookkeeping invariant broken; defensive guard against the
    /// append-only cache contract being violated by future edits.
    CacheIndexOutOfRange(usize),
    /// Every candidate was tried without a symbol hit. Diagnostic precedence
    /// stays with the caller: symbol-miss text beats later load-failure text,
    /// and the no-candidate fallback wording is runtime-specific (pinned
    /// independently by each runtime's tests).
    LookupFailed {
        last_symbol_error: Option<String>,
        last_load_error: Option<String>,
    },
}

/// Shared candidate-selection policy for the canonical MIR FFI runtime.
///
/// No-environment candidates are best-effort: an existing file may still be a
/// linker script, the wrong architecture, or simply lack the symbol, so load
/// failures are recorded and iteration continues. Explicit bindings are
/// strict: the first load or symbol failure stops selection. Every library
/// loaded during probing stays cached, so repeated selections reuse handles.
pub(crate) fn select_library_index(
    cache: &mut impl SelectorLibraryCache,
    dialect: &SelectorDialect,
    candidate_paths: Vec<String>,
    configured: bool,
    symbol: &str,
) -> Result<usize, SelectorRejection> {
    let mut last_symbol_error = None;
    let mut last_load_error = None;
    for lib_path in candidate_paths {
        if lib_path.trim().is_empty() {
            return Err(SelectorRejection::EmptyPath);
        }
        let lib_idx = if let Some(index) = cache
            .loaded_libs()
            .iter()
            .position(|(candidate, _)| candidate == &lib_path)
        {
            index
        } else {
            // SAFETY: libloading keeps the library handle alive in
            // `loaded_libs` for every symbol probe made below and for later
            // selections on this runtime.
            let library = unsafe {
                match libloading::Library::new(&lib_path) {
                    Ok(library) => library,
                    Err(error) if !configured => {
                        last_load_error = Some((dialect.load_failure)(&lib_path, &error));
                        continue;
                    }
                    Err(error) => {
                        return Err(SelectorRejection::LoadFailed((dialect.load_failure)(
                            &lib_path, &error,
                        )));
                    }
                }
            };
            let libs = cache.loaded_libs_mut();
            libs.push((lib_path.clone(), library));
            libs.len() - 1
        };
        let Some((_, library)) = cache.loaded_libs().get(lib_idx) else {
            return Err(SelectorRejection::CacheIndexOutOfRange(lib_idx));
        };
        // SAFETY: `library` remains alive in `loaded_libs`; libloading only
        // borrows the handle while probing the NUL-free symbol name.
        match unsafe { library.get::<*mut std::ffi::c_void>(symbol.as_bytes()) } {
            Ok(_) => return Ok(lib_idx),
            Err(error) => {
                last_symbol_error = Some((dialect.symbol_miss)(symbol, &error));
                if configured {
                    break;
                }
            }
        }
    }
    Err(SelectorRejection::LookupFailed {
        last_symbol_error,
        last_load_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ProbeCache(Vec<(String, libloading::Library)>);

    impl SelectorLibraryCache for ProbeCache {
        fn loaded_libs(&self) -> &Vec<(String, libloading::Library)> {
            &self.0
        }
        fn loaded_libs_mut(&mut self) -> &mut Vec<(String, libloading::Library)> {
            &mut self.0
        }
    }

    fn probe_dialect() -> SelectorDialect {
        SelectorDialect {
            empty_path: "empty path rejected",
            load_failure: |path, error| format!("load '{path}': {error}"),
            symbol_miss: |symbol, error| format!("symbol '{symbol}': {error}"),
            cache_index_out_of_range: |index| format!("cache index {index} out of range"),
        }
    }

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

    #[test]
    fn shared_selector_rejects_empty_path_before_touching_loader() {
        let mut cache = ProbeCache(Vec::new());
        let rejection = select_library_index(
            &mut cache,
            &probe_dialect(),
            vec!["   ".to_owned(), "/tmp/r61030-later.so".to_owned()],
            false,
            "labs",
        )
        .expect_err("whitespace-only candidates must be rejected outright");
        assert!(
            matches!(rejection, SelectorRejection::EmptyPath),
            "empty paths must fail before any load attempt: {rejection:?}"
        );
        assert!(
            cache.0.is_empty(),
            "a rejected candidate list must not grow the library cache"
        );
    }

    #[test]
    fn shared_selector_strict_mode_stops_at_first_load_failure() {
        let mut cache = ProbeCache(Vec::new());
        let rejection = select_library_index(
            &mut cache,
            &probe_dialect(),
            vec!["/tmp/r61030-strict-missing-library.so".to_owned()],
            true,
            "labs",
        )
        .expect_err("strict bindings must stop at the first load failure");
        match rejection {
            SelectorRejection::LoadFailed(message) => {
                assert!(
                    message.starts_with("load '/tmp/r61030-strict-missing-library.so':"),
                    "strict load failures surface the dialect text directly: {message}"
                );
            }
            other => panic!("expected a direct strict load failure, got {other:?}"),
        }
        assert!(
            cache.0.is_empty(),
            "a failed load must not poison the library cache"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_selector_records_load_failure_then_prefers_symbol_diagnostic() {
        let dialect = probe_dialect();
        let mut cache = ProbeCache(Vec::new());
        let missing = "/tmp/r61030-best-effort-missing-library.so";
        let selected = select_library_index(
            &mut cache,
            &dialect,
            vec![missing.to_owned(), "libc.so.6".to_owned()],
            false,
            "labs",
        )
        .expect("a best-effort selection must skip the unloadable candidate");
        assert_eq!(
            selected, 0,
            "the failed candidate must not enter the cache; libc.so.6 takes index 0"
        );
        let rejection = select_library_index(
            &mut cache,
            &dialect,
            vec!["libc.so.6".to_owned()],
            false,
            "cos",
        )
        .expect_err("glibc does not export cos");
        match rejection {
            SelectorRejection::LookupFailed {
                last_symbol_error,
                last_load_error,
            } => {
                assert!(
                    last_symbol_error.is_some(),
                    "a symbol miss on a loaded library must be recorded"
                );
                assert!(
                    last_load_error.is_none(),
                    "no load failure happened on the cached candidate"
                );
            }
            other => panic!("expected an aggregate lookup failure, got {other:?}"),
        }
    }
}
