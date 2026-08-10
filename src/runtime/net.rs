// ===========================================================================
// Network / Socket + HTTP client (extracted from runtime/mod.rs)
//
// TCP socket primitives (mimi_socket/connect/bind/listen/accept/send/recv/
// close) built directly on libc, plus a minimal blocking HTTP client
// (mimi_http_get / mimi_http_post). Mirrors stdlib `net.mimi`.
// ===========================================================================

#[cfg(standalone)]
use super::libc;
use super::{alloc_c_string, alloc_c_string_from_bytes, cstr_to_string};
use std::ffi::CString;

// ---------------------------------------------------------------------------
// Network / Socket
// ---------------------------------------------------------------------------

/// Safely convert i64 fd to i32, returning None if out of range.
fn fd_to_i32(fd: i64) -> Option<i32> {
    if fd < 0 || fd > i32::MAX as i64 {
        None
    } else {
        Some(fd as i32)
    }
}

#[no_mangle]
pub extern "C" fn mimi_socket(domain: i64, type_: i64, protocol: i64) -> i64 {
    // H13 fix: validate domain/type/protocol fit in i32 range before truncation.
    let domain_i32 = match i32::try_from(domain) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    let type_i32 = match i32::try_from(type_) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    let protocol_i32 = match i32::try_from(protocol) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    unsafe {
        let fd = libc::socket(domain_i32, type_i32, protocol_i32);
        if fd >= 0 {
            let reuse: i32 = 1;
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &reuse as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as libc::socklen_t,
            );
        }
        fd as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_connect(fd: i64, host: *const std::ffi::c_char, port: i64) -> i64 {
    if host.is_null() || fd < 0 {
        return -1;
    }
    // SAFETY: `host` was checked non-null above.
    let h = unsafe { cstr_to_string(host) };

    // Resolve address
    let port_str = format!("{}", port);
    // SAFETY: `addrinfo` is zero-initialized before passing to `getaddrinfo`.
    let hints = unsafe {
        let mut hints_raw: libc::addrinfo = std::mem::zeroed();
        hints_raw.ai_family = libc::AF_UNSPEC;
        hints_raw.ai_socktype = libc::SOCK_STREAM;
        hints_raw
    };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let c_host = CString::new(h.as_str()).unwrap_or_default();
    let c_port = CString::new(port_str.as_str()).unwrap_or_default();
    // SAFETY: `c_host` and `c_port` are valid NUL-terminated `CString`s; `res` is out-param.
    let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
    if err != 0 || res.is_null() {
        return -1;
    }

    // SAFETY: freeing a non-null pointer allocated by the matching allocator.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => {
                libc::freeaddrinfo(res);
                return -1;
            }
        };
        // SAFETY: `res` is non-null and came from `getaddrinfo`; `fd_i32` is validated.
        let r = libc::connect(fd_i32, (*res).ai_addr, (*res).ai_addrlen);
        if r == 0 {
            let flag: i32 = 1;
            // SAFETY: `fd_i32` is a valid socket file descriptor.
            libc::setsockopt(
                fd_i32,
                libc::IPPROTO_TCP,
                libc::TCP_NODELAY,
                &flag as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as libc::socklen_t,
            );
        }
        libc::freeaddrinfo(res);
        r as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_bind(fd: i64, port: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // H13 fix: validate port fits in u16 range before truncation.
    let port_u16 = match u16::try_from(port) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = port_u16.to_be();
        addr.sin_addr.s_addr = libc::INADDR_ANY;
        libc::bind(
            fd_i32,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        ) as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_listen(fd: i64, backlog: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // SAFETY: direct POSIX calls with a validated file descriptor.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        libc::listen(fd_i32, backlog as i32) as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_accept(fd: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // SAFETY: direct POSIX calls with a validated file descriptor.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        let mut addr_len: libc::socklen_t =
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let client_fd = libc::accept(
            fd_i32,
            &mut addr as *mut _ as *mut libc::sockaddr,
            &mut addr_len,
        );
        client_fd as i64
    }
}

/// Maximum `mimi_send` byte count. Mirrors `MAX_RECV_SIZE`: an absurd `len`
/// is rejected instead of handed to `send(2)` (audit 2026-08-05, H-28).
const MAX_SEND_SIZE: i64 = 100 * 1024 * 1024; // 100MB

