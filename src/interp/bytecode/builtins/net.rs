//! Networking builtins: socket, connect, bind, listen, accept, send, recv, http_get, http_post.
//!
//! These are thin wrappers around libc socket calls with argument validation.

use crate::interp::bytecode::registry::{BuiltinCategory, BuiltinDesc, BuiltinRegistry};
use crate::interp::bytecode::vm::BytecodeVM;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(BuiltinDesc {
        name: "socket",
        arity: 3,
        category: BuiltinCategory::System,
        func: builtin_socket,
    });
    reg.register(BuiltinDesc {
        name: "connect",
        arity: 3,
        category: BuiltinCategory::System,
        func: builtin_connect,
    });
    reg.register(BuiltinDesc {
        name: "bind",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_bind,
    });
    reg.register(BuiltinDesc {
        name: "listen",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_listen,
    });
    reg.register(BuiltinDesc {
        name: "accept",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_accept,
    });
    reg.register(BuiltinDesc {
        name: "send",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_send,
    });
    reg.register(BuiltinDesc {
        name: "recv",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_recv,
    });
    reg.register(BuiltinDesc {
        name: "http_get",
        arity: 1,
        category: BuiltinCategory::System,
        func: builtin_http_get,
    });
    reg.register(BuiltinDesc {
        name: "http_post",
        arity: 2,
        category: BuiltinCategory::System,
        func: builtin_http_post,
    });
}

fn builtin_socket(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let domain = args[0]
        .as_int()
        .ok_or_else(|| InterpError::new("socket: domain must be i32"))? as i32;
    let type_ = args[1]
        .as_int()
        .ok_or_else(|| InterpError::new("socket: type must be i32"))? as i32;
    let protocol = args[2]
        .as_int()
        .ok_or_else(|| InterpError::new("socket: protocol must be i32"))? as i32;
    // SAFETY: libc::socket 标准调用；domain/type_/protocol 已从 Value::as_int 校验为 i32。
    let fd = unsafe { libc::socket(domain, type_, protocol) };
    if fd < 0 {
        return Err(InterpError::new(format!(
            "socket() failed: domain={}, type={}, protocol={} (OS error: {})",
            domain,
            type_,
            protocol,
            std::io::Error::last_os_error()
        )));
    }
    let reuse: libc::c_int = 1;
    // SAFETY: fd 为 socket() 返回的有效描述符；&reuse 指向栈上有效 c_int。
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &reuse as *const _ as *const libc::c_void,
            std::mem::size_of_val(&reuse) as libc::socklen_t,
        );
    }
    Ok(Value::Int(fd as i64))
}

