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

#[no_mangle]
pub extern "C" fn mimi_send(fd: i64, data: *const std::ffi::c_char, len: i64) -> i64 {
    if fd < 0 || data.is_null() {
        return -1;
    }
    // SAFETY: direct POSIX calls with validated file descriptor and non-null buffer.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        libc::send(fd_i32, data as *const std::ffi::c_void, len as usize, 0) as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_recv(fd: i64, buf_size: i64, out_len: *mut i64) -> *mut std::ffi::c_char {
    if fd < 0 || buf_size <= 0 {
        return std::ptr::null_mut();
    }
    let fd_i32 = match fd_to_i32(fd) {
        Some(v) => v,
        None => return std::ptr::null_mut(),
    };
    let size = buf_size as usize;
    let mut buf: Vec<u8> = vec![0u8; size + 1];
    // SAFETY: `buf` has `size + 1` allocated bytes; `fd_i32` is validated.
    let n = unsafe { libc::recv(fd_i32, buf.as_mut_ptr() as *mut std::ffi::c_void, size, 0) };
    if n <= 0 {
        if !out_len.is_null() {
            unsafe {
                // SAFETY: `out_len` was checked non-null above.
                *out_len = 0;
            }
        }
        return std::ptr::null_mut();
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

fn http_request(host: &str, port: u16, request: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::net::TcpStream;

    let addr = format!("{}:{}", host, port);
    let mut stream = TcpStream::connect(&addr).ok()?;
    // C5-fix: propagate timeout failure instead of silently ignoring
    if let Err(e) = stream.set_read_timeout(Some(std::time::Duration::from_secs(5))) {
        eprintln!("[mimi runtime] HTTP set_read_timeout failed: {}", e);
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
