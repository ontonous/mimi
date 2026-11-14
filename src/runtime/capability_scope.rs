//! Process-wide capability scope registry.
//!
//! `SystemToken` is deliberately represented as an opaque `i64` at the Mimi
//! ABI boundary.  The integer is only an identity; all authorization state is
//! kept here in a host-owned registry.  This module is the dynamic half of the
//! capability contract: the checker verifies that a guarded operation carries
//! a `SystemToken`, while this registry verifies the concrete path/endpoint at
//! the point of use.
//!
//! A token returned by [`issue_unrestricted`] is the compatibility form used
//! by the existing `make_token()` builtin.  It is still an unforgeable
//! registry identity, but has a wildcard scope.  Hosts that need least
//! privilege should issue a scoped token with [`issue_path`] or
//! [`issue_endpoint`], or narrow a compatibility token with one of the
//! `grant_*` functions before handing it to Mimi code.  Once a grant is added,
//! the wildcard is removed and every subsequent check is fail-closed.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{OnceLock, RwLock};

/// Stable version for serialized host capability manifests.
pub const CAPABILITY_SCOPE_ABI_VERSION: u32 = 1;

static NEXT_TOKEN: AtomicI64 = AtomicI64::new(1);
static TOKEN_SALT: OnceLock<u64> = OnceLock::new();
static TOKENS: OnceLock<RwLock<HashMap<i64, TokenGrant>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<i64, TokenGrant>> {
    TOKENS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Access type for a filesystem capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAccess {
    Read,
    Write,
}

impl fmt::Display for PathAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
        }
    }
}

/// Dynamic authorization failure.  The variants are intentionally stable so
/// callers can turn them into a diagnostic without matching on prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    UnknownToken(i64),
    Revoked(i64),
    PathDenied {
        token: i64,
        access: PathAccess,
        path: PathBuf,
    },
    EndpointDenied {
        token: i64,
        host: String,
        port: u16,
    },
    EnvDenied {
        token: i64,
        name: String,
    },
    InvalidPath(String),
    InvalidEndpoint(String),
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownToken(token) => write!(f, "unknown capability token {token}"),
            Self::Revoked(token) => write!(f, "capability token {token} has been revoked"),
            Self::PathDenied {
                token,
                access,
                path,
            } => write!(
                f,
                "capability token {token} is not authorized to {access} path {}",
                path.display()
            ),
            Self::EndpointDenied { token, host, port } => write!(
                f,
                "capability token {token} is not authorized for endpoint {host}:{port}"
            ),
            Self::EnvDenied { token, name } => write!(
                f,
                "capability token {token} is not authorized to read environment variable {name}"
            ),
            Self::InvalidPath(path) => write!(f, "invalid capability path: {path}"),
            Self::InvalidEndpoint(endpoint) => write!(f, "invalid capability endpoint: {endpoint}"),
        }
    }
}

impl std::error::Error for ScopeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathGrant {
    root: PathBuf,
    read: bool,
    write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EndpointGrant {
    host: String,
    port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenGrant {
    wildcard: bool,
    revoked: bool,
    paths: Vec<PathGrant>,
    endpoints: Vec<EndpointGrant>,
    env: Vec<String>,
    generation: u64,
}

impl TokenGrant {
    fn wildcard() -> Self {
        Self {
            wildcard: true,
            revoked: false,
            paths: Vec::new(),
            endpoints: Vec::new(),
            env: Vec::new(),
            generation: 1,
        }
    }

    fn scoped() -> Self {
        Self {
            wildcard: false,
            revoked: false,
            paths: Vec::new(),
            endpoints: Vec::new(),
            env: Vec::new(),
            generation: 1,
        }
    }
}

fn next_token(grant: TokenGrant) -> i64 {
    // Zero is reserved as the invalid/empty handle.  The sequence is mixed
    // with a process-local entropy seed before entering the registry: a raw
    // sequential guess from an FFI caller therefore cannot manufacture a
    // live capability identity.  Wrapping is practically unreachable, but
    // skip zero/collisions so an eventual counter wrap remains fail closed.
    loop {
        let sequence = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed) as u64;
        let token = ((sequence.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ token_salt())
            & (i64::MAX as u64)) as i64;
        if token == 0 {
            continue;
        }
        let mut table = registry()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let std::collections::hash_map::Entry::Vacant(slot) = table.entry(token) {
            slot.insert(grant);
            return token;
        }
    }
}

fn token_salt() -> u64 {
    *TOKEN_SALT.get_or_init(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0);
        let stack = (&now as *const u64 as usize) as u64;
        let process = std::process::id() as u64;
        let mut seed = now ^ stack.rotate_left(17) ^ process.rotate_left(31);
        // A zero seed would make the first mixed value predictable even
        // though the sequence remains nonzero; use a fixed odd fallback.
        if seed == 0 {
            seed = 0xa5a5_5a5a_1357_9bdf;
        }
        seed ^ seed.rotate_left(29) ^ seed.rotate_right(13)
    })
}