fn builtin_connect(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let fd = args[0]
        .as_int()
        .ok_or_else(|| InterpError::new("connect: fd must be i32"))? as i32;
    // dup2(new_fd, fd) below silently closes and replaces any already-open fd.
    // Guard: fd must be a live socket we are allowed to take over. In
    // particular 0/1/2 (stdin/stdout/stderr) and arbitrary files must not be
    // silently hijacked (which would even redirect stdio).
    if fd <= 2 {
        return Err(InterpError::new(format!(
            "connect: fd={} would replace a standard stream (0/1/2); pass a socket() fd",
            fd
        )));
    }
    let mut so_type: libc::c_int = 0;
    let mut so_len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: fd 为 socket() 返回的有效描述符；&mut so_type 指向栈上变量，so_len 携带缓冲区大小。
    let is_socket = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            &mut so_type as *mut _ as *mut libc::c_void,
            &mut so_len,
        ) == 0
    };
    if !is_socket {
        return Err(InterpError::new(format!(
            "connect: fd={} is not an open socket; pass a socket() fd",
            fd
        )));
    }
    let host = args[1]
        .as_string()
        .ok_or_else(|| InterpError::new("connect: host must be string"))?;
    let port = args[2]
        .as_int()
        .ok_or_else(|| InterpError::new("connect: port must be i32"))?;
    if !(0..=65535).contains(&port) {
        return Err(InterpError::new("connect: port must be in range 0-65535"));
    }
    let c_host = std::ffi::CString::new(host)
        .map_err(|e| InterpError::new(format!("connect: invalid host: {}", e)))?;
    // SAFETY: zeroed 初始化 POD addrinfo，字段随后显式赋值。
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;
    let port_str = format!("{}", port);
    let c_port =
        std::ffi::CString::new(port_str).map_err(|_| InterpError::new("connect: invalid port"))?;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: CString::new 保证 NUL 结尾；res 由 getaddrinfo 分配，随后 freeaddrinfo 释放。
    let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
    if err != 0 || res.is_null() {
        return Err(InterpError::new(format!(
            "connect: getaddrinfo failed for '{}'",
            host
        )));
    }
    let mut ret = -1i64;
    let mut ai = res;
    while !ai.is_null() && ret != 0 {
        // SAFETY: ai 为 getaddrinfo 返回链表的有效节点；new_fd 来自 socket()，dup2/close 成对释放。
        unsafe {
            let new_fd = libc::socket((*ai).ai_family, (*ai).ai_socktype, (*ai).ai_protocol);
            if new_fd >= 0 {
                ret = libc::connect(new_fd, (*ai).ai_addr, (*ai).ai_addrlen) as i64;
                if ret == 0 {
                    let nodelay: libc::c_int = 1;
                    libc::setsockopt(
                        new_fd,
                        libc::IPPROTO_TCP,
                        libc::TCP_NODELAY,
                        &nodelay as *const _ as *const libc::c_void,
                        std::mem::size_of_val(&nodelay) as libc::socklen_t,
                    );
                    libc::dup2(new_fd, fd);
                    libc::close(new_fd);
                } else {
                    libc::close(new_fd);
                }
            }
            ai = (*ai).ai_next;
        }
    }
    // SAFETY: res 为 getaddrinfo 分配的有效指针，仅释放一次。
    unsafe { libc::freeaddrinfo(res) };
    if ret != 0 {
        return Err(InterpError::new(format!(
            "connect() failed for '{}:{}' (OS error: {})",
            host,
            port,
            std::io::Error::last_os_error()
        )));
    }
    Ok(Value::Int(0))
}

fn builtin_bind(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let fd = args[0]
        .as_int()
        .ok_or_else(|| InterpError::new("bind: fd must be i32"))? as i32;
    let port = args[1]
        .as_int()
        .ok_or_else(|| InterpError::new("bind: port must be i32"))?;
    // SAFETY: zeroed 初始化 POD sockaddr_in，字段随后显式赋值。
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = (port as u16).to_be();
    addr.sin_addr.s_addr = libc::INADDR_ANY;
    // SAFETY: fd 为 socket() 返回的有效描述符；&addr 指向栈上已初始化的 sockaddr_in。
    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        )
    };
    if ret < 0 {
        return Err(InterpError::new(format!(
            "bind() failed: fd={}, port={} (OS error: {})",
            fd,
            port,
            std::io::Error::last_os_error()
        )));
    }
    Ok(Value::Int(0))
}

fn builtin_listen(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let fd = args[0]
        .as_int()
        .ok_or_else(|| InterpError::new("listen: fd must be i32"))? as i32;
    let backlog = args[1]
        .as_int()
        .ok_or_else(|| InterpError::new("listen: backlog must be i32"))? as i32;
    // SAFETY: fd 为 socket() 返回的有效描述符；backlog 为已校验 i32。
    let ret = unsafe { libc::listen(fd, backlog) };
    if ret < 0 {
        return Err(InterpError::new(format!(
            "listen() failed: fd={}, backlog={} (OS error: {})",
            fd,
            backlog,
            std::io::Error::last_os_error()
        )));
    }
    Ok(Value::Int(0))
}