#[no_mangle]
pub extern "C" fn mimi_send(fd: i64, data: *const std::ffi::c_char, len: i64) -> i64 {
    // Audit fix (H-28, SECURITY): validate `len` BEFORE the usize cast and
    // before any fd use. A negative len wrapped `len as usize` to ~2^64-1,
    // making send(2) read out of bounds past `data` — a page fault (SIGSEGV)
    // or a HEAP MEMORY LEAK TO THE PEER for whatever bytes mapped. mimi_recv
    // already had the symmetric MAX_RECV_SIZE hardening; send was missed.
    // Compared as i64 so it is correct on 32-bit too.
    if len < 0 {
        return -1;
    }
    if len == 0 {
        return 0; // nothing to send; fd untouched (mimi_recv parity)
    }
    if len > MAX_SEND_SIZE {
        return -1;
    }
    if fd < 0 || data.is_null() {
        return -1;
    }
    // SAFETY: `len` is bounded to [1, MAX_SEND_SIZE] above, so the cast is
    // lossless; fd and buffer preconditions checked.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        libc::send(fd_i32, data as *const std::ffi::c_void, len as usize, 0) as i64
    }
}

/// Maximum `mimi_recv` buffer size. Mirrors `MAX_HTTP_RESPONSE` below:
/// an uncapped `buf_size` (e.g. `i64::MAX`) made `vec![0u8; size + 1]`
/// panic with a capacity overflow across the FFI boundary
/// (2026-08-05 full audit, HIGH).
const MAX_RECV_SIZE: usize = 100 * 1024 * 1024; // 100MB

#[no_mangle]
pub extern "C" fn mimi_recv(fd: i64, buf_size: i64, out_len: *mut i64) -> *mut std::ffi::c_char {
    // Audit fix: validate the size BEFORE any fd use so an absurd buf_size
    // returns null gracefully (no capacity-overflow panic across FFI, and no
    // fd is touched at all). Compared as i64 so it is correct on 32-bit too.
    if buf_size <= 0 || buf_size > MAX_RECV_SIZE as i64 {
        return std::ptr::null_mut();
    }
    if fd < 0 {
        return std::ptr::null_mut();
    }
    let fd_i32 = match fd_to_i32(fd) {
        Some(v) => v,
        None => return std::ptr::null_mut(),
    };
    // SAFETY: buf_size is bounded to [1, MAX_RECV_SIZE] above, so this cast
    // is lossless and `size + 1` cannot overflow.
    let size = buf_size as usize;
    let mut buf: Vec<u8> = vec![0u8; size + 1];
    // SAFETY: `buf` has `size + 1` allocated bytes; `fd_i32` is validated.
    let n = unsafe { libc::recv(fd_i32, buf.as_mut_ptr() as *mut std::ffi::c_void, size, 0) };
    if n < 0 {
        // Real error (ECONNRESET, EAGAIN-after-timeout, ...). Return NULL so
        // the codegen side can fail loud (compile_recv NULL check → trap).
        if !out_len.is_null() {
            unsafe {
                // SAFETY: `out_len` was checked non-null above.
                *out_len = 0;
            }
        }
        return std::ptr::null_mut();
    }
    if n == 0 {
        // EOF (peer closed the connection). The stdlib contract
        // (std/net.mimi recv) is: "" on EOF, hard failures abort. EOF is a
        // SUCCESSFUL read of zero bytes — return a non-NULL empty string so
        // the codegen side surfaces Ok("") instead of trapping on NULL.
        if !out_len.is_null() {
            unsafe {
                // SAFETY: `out_len` was checked non-null above.
                *out_len = 0;
            }
        }
        return alloc_c_string("");
    }
    // S8: Clamp n to buffer size to prevent out-of-bounds write.
    let n = (n as usize).min(size);
    buf[n] = 0;
    if !out_len.is_null() {
        unsafe {
            // SAFETY: `out_len` was checked non-null above.
            *out_len = n as i64;
        }
    }
    alloc_c_string_from_bytes(&buf[..=n as usize])
}

