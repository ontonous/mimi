//! Shared source identity helpers for the standalone Rust runtime cache.
//!
//! Both production builds and test-only runtime archives must discover the
//! same `include!` source graph and encode paths without lossy conversion.

use std::ffi::OsStr;
use std::path::Path;

/// Apply environment overrides that are part of the standalone `rustc`
/// invocation contract.  Keep this shared by identity probes and archive
/// compilation so the cache frame cannot drift from the command that builds
/// the artifact.
pub fn configure_rustc_command(command: &mut std::process::Command, asan: bool) {
    if asan {
        command.env("RUSTUP_TOOLCHAIN", "nightly");
    }
}

pub fn included_sources(
    runtime_rs: &Path,
    known_sources: &[std::path::PathBuf],
) -> Result<Vec<std::path::PathBuf>, String> {
    let mut known = known_sources
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut pending = known_sources.to_vec();
    pending.push(runtime_rs.to_path_buf());
    let mut scanned = std::collections::HashSet::new();
    let mut included = Vec::new();

    while let Some(source) = pending.pop() {
        if !scanned.insert(source.clone()) {
            continue;
        }
        let source_text = std::fs::read_to_string(&source)
            .map_err(|error| format!("read runtime include source {source:?}: {error}"))?;
        let includes = include_literals(&source_text)
            .map_err(|error| format!("parse runtime include source {source:?}: {error}"))?;
        for include in includes {
            let include_path = source
                .parent()
                .ok_or_else(|| format!("runtime include source has no parent: {source:?}"))?
                .join(include);
            if known.insert(include_path.clone()) {
                let metadata = std::fs::symlink_metadata(&include_path).map_err(|error| {
                    format!("inspect runtime included source: {include_path:?}: {error}")
                })?;
                if !metadata.file_type().is_file() {
                    return Err(format!(
                        "runtime included source is not a regular file: {include_path:?}"
                    ));
                }
                included.push(include_path.clone());
            }
            if !scanned.contains(&include_path) {
                pending.push(include_path);
            }
        }
    }
    Ok(included)
}

pub fn include_literals(source: &str) -> Result<Vec<String>, String> {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut cursor = 0;

    while cursor < bytes.len() {
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'/') {
            cursor += 2;
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            continue;
        }
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            cursor = skip_rust_block_comment(bytes, cursor)?;
            continue;
        }
        if bytes[cursor] == b'"' {
            cursor = skip_rust_quoted_literal(bytes, cursor)?;
            continue;
        }
        if bytes[cursor] == b'r' {
            if let Some(next) = skip_rust_raw_literal(bytes, cursor)? {
                cursor = next;
                continue;
            }
        }
        if bytes[cursor] == b'\'' {
            let escaped = bytes.get(cursor + 1) == Some(&b'\\');
            let ascii_char = bytes.get(cursor + 2) == Some(&b'\'');
            if escaped || ascii_char {
                cursor = skip_rust_char_literal(bytes, cursor)?;
                continue;
            }
        }

        let macro_name = b"include!";
        let is_macro = bytes[cursor..].starts_with(macro_name)
            && (cursor == 0 || !is_rust_ident_byte(bytes[cursor - 1]));
        if !is_macro {
            cursor += 1;
            continue;
        }

        let macro_end = cursor + macro_name.len();
        cursor = macro_end;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'(') {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }

        if bytes.get(cursor) == Some(&b'"') {
            let start = cursor + 1;
            let end = find_rust_quoted_literal_end(bytes, cursor)?;
            let literal = &source[start..end];
            if literal.as_bytes().contains(&b'\\') {
                return Err(
                    "include! path literal contains an escape; use a raw string literal".into(),
                );
            }
            literals.push(literal.to_owned());
            cursor = end + 1;
            continue;
        }
        if bytes.get(cursor) == Some(&b'r') {
            if let Some((start, end, next)) = parse_rust_raw_literal(bytes, cursor)? {
                literals.push(source[start..end].to_owned());
                cursor = next;
                continue;
            }
        }
        return Err("include! path must be a direct string literal".into());
    }
    Ok(literals)
}

fn is_rust_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_rust_block_comment(bytes: &[u8], mut cursor: usize) -> Result<usize, String> {
    let mut depth = 1_u32;
    cursor += 2;
    while cursor < bytes.len() {
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            depth = depth
                .checked_add(1)
                .ok_or_else(|| "nested Rust block comments overflowed".to_string())?;
            cursor += 2;
        } else if bytes[cursor] == b'*' && bytes.get(cursor + 1) == Some(&b'/') {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return Ok(cursor);
            }
        } else {
            cursor += 1;
        }
    }
    Err("unterminated Rust block comment".into())
}

fn skip_rust_quoted_literal(bytes: &[u8], cursor: usize) -> Result<usize, String> {
    Ok(find_rust_quoted_literal_end(bytes, cursor)? + 1)
}

fn find_rust_quoted_literal_end(bytes: &[u8], mut cursor: usize) -> Result<usize, String> {
    cursor += 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => {
                cursor += 2;
            }
            b'"' => return Ok(cursor),
            _ => cursor += 1,
        }
    }
    Err("unterminated Rust string literal".into())
}

fn skip_rust_char_literal(bytes: &[u8], mut cursor: usize) -> Result<usize, String> {
    cursor += 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor += 2,
            b'\'' => return Ok(cursor + 1),
            _ => cursor += 1,
        }
    }
    Err("unterminated Rust character literal".into())
}

fn skip_rust_raw_literal(bytes: &[u8], cursor: usize) -> Result<Option<usize>, String> {
    Ok(parse_rust_raw_literal(bytes, cursor)?.map(|(_, _, next)| next))
}