/// Issue the compatibility wildcard token used by `make_token()`.
pub fn issue_unrestricted() -> i64 {
    next_token(TokenGrant::wildcard())
}

/// Issue a token with one read/write directory root.
pub fn issue_path(root: impl AsRef<Path>) -> Result<i64, ScopeError> {
    let root = canonical_root(root.as_ref())?;
    let token = next_token(TokenGrant::scoped());
    grant_path(token, root).map(|_| token)
}

/// Issue a token with one endpoint grant.
pub fn issue_endpoint(host: &str, port: Option<u16>) -> Result<i64, ScopeError> {
    let host = normalize_host(host)?;
    let token = next_token(TokenGrant::scoped());
    grant_endpoint(token, &host, port).map(|_| token)
}

/// Add a read/write directory grant.  Adding the first grant narrows a
/// wildcard token to explicit scope; subsequent grants are additive.
pub fn grant_path(token: i64, root: impl AsRef<Path>) -> Result<(), ScopeError> {
    grant_path_access(token, root, true, true)
}

/// Add a read-only directory grant.
pub fn grant_path_read(token: i64, root: impl AsRef<Path>) -> Result<(), ScopeError> {
    grant_path_access(token, root, true, false)
}

/// Add a write-only directory grant.
pub fn grant_path_write(token: i64, root: impl AsRef<Path>) -> Result<(), ScopeError> {
    grant_path_access(token, root, false, true)
}

fn grant_path_access(
    token: i64,
    root: impl AsRef<Path>,
    read: bool,
    write: bool,
) -> Result<(), ScopeError> {
    let root = canonical_root(root.as_ref())?;
    let mut table = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let grant = table
        .get_mut(&token)
        .ok_or(ScopeError::UnknownToken(token))?;
    if grant.revoked {
        return Err(ScopeError::Revoked(token));
    }
    grant.wildcard = false;
    if let Some(existing) = grant.paths.iter_mut().find(|entry| entry.root == root) {
        existing.read |= read;
        existing.write |= write;
    } else {
        grant.paths.push(PathGrant { root, read, write });
    }
    grant.generation = grant.generation.saturating_add(1);
    Ok(())
}

/// Add an endpoint grant.  `None` for the port means all ports for the host.
pub fn grant_endpoint(token: i64, host: &str, port: Option<u16>) -> Result<(), ScopeError> {
    let host = normalize_host(host)?;
    let mut table = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let grant = table
        .get_mut(&token)
        .ok_or(ScopeError::UnknownToken(token))?;
    if grant.revoked {
        return Err(ScopeError::Revoked(token));
    }
    grant.wildcard = false;
    let entry = EndpointGrant { host, port };
    if !grant.endpoints.contains(&entry) {
        grant.endpoints.push(entry);
    }
    grant.generation = grant.generation.saturating_add(1);
    Ok(())
}

/// Add one environment variable name to the token's scope.
pub fn grant_env(token: i64, name: &str) -> Result<(), ScopeError> {
    if name.is_empty() || name.contains('\0') || name.contains('=') {
        return Err(ScopeError::InvalidEndpoint(name.to_string()));
    }
    let mut table = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let grant = table
        .get_mut(&token)
        .ok_or(ScopeError::UnknownToken(token))?;
    if grant.revoked {
        return Err(ScopeError::Revoked(token));
    }
    grant.wildcard = false;
    if !grant.env.iter().any(|item| item == name) {
        grant.env.push(name.to_string());
    }
    grant.generation = grant.generation.saturating_add(1);
    Ok(())
}

