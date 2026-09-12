//! Shared source identity helpers for the standalone Rust runtime cache.
//!
//! Both production builds and test-only runtime archives must discover the
//! same `include!` source graph and encode paths without lossy conversion.

use std::path::Path;

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

#[cfg(unix)]
pub fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
pub fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(not(any(unix, windows)))]
pub fn path_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_encoded_bytes().to_vec()
}