fn parse_rust_raw_literal(
    bytes: &[u8],
    cursor: usize,
) -> Result<Option<(usize, usize, usize)>, String> {
    if bytes.get(cursor) != Some(&b'r') {
        return Ok(None);
    }
    let mut quote = cursor + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return Ok(None);
    }
    let hash_count = quote - cursor - 1;
    let start = quote + 1;
    let mut end = start;
    while end < bytes.len() {
        if bytes[end] == b'"'
            && bytes
                .get(end + 1..end + 1 + hash_count)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            return Ok(Some((start, end, end + 1 + hash_count)));
        }
        end += 1;
    }
    Err("unterminated Rust raw string literal".into())
}

const RUSTC_ENVIRONMENT_KEYS: &[&str] = &[
    "RUSTUP_TOOLCHAIN",
    "RUSTC_BOOTSTRAP",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTUP_HOME",
    "PATH",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
];

/// Encode the effective environment inherited by the standalone `rustc`
/// invocation.  The presence marker distinguishes an unset variable from an
/// explicitly empty one, and ASan mirrors the production command's explicit
/// `RUSTUP_TOOLCHAIN=nightly` override.
pub fn compiler_environment_frame(asan: bool) -> Vec<u8> {
    compiler_environment_frame_from_lookup(asan, |key| {
        std::env::var_os(key).map(|value| os_str_bytes(&value))
    })
}

fn compiler_environment_frame_from_lookup<F>(asan: bool, mut lookup: F) -> Vec<u8>
where
    F: FnMut(&str) -> Option<Vec<u8>>,
{
    let mut frame = b"rustc-env\0".to_vec();
    for key in RUSTC_ENVIRONMENT_KEYS {
        let value = if asan && *key == "RUSTUP_TOOLCHAIN" {
            Some(b"nightly".to_vec())
        } else {
            lookup(key)
        };
        append_len_framed(&mut frame, key.as_bytes());
        match value {
            Some(value) => {
                frame.push(1);
                append_len_framed(&mut frame, &value);
            }
            None => frame.push(0),
        }
    }
    frame
}

fn append_len_framed(frame: &mut Vec<u8>, bytes: &[u8]) {
    frame.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    frame.extend_from_slice(bytes);
}

#[cfg(unix)]
pub fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    value.as_bytes().to_vec()
}

#[cfg(windows)]
pub fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
pub fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    value.as_encoded_bytes().to_vec()
}

pub fn path_bytes(path: &Path) -> Vec<u8> {
    os_str_bytes(path.as_os_str())
}

#[cfg(test)]
mod tests {
    use super::{
        compiler_environment_frame, compiler_environment_frame_from_lookup,
        configure_rustc_command, RUSTC_ENVIRONMENT_KEYS,
    };
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[cfg(unix)]
    fn rustc_on_path() -> PathBuf {
        let path = std::env::var_os("PATH").expect("PATH must be available for rustc probe");
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join("rustc");
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!("rustc is not available on PATH");
    }

    #[cfg(unix)]
    fn parse_environment(bytes: &[u8]) -> HashMap<String, Vec<u8>> {
        bytes
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                let separator = entry
                    .iter()
                    .position(|byte| *byte == b'=')
                    .expect("env -0 entry must contain '='");
                let key = String::from_utf8(entry[..separator].to_vec())
                    .expect("environment key must be UTF-8");
                (key, entry[separator + 1..].to_vec())
            })
            .collect()
    }

    #[cfg(unix)]
    fn write_probe_script(path: &Path) {
        std::fs::write(
            path,
            b"#!/bin/sh\nset -eu\n/usr/bin/env -0 > \"$MIMI_RUSTC_ENV_CAPTURE\"\nexec \"$MIMI_RUSTC_REAL\" \"$@\"\n",
        )
        .expect("write rustc environment probe");
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)
            .expect("stat rustc environment probe")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)
            .expect("make rustc environment probe executable");
    }

    /// Check the frame against the environment captured immediately before a
    /// real rustc process is exec'd.  This catches drift in presence handling,
    /// non-UTF-8 values, and the ASan toolchain override without mutating the
    /// test process environment.
    #[cfg(unix)]
    #[test]
    fn compiler_environment_frame_matches_actual_rustc_subprocess() {
        let root = std::env::temp_dir().join(format!(
            "mimi-runtime-env-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create rustc environment probe directory");
        let script = root.join("rustc-probe.sh");
        let capture = root.join("environment.bin");
        write_probe_script(&script);
        let real_rustc = rustc_on_path();

        for asan in [false, true] {
            let mut command = Command::new(&script);
            command
                .env("MIMI_RUSTC_ENV_CAPTURE", &capture)
                .env("MIMI_RUSTC_REAL", &real_rustc)
                .args(["--version", "--verbose"]);
            configure_rustc_command(&mut command, asan);
            let output = command.output().expect("spawn rustc environment probe");
            assert!(
                output.status.success(),
                "rustc environment probe failed for asan={asan}: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            let observed = parse_environment(
                &std::fs::read(&capture).expect("read captured rustc environment"),
            );
            let observed_frame =
                compiler_environment_frame_from_lookup(asan, |key| observed.get(key).cloned());
            assert_eq!(
                observed_frame,
                compiler_environment_frame(asan),
                "cache environment frame diverged from rustc child for asan={asan}; keys={RUSTC_ENVIRONMENT_KEYS:?}"
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn compiler_environment_frame_distinguishes_unset_and_empty_values() {
        let unset = compiler_environment_frame_from_lookup(false, |_| None);
        let empty =
            compiler_environment_frame_from_lookup(false, |key| (key == "PATH").then(Vec::new));
        assert_ne!(unset, empty);
        assert!(empty.windows(b"PATH".len()).any(|window| window == b"PATH"));
    }
}