/// Revoke a token.  Revocation is sticky and the integer is never reused.
pub fn revoke(token: i64) -> Result<(), ScopeError> {
    let mut table = registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let grant = table
        .get_mut(&token)
        .ok_or(ScopeError::UnknownToken(token))?;
    grant.revoked = true;
    grant.wildcard = false;
    grant.generation = grant.generation.saturating_add(1);
    Ok(())
}

/// Remove a token from the registry.  This is intended for test/host cleanup;
/// normal capability revocation should use [`revoke`] so stale handles remain
/// diagnosable as revoked rather than becoming indistinguishable from forged
/// integers.
pub fn forget(token: i64) {
    registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&token);
}

/// Check a path against a token's dynamic directory scope.
pub fn check_path(
    token: i64,
    path: impl AsRef<Path>,
    access: PathAccess,
) -> Result<(), ScopeError> {
    let path = path.as_ref();
    if path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
        return Err(ScopeError::InvalidPath(path.display().to_string()));
    }
    let table = registry()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let grant = table.get(&token).ok_or(ScopeError::UnknownToken(token))?;
    if grant.revoked {
        return Err(ScopeError::Revoked(token));
    }
    if grant.wildcard {
        return Ok(());
    }
    let canonical = canonical_target(path).map_err(|_| ScopeError::PathDenied {
        token,
        access,
        path: path.to_path_buf(),
    })?;
    if grant.paths.iter().any(|entry| {
        let permission = match access {
            PathAccess::Read => entry.read,
            PathAccess::Write => entry.write,
        };
        permission && canonical.starts_with(&entry.root)
    }) {
        Ok(())
    } else {
        Err(ScopeError::PathDenied {
            token,
            access,
            path: path.to_path_buf(),
        })
    }
}

/// Check a normalized endpoint against the token's dynamic network scope.
pub fn check_endpoint(token: i64, host: &str, port: u16) -> Result<(), ScopeError> {
    let host = normalize_host(host)?;
    let table = registry()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let grant = table.get(&token).ok_or(ScopeError::UnknownToken(token))?;
    if grant.revoked {
        return Err(ScopeError::Revoked(token));
    }
    if grant.wildcard
        || grant
            .endpoints
            .iter()
            .any(|entry| entry.host == host && entry.port.map_or(true, |allowed| allowed == port))
    {
        Ok(())
    } else {
        Err(ScopeError::EndpointDenied { token, host, port })
    }
}

/// Check environment-variable scope.
pub fn check_env(token: i64, name: &str) -> Result<(), ScopeError> {
    let table = registry()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let grant = table.get(&token).ok_or(ScopeError::UnknownToken(token))?;
    if grant.revoked {
        return Err(ScopeError::Revoked(token));
    }
    if grant.wildcard || grant.env.iter().any(|item| item == name) {
        Ok(())
    } else {
        Err(ScopeError::EnvDenied {
            token,
            name: name.to_owned(),
        })
    }
}

/// Return a deterministic, human/machine readable scope summary.
pub fn describe(token: i64) -> Result<String, ScopeError> {
    let table = registry()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let grant = table.get(&token).ok_or(ScopeError::UnknownToken(token))?;
    let mut paths: Vec<String> = grant
        .paths
        .iter()
        .map(|entry| {
            let mut mode = String::new();
            if entry.read {
                mode.push('r');
            }
            if entry.write {
                mode.push('w');
            }
            format!("{}:{mode}", entry.root.display())
        })
        .collect();
    paths.sort();
    let mut endpoints: Vec<String> = grant
        .endpoints
        .iter()
        .map(|entry| {
            format!(
                "{}:{}",
                entry.host,
                entry.port.map_or_else(|| "*".into(), |p| p.to_string())
            )
        })
        .collect();
    endpoints.sort();
    let mut env = grant.env.clone();
    env.sort();
    Ok(format!(
        "version={};token={token};generation={};revoked={};wildcard={};paths=[{}];endpoints=[{}];env=[{}]",
        CAPABILITY_SCOPE_ABI_VERSION,
        grant.generation,
        grant.revoked,
        grant.wildcard,
        paths.join(","),
        endpoints.join(","),
        env.join(",")
    ))
}