#[no_mangle]
pub extern "C" fn mimi_close(fd: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // 0.35.29 (H13): fd <= 2 (standard streams) must not be closable from
    // user code — closing 0/1/2 hijacks the interpreter/compiled-process
    // stdio. Mirrors the connect guard at mimi_connect (fd <= 2 rejected);
    // arbitrary files/sockets (>2) remain closable.
    if fd <= 2 {
        return -1;
    }
    // SAFETY: direct POSIX close with a validated file descriptor.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        libc::close(fd_i32) as i64
    }
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    // M20: explicitly reject HTTPS (no TLS support in this runtime).
    if url.starts_with("https://") {
        return None;
    }
    let rest = url.strip_prefix("http://")?;

    let (host_part, path_part) = if let Some(slash_idx) = rest.find('/') {
        let (h, p) = rest.split_at(slash_idx);
        (h, p)
    } else {
        (rest, "/")
    };

    let (host, port) = if host_part.starts_with('[') {
        // IPv6: [addr] or [addr]:port
        let close_bracket = host_part.find(']')?;
        let addr = &host_part[1..close_bracket];
        let after = &host_part[close_bracket + 1..];
        if after.is_empty() {
            (format!("[{}]", addr), 80u16)
        } else {
            let port_str = after.strip_prefix(':')?;
            let port: u16 = port_str.parse().ok()?;
            (format!("[{}]", addr), port)
        }
    } else if let Some(colon_idx) = host_part.find(':') {
        let port_str = &host_part[colon_idx + 1..];
        let port: u16 = port_str.parse().ok()?;
        let h = &host_part[..colon_idx];
        (h.to_string(), port)
    } else {
        (host_part.to_string(), 80u16)
    };

    Some((host, port, path_part.to_string()))
}

/// SSRF protection for the native (codegen) HTTP client. Mirrors the bytecode
/// VM's validate_host_ssrf (interp/bytecode/builtins/net.rs) — the VM blocks
/// loopback/private/internal addresses; the native runtime must too, or a
/// codegen-built program can reach 127.0.0.1/169.254.169.254 while the VM
/// cannot (L1 divergence + a genuine SSRF hole in the native backend).
/// Returns None when the host is blocked.
///
/// 0.35.29 H4: `parse_http_url` hands us the host WITH its IPv6 brackets
/// (`[::1]`), so the guard must strip them before matching — the old check
/// matched the raw string and every `[v6]` literal sailed through. Also
/// decodes inet_aton-style numeric IPv4 literals (2130706433 / 0x7f000001 /
/// 017700000001 / 127.1) which getaddrinfo resolves but a string-prefix
/// check cannot see.
fn ssrf_validate_host(host: &str) -> Option<()> {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    let blocked_hosts = [
        "localhost",
        "127.0.0.1",
        "0.0.0.0",
        "::1",
        "metadata.google.internal",
    ];
    if blocked_hosts.contains(&h) {
        return None;
    }
    let private_prefixes = [
        "127.", "10.", "172.16.", "172.17.", "172.18.", "172.19.", "172.20.", "172.21.", "172.22.",
        "172.23.", "172.24.", "172.25.", "172.26.", "172.27.", "172.28.", "172.29.", "172.30.",
        "172.31.", "192.168.", "169.254.", "::1", "fc", "fd", "fe8", "fe9", "fea", "feb",
    ];
    if private_prefixes.iter().any(|p| h.starts_with(p)) {
        return None;
    }
    // Numeric IPv4 literals that getaddrinfo resolves but a prefix match
    // cannot see (inet_aton compatibility: decimal/hex/octal integers and
    // 1-4 dotted parts). 127.0.0.1 == 2130706433 == 0x7f000001 ==
    // 017700000001 == 127.1.
    if let Some(v4) = decode_ipv4_literal(h) {
        if is_private_ipv4(v4) {
            return None;
        }
    }
    // IPv4-mapped IPv6 (::ffff:127.0.0.1) routes straight to the v4 address
    // — must not fall through the prefix check on the v6 side.
    if let Some(suffix) = h.strip_prefix("::ffff:") {
        if let Some(v4) = decode_ipv4_literal(suffix) {
            if is_private_ipv4(v4) {
                return None;
            }
        }
    }
    Some(())
}