fn builtin_accept(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let fd = args[0]
        .as_int()
        .ok_or_else(|| InterpError::new("accept: fd must be i32"))? as i32;
    // SAFETY: zeroed 初始化 POD sockaddr_in；addr_len 携带缓冲区大小。
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut addr_len: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as u32;
    // SAFETY: fd 为 socket() 返回的有效描述符；&mut addr 指向栈上缓冲。
    let client_fd = unsafe {
        libc::accept(
            fd,
            &mut addr as *mut _ as *mut libc::sockaddr,
            &mut addr_len,
        )
    };
    if client_fd < 0 {
        return Err(InterpError::new(format!(
            "accept() failed: fd={} (OS error: {})",
            fd,
            std::io::Error::last_os_error()
        )));
    }
    Ok(Value::Int(client_fd as i64))
}

fn builtin_send(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let fd = args[0]
        .as_int()
        .ok_or_else(|| InterpError::new("send: fd must be i32"))? as i32;
    let data = args[1]
        .as_string()
        .ok_or_else(|| InterpError::new("send: data must be string"))?;
    // SAFETY: data.as_ptr() 指向 Rust String 数据，len 为有效长度；libc 按 len 字节读。
    let sent = unsafe { libc::send(fd, data.as_ptr() as *const libc::c_void, data.len(), 0) };
    if sent < 0 {
        return Err(InterpError::new(format!(
            "send() failed: fd={}, len={} (OS error: {})",
            fd,
            data.len(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(Value::Int(sent as i64))
}

fn builtin_recv(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let fd = args[0]
        .as_int()
        .ok_or_else(|| InterpError::new("recv: fd must be i32"))? as i32;
    let buf_size = args[1]
        .as_int()
        .ok_or_else(|| InterpError::new("recv: buf_size must be i32"))?;
    if buf_size <= 0 {
        return Err(InterpError::new("recv: buf_size must be positive"));
    }
    let mut buf: Vec<u8> = vec![0u8; buf_size as usize];
    // SAFETY: buf 为 vec![0; buf_size]，as_mut_ptr 指向其堆缓冲，长度与容量匹配。
    let n = unsafe {
        libc::recv(
            fd,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf_size as usize,
            0,
        )
    };
    if n < 0 {
        return Err(InterpError::new(format!(
            "recv() failed: fd={}, buf_size={} (OS error: {})",
            fd,
            buf_size,
            std::io::Error::last_os_error()
        )));
    }
    if n == 0 {
        return Ok(Value::String(String::new()));
    }
    let n = n as usize;
    buf.truncate(n);
    Ok(Value::String(String::from_utf8_lossy(&buf).to_string()))
}

// === HTTP builtins ===

fn http_connect(host: &str, port: i64) -> Result<i64, InterpError> {
    // SAFETY: libc::socket 标准调用；常量参数均为合法 i32。
    let domain = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if domain < 0 {
        return Err(InterpError::new("http: failed to create socket"));
    }
    let c_host = std::ffi::CString::new(host)
        .map_err(|e| InterpError::new(format!("http: invalid host: {}", e)))?;
    // SAFETY: zeroed 初始化 POD addrinfo，字段随后显式赋值。
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;
    let port_str = format!("{}", port);
    let c_port =
        std::ffi::CString::new(port_str).map_err(|_| InterpError::new("http: invalid port"))?;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: CString::new 保证 NUL 结尾；res 由 getaddrinfo 分配，随后 freeaddrinfo 释放。
    let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
    if err != 0 || res.is_null() {
        // SAFETY: domain 为 socket() 返回的有效描述符，close 释放一次。
        unsafe { libc::close(domain) };
        return Err(InterpError::new(format!(
            "http: could not resolve host '{}'",
            host
        )));
    }
    // SAFETY: (*res) 指向 getaddrinfo 返回链表的有效节点。
    let ret = unsafe { libc::connect(domain, (*res).ai_addr, (*res).ai_addrlen) };
    // SAFETY: res 为 getaddrinfo 分配的有效指针，仅释放一次。
    unsafe { libc::freeaddrinfo(res) };
    if ret < 0 {
        // SAFETY: 同上——domain 有效，close 释放一次。
        unsafe { libc::close(domain) };
        return Err(InterpError::new(format!(
            "http: connection refused to '{}:{}'",
            host, port
        )));
    }
    Ok(domain as i64)
}

fn send_all(fd: i32, buf: *const libc::c_void, len: usize) -> Result<(), InterpError> {
    let mut sent: isize = 0;
    while (sent as usize) < len {
        // SAFETY: buf 指向调用方提供的有效缓冲，len-sent 为剩余字节数；指针算术在分配内。
        let n = unsafe {
            libc::send(
                fd,
                (buf as *const u8).add(sent as usize) as *const libc::c_void,
                len - sent as usize,
                0,
            )
        };
        if n == 0 {
            return Err(InterpError::new(
                "send: connection closed while sending data",
            ));
        }
        if n < 0 {
            // SAFETY: libc::__errno_location 返回线程局部 errno 的有效非空指针。
            let err = unsafe { *libc::__errno_location() };
            if err == libc::EINTR {
                continue;
            }
            return Err(InterpError::new(format!("send error: {}", err)));
        }
        sent += n;
    }
    Ok(())
}

fn recv_all_into(fd: i32, result: &mut Vec<u8>) -> Result<(), InterpError> {
    let mut chunk = vec![0u8; 32768];
    loop {
        // SAFETY: chunk 为 vec![0u8; 32768]，指针与长度匹配。
        let n = unsafe { libc::recv(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len(), 0) };
        if n < 0 {
            // SAFETY: 同上——线程局部 errno。
            let err = unsafe { *libc::__errno_location() };
            if err == libc::EINTR {
                continue;
            }
            return Err(InterpError::new(format!("recv error: {}", err)));
        }
        if n == 0 {
            break;
        }
        result.extend_from_slice(&chunk[..n as usize]);
    }
    Ok(())
}

fn http_send_recv(fd: i64, request: &str) -> Result<String, InterpError> {
    let c_req = std::ffi::CString::new(request)
        .map_err(|e| InterpError::new(format!("http: invalid request: {}", e)))?;
    send_all(
        fd as i32,
        c_req.as_ptr() as *const libc::c_void,
        request.len(),
    )?;
    let mut buf = Vec::new();
    recv_all_into(fd as i32, &mut buf)?;
    // SAFETY: fd 为 socket() 返回的有效描述符，close 释放一次。
    unsafe { libc::close(fd as i32) };
    if buf.is_empty() {
        return Err(InterpError::new("http: empty response"));
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn validate_http_url(url: &str) -> Result<(), InterpError> {
    let lower = url.to_lowercase();
    if lower.starts_with("https://") {
        // TLS is not implemented; accepting https:// here would be a false
        // promise (the connection below would always fail or be misparsed).
        return Err(InterpError::new(
            "http_get/http_post: https:// is not supported (no TLS); use http://",
        ));
    }
    if lower.starts_with("http://") {
        return Ok(());
    }
    if lower.contains("://") {
        return Err(InterpError::new(
            "http_get/http_post: only http:// and https:// schemes are allowed",
        ));
    }
    Ok(())
}

/// Split a validated http:// URL into (host, port), enforcing the port range.
/// IPv6 literals use the [addr]:port form (bare "::1" would be misparsed by
/// the ':' split) — same rules as the runtime `parse_http_url`.
fn http_split_host_port(host: &str) -> Result<(&str, i64), InterpError> {
    if host.contains('[') || host.contains(']') {
        // IPv6 literal: [addr] or [addr]:port
        let close = host
            .find(']')
            .ok_or_else(|| InterpError::new("http_get/http_post: unterminated '[' in host"))?;
        if !host.starts_with('[') || host[..close].contains('[') {
            return Err(InterpError::new(
                "http_get/http_post: invalid IPv6 host literal",
            ));
        }
        let addr = &host[1..close];
        if addr.is_empty() {
            return Err(InterpError::new(
                "http_get/http_post: empty IPv6 host literal",
            ));
        }
        let after = &host[close + 1..];
        let port = if after.is_empty() {
            80
        } else if let Some(p) = after.strip_prefix(':') {
            p.parse()
                .map_err(|_| InterpError::new("http_get/http_post: invalid port"))?
        } else {
            return Err(InterpError::new(
                "http_get/http_post: invalid IPv6 host suffix (expected [addr]:port)",
            ));
        };
        if !(0..=65535).contains(&port) {
            return Err(InterpError::new(
                "http_get/http_post: port must be in range 0-65535",
            ));
        }
        return Ok((addr, port));
    }
    if host.matches(':').count() > 1 {
        return Err(InterpError::new(
            "http_get/http_post: bare IPv6 hosts are not supported (use [addr]:port form)",
        ));
    }
    let (host, port) = if let Some((h, p)) = host.split_once(':') {
        let port: i64 = p
            .parse()
            .map_err(|_| InterpError::new("http_get/http_post: invalid port"))?;
        if !(0..=65535).contains(&port) {
            return Err(InterpError::new(
                "http_get/http_post: port must be in range 0-65535",
            ));
        }
        (h, port)
    } else {
        (host, 80)
    };
    if host.is_empty() {
        return Err(InterpError::new("http_get/http_post: empty host"));
    }
    Ok((host, port))
}

fn validate_host_ssrf(host: &str) -> Result<(), InterpError> {
    let blocked_hosts = [
        "localhost",
        "127.0.0.1",
        "0.0.0.0",
        "::1",
        "metadata.google.internal",
    ];
    if blocked_hosts.contains(&host) {
        return Err(InterpError::new(
            "http_get/http_post: SSRF protection — loopback addresses are blocked",
        ));
    }
    let private_prefixes = [
        "127.", "10.", "172.16.", "172.17.", "172.18.", "172.19.", "172.20.", "172.21.", "172.22.",
        "172.23.", "172.24.", "172.25.", "172.26.", "172.27.", "172.28.", "172.29.", "172.30.",
        "172.31.", "192.168.", "169.254.", "::1", "fc", "fd",
    ];
    if private_prefixes.iter().any(|p| host.starts_with(p)) {
        return Err(InterpError::new(
            "http_get/http_post: SSRF protection — private/internal addresses are blocked",
        ));
    }
    Ok(())
}

fn builtin_http_get(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let url = args[0]
        .as_string()
        .ok_or_else(|| InterpError::new("http_get: url must be string"))?;
    validate_http_url(url)?;
    let url = url.trim_start_matches("http://");
    let (host, rest) = url.split_once('/').unwrap_or((url, ""));
    let path = if rest.is_empty() {
        "/"
    } else {
        &format!("/{}", rest)
    };
    let (host, port) = http_split_host_port(host)?;
    validate_host_ssrf(host)?;
    let fd = http_connect(host, port)?;
    let request = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, host
    );
    let response = http_send_recv(fd, &request)?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&response);
    Ok(Value::String(body.to_string()))
}

fn builtin_http_post(_vm: &mut BytecodeVM, args: &[Value]) -> Result<Value, InterpError> {
    let url = args[0]
        .as_string()
        .ok_or_else(|| InterpError::new("http_post: url must be string"))?;
    let body = args[1]
        .as_string()
        .ok_or_else(|| InterpError::new("http_post: body must be string"))?;
    validate_http_url(url)?;
    let url = url.trim_start_matches("http://");
    let (host, rest) = url.split_once('/').unwrap_or((url, ""));
    let path = if rest.is_empty() {
        "/"
    } else {
        &format!("/{}", rest)
    };
    let (host, port) = http_split_host_port(host)?;
    validate_host_ssrf(host)?;
    let fd = http_connect(host, port)?;
    let request = format!(
        "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n{}",
        path, host, body.len(), body
    );
    let response = http_send_recv(fd, &request)?;
    let res_body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&response);
    Ok(Value::String(res_body.to_string()))
}