fn canonical_root(root: &Path) -> Result<PathBuf, ScopeError> {
    if root.as_os_str().is_empty() || root.to_string_lossy().contains('\0') {
        return Err(ScopeError::InvalidPath(root.display().to_string()));
    }
    // A root must already exist.  This prevents a host from authorizing one
    // spelling and later having a symlink swap create a different root.
    let canonical = std::fs::canonicalize(root)
        .map_err(|_| ScopeError::InvalidPath(root.display().to_string()))?;
    if !canonical.is_dir() {
        return Err(ScopeError::InvalidPath(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    Ok(canonical)
}

fn canonical_target(path: &Path) -> Result<PathBuf, std::io::Error> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // For a new file, canonicalize the nearest existing parent and
            // append the final component.  Every existing parent is therefore
            // checked for symlink escapes.
            let name = path.file_name().ok_or(error)?;
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            Ok(std::fs::canonicalize(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

fn normalize_host(host: &str) -> Result<String, ScopeError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || host.contains('\0')
        || host.contains('/')
        || host.contains('\\')
        || host.contains(['?', '#', '%', '@'])
        || host.chars().any(char::is_whitespace)
    {
        return Err(ScopeError::InvalidEndpoint(host));
    }
    // A colon is legal only as part of a bare IPv6 literal.  Host names and
    // IPv4 literals must never carry a second `:port` component here; callers
    // pass the port separately so accepting it would make scope comparisons
    // ambiguous.  Bracketed literals are stripped by the URL parser before
    // reaching this function, but accepting brackets directly keeps the C API
    // useful for a host/port pair supplied by the caller.
    let candidate = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'));
    let normalized = candidate.unwrap_or(&host);
    if normalized.contains(':') && normalized.parse::<std::net::IpAddr>().is_err() {
        return Err(ScopeError::InvalidEndpoint(host));
    }
    Ok(normalized.to_owned())
}

fn cstr(ptr: *const std::ffi::c_char) -> Result<String, ScopeError> {
    if ptr.is_null() {
        return Err(ScopeError::InvalidPath("null C string".into()));
    }
    // SAFETY: callers of the C ABI must provide a valid NUL-terminated string.
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| ScopeError::InvalidPath("invalid UTF-8 C string".into()))
}

fn scope_result(result: Result<(), ScopeError>) -> i32 {
    match result {
        Ok(()) => 1,
        Err(error) => {
            eprintln!("[mimi capability] {error}");
            0
        }
    }
}

/// Issue the compatibility wildcard token through the native ABI.
#[no_mangle]
pub extern "C" fn mimi_capability_issue_unrestricted() -> i64 {
    issue_unrestricted()
}

/// Issue a token scoped to a directory (read/write).
///
/// # Safety
/// If `root` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_issue_path(root: *const std::ffi::c_char) -> i64 {
    let root = match cstr(root) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[mimi capability] {error}");
            return 0;
        }
    };
    issue_path(root).unwrap_or(0)
}

/// Issue a token scoped to one host and optional port.  A negative port means
/// all ports for that host.
///
/// # Safety
/// If `host` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_issue_endpoint(
    host: *const std::ffi::c_char,
    port: i64,
) -> i64 {
    let host = match cstr(host) {
        Ok(host) => host,
        Err(error) => {
            eprintln!("[mimi capability] {error}");
            return 0;
        }
    };
    issue_endpoint(
        &host,
        (0..=u16::MAX as i64).contains(&port).then_some(port as u16),
    )
    .unwrap_or(0)
}

/// Narrow a token with a read/write directory grant.
///
/// # Safety
/// If `root` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_grant_path(
    token: i64,
    root: *const std::ffi::c_char,
) -> i32 {
    let root = match cstr(root) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[mimi capability] {error}");
            return 0;
        }
    };
    scope_result(grant_path(token, root))
}

/// Narrow a token with an endpoint grant.  A negative port means all ports.
///
/// # Safety
/// If `host` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_grant_endpoint(
    token: i64,
    host: *const std::ffi::c_char,
    port: i64,
) -> i32 {
    let host = match cstr(host) {
        Ok(host) => host,
        Err(error) => {
            eprintln!("[mimi capability] {error}");
            return 0;
        }
    };
    scope_result(grant_endpoint(
        token,
        &host,
        (0..=u16::MAX as i64).contains(&port).then_some(port as u16),
    ))
}