/// Decode an inet_aton-compatible IPv4 literal to its 32-bit value, or None
/// if `s` is not a numeric literal (hostname, IPv6, etc.). Supported forms:
/// single decimal/hex/octal integer (a 32-bit value), and 1-4 dotted parts
/// where each part is decimal or octal (0-prefixed); 3-part forms treat the
/// last part as 16-bit, 2-part forms as 24-bit — exactly what glibc's
/// getaddrinfo/inet_aton accept.
fn decode_ipv4_literal(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 45 {
        return None;
    }
    // Single integer (whole 32-bit value).
    if !s.contains('.') {
        if s.len() > 1 && s.starts_with('0') {
            // Octal (leading 0, digits 0-7); malformed → not a literal.
            if !s.starts_with("0x") && !s.starts_with("0X") {
                if !s.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
                    return None;
                }
                return u32::from_str_radix(s, 8).ok();
            }
        }
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            return u32::from_str_radix(hex, 16).ok();
        }
        if s.bytes().all(|b| b.is_ascii_digit()) {
            return s.parse::<u32>().ok();
        }
        return None;
    }
    // Dotted parts. Each part is decimal, or octal when 0-prefixed
    // (inet_aton: 0177.0.0.1 == 127.0.0.1).
    fn part(p: &str) -> Option<u32> {
        if p.is_empty() || p.len() > 4 {
            return None;
        }
        if let Some(hex) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
            if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            return u32::from_str_radix(hex, 16).ok();
        }
        if p.len() > 1 && p.starts_with('0') {
            if !p.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
                return None;
            }
            return u32::from_str_radix(p, 8).ok();
        }
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse::<u32>().ok()
    }
    let parts: Vec<&str> = s.split('.').collect();
    match parts.len() {
        4 => {
            let mut v: u32 = 0;
            for p in &parts {
                let n = part(p)?;
                if n > 255 {
                    return None;
                }
                v = (v << 8) | n;
            }
            Some(v)
        }
        // inet_aton short forms.
        3 => {
            let a = part(parts[0])?;
            let b = part(parts[1])?;
            let c = part(parts[2])?;
            if a > 255 || b > 255 || c > 65535 {
                return None;
            }
            Some((a << 24) | (b << 16) | c)
        }
        2 => {
            let a = part(parts[0])?;
            let b = part(parts[1])?;
            if a > 255 || b > 0x00ff_ffff {
                return None;
            }
            Some((a << 24) | b)
        }
        1 => part(parts[0]),
        _ => None,
    }
}

/// True when `v4` is loopback (127/8), private (10/8, 172.16/12, 192.168/16),
/// link-local (169.254/16), or the unspecified address (0.0.0.0) — the same
/// address families the string-prefix check blocks.
fn is_private_ipv4(v4: u32) -> bool {
    let first = v4 >> 24;
    if first == 127 || first == 10 || first == 0 {
        return true;
    }
    if (v4 >> 16) == 0xc0a8 {
        // 192.168.0.0/16
        return true;
    }
    if (v4 >> 16) == 0xa9fe {
        // 169.254.0.0/16
        return true;
    }
    // 172.16.0.0/12 — every address in 172.16.0.0-172.31.255.255 shifts to
    // exactly 0xac1 when >> 20 (the range is 16 contiguous /16 blocks, so a
    // <= upper bound would wrongly admit 172.32+).
    (v4 >> 20) == 0xac1
}

fn http_request(host: &str, port: u16, request: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::net::TcpStream;

    // SSRF protection (2026-08-05 Wave-2): the bytecode VM rejects loopback
    // and private/internal addresses for http_get/http_post (validate_host_ssrf
    // in interp/bytecode/builtins/net.rs). The runtime entry points (mimi_http_get
    // / mimi_http_post) had NO equivalent guard — codegen-compiled programs could
    // fetch loopback/cloud-metadata/private hosts while VM programs could not:
    // a security-relevant dual-backend divergence (P-0: VM is reference).
    // Centralize here so every HTTP request is guarded once (0.35.29 H4:
    // single source of truth — the old duplicate inline check disagreed with
    // validate_ssrf and neither stripped IPv6 brackets).
    if ssrf_validate_host(host).is_none() {
        eprintln!(
            "[mimi runtime] http_get/http_post: SSRF protection — loopback/private addresses are blocked"
        );
        return None;
    }

    // Audit 2026-08-05 (N-2): connect timeout + write timeout. The old
    // client had NEITHER — `TcpStream::connect` against a packet-dropping
    // firewall blocked ~2 minutes on OS SYN retries before the post-connect
    // 5s read timeout even existed. Resolve the address(es) and try each
    // with an explicit connect timeout (std handles v4/v6, multi-homed
    // hosts, and the nonblocking-connect/select dance internally).
    const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    const HTTP_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    let addr = format!("{}:{}", host, port);
    use std::net::ToSocketAddrs;
    let addrs = match addr.to_socket_addrs() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[mimi runtime] HTTP resolve failed for {}: {}", addr, e);
            return None;
        }
    };
    let mut stream: Option<TcpStream> = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, HTTP_CONNECT_TIMEOUT) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => {
                // Try the next resolved address (multi-homed / v6-vs-v4).
                eprintln!("[mimi runtime] HTTP connect to {} failed: {}", a, e);
            }
        }
    }
    let mut stream = match stream {
        Some(s) => s,
        None => return None,
    };
    // C5-fix: propagate timeout failure instead of silently ignoring
    if let Err(e) = stream.set_read_timeout(Some(std::time::Duration::from_secs(5))) {
        eprintln!("[mimi runtime] HTTP set_read_timeout failed: {}", e);
        return None;
    }
    // N-2: write timeout (the old client had none — a connected-but-stalled
    // peer blocked write_all indefinitely).
    if let Err(e) = stream.set_write_timeout(Some(HTTP_WRITE_TIMEOUT)) {
        eprintln!("[mimi runtime] HTTP set_write_timeout failed: {}", e);
        return None;
    }

    // Send request
    use std::io::Write;
    if let Err(e) = stream.write_all(request.as_bytes()) {
        eprintln!("[mimi runtime] HTTP write error: {}", e);
        return None;
    }

    // Read response
    // M27: limit total response size to prevent OOM from malicious server.
    const MAX_HTTP_RESPONSE: usize = 100 * 1024 * 1024; // 100MB
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if response.len() + n > MAX_HTTP_RESPONSE {
                    return None;
                }
                response.extend_from_slice(&buf[..n]);
            }
            Err(_) => break,
        }
    }

    if response.is_empty() {
        return None;
    }

    // Strip HTTP headers
    let body_start = if let Some(pos) = response.windows(4).position(|w| w == b"\r\n\r\n") {
        pos + 4
    } else if let Some(pos) = response.windows(2).position(|w| w == b"\n\n") {
        pos + 2
    } else {
        return None;
    };

    Some(response[body_start..].to_vec())
}