/// Add one environment variable grant.
///
/// # Safety
/// If `name` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_grant_env(
    token: i64,
    name: *const std::ffi::c_char,
) -> i32 {
    let name = match cstr(name) {
        Ok(name) => name,
        Err(error) => {
            eprintln!("[mimi capability] {error}");
            return 0;
        }
    };
    scope_result(grant_env(token, &name))
}

/// Revoke a token.  A revoked token never authorizes a later call.
#[no_mangle]
pub extern "C" fn mimi_capability_revoke(token: i64) -> i32 {
    scope_result(revoke(token))
}

/// Check a path.  `write != 0` requests write access; zero requests read.
///
/// # Safety
/// If `path` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_check_path(
    token: i64,
    path: *const std::ffi::c_char,
    write: i32,
) -> i32 {
    let path = match cstr(path) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("[mimi capability] {error}");
            return 0;
        }
    };
    scope_result(check_path(
        token,
        path,
        if write == 0 {
            PathAccess::Read
        } else {
            PathAccess::Write
        },
    ))
}

/// Check a host/port endpoint.
///
/// # Safety
/// If `host` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_check_endpoint(
    token: i64,
    host: *const std::ffi::c_char,
    port: i64,
) -> i32 {
    let host = match cstr(host) {
        Ok(host) => host,
        Err(error) => {
            eprintln!("[mimi capability] {error}");
            return 0;
        }
    };
    let Ok(port) = u16::try_from(port) else {
        eprintln!("[mimi capability] invalid endpoint port {port}");
        return 0;
    };
    scope_result(check_endpoint(token, &host, port))
}

/// Check an `http://host[:port]/...` URL directly.  Keeping URL parsing next
/// to the endpoint registry ensures the native guarded builtin and the VM
/// guarded builtin apply the same host/port rule.
///
/// # Safety
/// If `url` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_check_url(
    token: i64,
    url: *const std::ffi::c_char,
) -> i32 {
    let Ok(url) = cstr(url) else {
        return 0;
    };
    let scheme_end = url.find("://");
    let host_start = match scheme_end {
        Some(index) if url[..index].eq_ignore_ascii_case("http") => index + 3,
        Some(_) => {
            eprintln!("[mimi capability] unsupported URL scheme");
            return 0;
        }
        None => 0,
    };
    let rest = &url[host_start..];
    if rest.is_empty() {
        eprintln!("[mimi capability] URL is missing a host");
        return 0;
    }
    let host_part = rest.split_once('/').map_or(rest, |(host, _)| host);
    if host_part.is_empty() {
        eprintln!("[mimi capability] URL is missing a host");
        return 0;
    }
    let (host, port) = if host_part.starts_with('[') {
        let Some(close) = host_part.find(']') else {
            eprintln!("[mimi capability] unterminated IPv6 host");
            return 0;
        };
        let host = &host_part[1..close];
        if host.is_empty() {
            eprintln!("[mimi capability] empty IPv6 host");
            return 0;
        }
        let suffix = &host_part[close + 1..];
        let port = if suffix.is_empty() {
            80
        } else if let Some(port) = suffix.strip_prefix(':') {
            match port.parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    eprintln!("[mimi capability] invalid URL port");
                    return 0;
                }
            }
        } else {
            eprintln!("[mimi capability] invalid IPv6 host suffix");
            return 0;
        };
        (host, port)
    } else if let Ok(address) = host_part.parse::<std::net::IpAddr>() {
        // Bare IPv6 literals have several colons and therefore cannot be
        // split with `rsplit_once(':')`; a port must use bracket notation.
        (
            match address {
                std::net::IpAddr::V4(_) | std::net::IpAddr::V6(_) => host_part,
            },
            80,
        )
    } else if let Some((host, port)) = host_part.rsplit_once(':') {
        if host.is_empty() {
            eprintln!("[mimi capability] URL is missing a host");
            return 0;
        }
        let Ok(port) = port.parse::<u16>() else {
            eprintln!("[mimi capability] invalid URL port");
            return 0;
        };
        (host, port)
    } else {
        (host_part, 80)
    };
    scope_result(check_endpoint(token, host, port))
}

/// Check an environment variable name.
///
/// # Safety
/// If `name` is non-null, it must point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_check_env(
    token: i64,
    name: *const std::ffi::c_char,
) -> i32 {
    let name = match cstr(name) {
        Ok(name) => name,
        Err(error) => {
            eprintln!("[mimi capability] {error}");
            return 0;
        }
    };
    scope_result(check_env(token, &name))
}

/// Return a deterministic text descriptor allocated with the Mimi runtime
/// allocator.  The caller owns the returned string and must free it with
/// `mimi_free`.
///
/// # Safety
/// Caller owns the returned non-null C string allocation and must free it with
/// `mimi_free` after use.
#[no_mangle]
pub unsafe extern "C" fn mimi_capability_describe(token: i64) -> *mut std::ffi::c_char {
    let Ok(text) = describe(token) else {
        return std::ptr::null_mut();
    };
    let bytes = text.as_bytes();
    let ptr = super::mimi_alloc(bytes.len().saturating_add(1)) as *mut u8;
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: ptr points to a fresh allocation of bytes.len()+1 bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr.cast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("mimi_cap_scope_{label}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test scope");
        path
    }

    #[test]
    fn scoped_path_rejects_traversal_and_symlink_escape() {
        let root = temp_dir("path");
        let outside = temp_dir("outside");
        fs::write(root.join("inside.txt"), "ok").expect("write inside");
        fs::write(outside.join("secret.txt"), "secret").expect("write outside");
        let token = issue_path(&root).expect("issue path token");
        assert!(check_path(token, root.join("inside.txt"), PathAccess::Read).is_ok());
        assert!(check_path(token, outside.join("secret.txt"), PathAccess::Read).is_err());
        assert!(check_path(token, root.join("../outside/secret.txt"), PathAccess::Read).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("link")).expect("symlink");
            assert!(check_path(token, root.join("link/secret.txt"), PathAccess::Read).is_err());
        }
        forget(token);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn grant_narrows_wildcard_and_revoke_is_sticky() {
        let root = temp_dir("narrow");
        let other = temp_dir("other");
        let token = issue_unrestricted();
        assert!(check_path(token, other.join("x"), PathAccess::Read).is_ok());
        grant_path_read(token, &root).expect("narrow token");
        assert!(check_path(token, root.join("x"), PathAccess::Read).is_ok());
        assert!(check_path(token, root.join("x"), PathAccess::Write).is_err());
        assert!(check_path(token, other.join("x"), PathAccess::Read).is_err());
        revoke(token).expect("revoke token");
        assert!(matches!(
            check_path(token, root.join("x"), PathAccess::Read),
            Err(ScopeError::Revoked(_))
        ));
        forget(token);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(other);
    }

    #[test]
    fn endpoint_scope_matches_host_and_port() {
        let token = issue_endpoint("Example.COM", Some(443)).expect("issue endpoint token");
        assert!(check_endpoint(token, "example.com", 443).is_ok());
        assert!(check_endpoint(token, "example.com", 80).is_err());
        assert!(check_endpoint(token, "other.example.com", 443).is_err());
        forget(token);
    }

    #[test]
    fn env_scope_and_url_parser_fail_closed() {
        let token = issue_endpoint("example.com", Some(80)).expect("issue endpoint token");
        assert!(check_endpoint(token, "example.com", 80).is_ok());
        assert!(check_endpoint(token, "example.com", 443).is_err());
        // The C URL parser has the same endpoint policy and must not turn a
        // malformed port into the default port.
        let url = std::ffi::CString::new("http://example.com:not-a-port/").unwrap();
        assert_eq!(unsafe { mimi_capability_check_url(token, url.as_ptr()) }, 0);
        forget(token);

        let token = issue_unrestricted();
        grant_env(token, "MIMI_SCOPE_TEST").expect("grant env");
        assert!(check_env(token, "MIMI_SCOPE_TEST").is_ok());
        assert!(matches!(
            check_env(token, "OTHER_SCOPE_TEST"),
            Err(ScopeError::EnvDenied { .. })
        ));
        forget(token);
    }
}