#[no_mangle]
pub extern "C" fn mimi_http_get(url: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if url.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `url` was checked non-null above.
    let u = unsafe { cstr_to_string(url) };
    let (host, port, path) = match parse_http_url(&u) {
        Some(v) => v,
        None => {
            // M20: HTTPS URLs are unsupported; log and return null.
            #[cfg(debug_assertions)]
            if u.starts_with("https://") {
                eprintln!("[mimi runtime] HTTPS not supported (no TLS), use http://");
            }
            return std::ptr::null_mut();
        }
    };

    let request = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, host
    );

    match http_request(&host, port, &request) {
        Some(body) => {
            let s = String::from_utf8_lossy(&body).into_owned();
            alloc_c_string(&s)
        }
        // audit (MEDIUM): return null on error so callers can distinguish
        // failure from a legitimate empty response body.
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn mimi_http_post(
    url: *const std::ffi::c_char,
    body: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if url.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `url` was checked non-null above.
    let u = unsafe { cstr_to_string(url) };
    let b = if body.is_null() {
        String::new()
    } else {
        // SAFETY: `body` was checked non-null above.
        unsafe { cstr_to_string(body) }
    };
    let (host, port, path) = match parse_http_url(&u) {
        Some(v) => v,
        None => {
            // M20: HTTPS URLs are unsupported; log and return null.
            #[cfg(debug_assertions)]
            if u.starts_with("https://") {
                eprintln!("[mimi runtime] HTTPS not supported (no TLS), use http://");
            }
            return std::ptr::null_mut();
        }
    };

    // SSRF: block loopback/private/internal hosts (VM parity; single guard
    // centralized in http_request — this pre-check gives http_post the same
    // loud-early failure http_get already has).
    if ssrf_validate_host(&host).is_none() {
        eprintln!(
            "[mimi runtime] http_post: SSRF protection — address blocked: {}",
            host
        );
        return std::ptr::null_mut();
    }
    let request = format!(
        "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path, host, b.len(), b
    );

    match http_request(&host, port, &request) {
        Some(body) => {
            let s = String::from_utf8_lossy(&body).into_owned();
            alloc_c_string(&s)
        }
        // audit (MEDIUM): return null on error so callers can distinguish
        // failure from a legitimate empty response body.
        None => std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for the 2026-08-05 audit fix (HIGH): `mimi_recv`
    //! must cap `buf_size` instead of panicking with a capacity overflow
    //! across the FFI boundary.

    use super::*;

    #[test]
    fn recv_absurd_buf_size_returns_null_without_panic() {
        // Size check is structurally first: `fd` is a plausible-but-unopened
        // descriptor and must never be touched for an out-of-range size.
        let mut out_len: i64 = 12345;
        let p = mimi_recv(999_999, i64::MAX, &mut out_len);
        assert!(p.is_null());
        assert_eq!(out_len, 12345); // untouched by the early return

        // One byte over the cap is rejected too.
        let p = mimi_recv(999_999, MAX_RECV_SIZE as i64 + 1, &mut out_len);
        assert!(p.is_null());
        assert_eq!(out_len, 12345);
    }

    #[test]
    fn recv_invalid_size_or_fd_returns_null() {
        assert!(mimi_recv(999_999, 0, std::ptr::null_mut()).is_null());
        assert!(mimi_recv(999_999, -1, std::ptr::null_mut()).is_null());
        assert!(mimi_recv(-1, 64, std::ptr::null_mut()).is_null());
    }

    // ── 0.35.29 H4: SSRF guard must strip IPv6 brackets and decode numeric
    // ── IPv4 literals (inet_aton family) — parse_http_url hands the guard
    // ── `[::1]`-style hosts, and getaddrinfo resolves 2130706433 to
    // ── 127.0.0.1, both invisible to the old raw string-prefix match.

    #[test]
    fn ssrf_blocks_bracketed_ipv6_loopback_and_ula() {
        // The H4 core: parse_http_url returns the host WITH its brackets.
        assert!(
            ssrf_validate_host("[::1]").is_none(),
            "[::1] must be blocked"
        );
        assert!(ssrf_validate_host("[::1]:80").is_none());
        assert!(
            ssrf_validate_host("[fc00::1]").is_none(),
            "ULA must be blocked"
        );
        assert!(
            ssrf_validate_host("[fd00::1]").is_none(),
            "ULA must be blocked"
        );
        assert!(ssrf_validate_host("[fe80::1]").is_none()); // link-local prefix
        assert!(ssrf_validate_host("::1").is_none());
        // A public IPv6 literal stays allowed.
        assert!(ssrf_validate_host("[2001:db8::1]").is_some());
    }

    #[test]
    fn ssrf_blocks_numeric_ipv4_literals() {
        // inet_aton family: all of these resolve to 127.0.0.1.
        assert!(
            ssrf_validate_host("2130706433").is_none(),
            "decimal int must be blocked"
        );
        assert!(
            ssrf_validate_host("0x7f000001").is_none(),
            "hex int must be blocked"
        );
        assert!(
            ssrf_validate_host("017700000001").is_none(),
            "octal int must be blocked"
        );
        assert!(
            ssrf_validate_host("127.1").is_none(),
            "2-part short form must be blocked"
        );
        assert!(
            ssrf_validate_host("0177.0.0.1").is_none(),
            "octal dotted quad must be blocked"
        );
        assert!(
            ssrf_validate_host("0x7f.0.0.1").is_none(),
            "hex dotted part must be blocked"
        );
        // Private/link-local families.
        assert!(ssrf_validate_host("10.0.0.5").is_none());
        assert!(ssrf_validate_host("172.16.0.1").is_none());
        assert!(ssrf_validate_host("192.168.1.1").is_none());
        assert!(ssrf_validate_host("169.254.169.254").is_none());
        assert!(ssrf_validate_host("0.0.0.0").is_none());
        // IPv4-mapped IPv6.
        assert!(
            ssrf_validate_host("[::ffff:127.0.0.1]").is_none(),
            "v4-mapped loopback must be blocked"
        );
        assert!(ssrf_validate_host("::ffff:10.0.0.5").is_none());
        // Public addresses still pass.
        assert!(ssrf_validate_host("8.8.8.8").is_some());
        assert!(
            ssrf_validate_host("172.32.0.1").is_some(),
            "outside 172.16/12 is public"
        );
        assert!(ssrf_validate_host("example.com").is_some());
        assert!(
            ssrf_validate_host("134744072").is_some(),
            "134744072 = 8.8.8.8 (public int form)"
        );
        assert!(
            ssrf_validate_host("2130706434").is_none(),
            "2130706434 = 0x7f000002, still loopback"
        );
    }

    #[test]
    fn http_url_ssrf_guard_blocks_loopback_variants() {
        // End-to-end through the URL parse path: http://[::1]/ must not reach
        // http_request with a bracketed host that slips the guard.
        let u = "http://[::1]/";
        let (host, _, _) = parse_http_url(u).unwrap();
        assert_eq!(host, "[::1]");
        assert!(ssrf_validate_host(&host).is_none());
        let u = "http://2130706433/x";
        let (host, _, _) = parse_http_url(u).unwrap();
        assert_eq!(host, "2130706433");
        assert!(ssrf_validate_host(&host).is_none());
    }
}
