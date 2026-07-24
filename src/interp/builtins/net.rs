use super::*;

impl<'a> Interpreter<'a> {
    // === Network builtins (interpreter implementations via libc) ===
    //
    // SAFETY: All network builtins call libc functions that are safe per POSIX when
    // given valid arguments. The Mimi interpreter validates argument types and ranges
    // before calling; return values are checked for error codes (typically -1). These
    // are FFI calls, not memory-safety critical — the worst outcome of a bug here is
    // a failed network operation, not memory corruption.
    pub(crate) fn builtin_socket(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 {
            return Err(InterpError::new(
                "socket expects 3 arguments (domain, type, protocol)",
            ));
        }
        let domain = match &args[0] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("socket: domain must be i32")),
        };
        let type_ = match &args[1] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("socket: type must be i32")),
        };
        let protocol = match &args[2] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("socket: protocol must be i32")),
        };
        // SAFETY: libc::socket is safe per POSIX when arguments are valid integers;
        // we validate types above. Returns -1 on error.
        let fd = unsafe { libc::socket(domain as i32, type_ as i32, protocol as i32) };
        // 0.31.22 Builtins 废除 sentinel：-1 拦截在 builtin 定义处
        if fd < 0 {
            return Err(InterpError::new(format!(
                "socket() failed: domain={}, type={}, protocol={} (OS error: {})",
                domain,
                type_,
                protocol,
                std::io::Error::last_os_error()
            )));
        }
        // Set SO_REUSEADDR so bind works immediately after close (TIME_WAIT avoidance)
        let reuse: libc::c_int = 1;
        // SAFETY: fd is valid and option value pointer/size are correct.
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

    pub(crate) fn builtin_connect(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 {
            return Err(InterpError::new(
                "connect expects 3 arguments (fd, host, port)",
            ));
        }
        let fd = match &args[0] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("connect: fd must be i32")),
        };
        let host = match &args[1] {
            Value::String(s) => s.clone(),
            _ => return Err(InterpError::new("connect: host must be string")),
        };
        let port = match &args[2] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("connect: port must be i32")),
        };
        let c_host = std::ffi::CString::new(host.as_str())
            .map_err(|e| InterpError::new(format!("connect: invalid host: {}", e)))?;
        // SAFETY: zeroed() is safe for POD structs like addrinfo/sockaddr_in.
        // getaddrinfo allocates memory that we check for null before use.
        // connect uses validated fd and the res pointer we receive from getaddrinfo.
        // freeaddrinfo frees memory allocated by getaddrinfo — safe as long as res is
        // non-null and was returned by getaddrinfo (both checked above).
        // SAFETY: zeroed() is safe for POD C structs.
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_family = libc::AF_UNSPEC;
        hints.ai_socktype = libc::SOCK_STREAM;
        let port_str = format!("{}", port);
        let c_port = std::ffi::CString::new(port_str)
            .map_err(|_| InterpError::new("connect: invalid port"))?;
        let mut res: *mut libc::addrinfo = std::ptr::null_mut();
        // SAFETY: input pointers come from valid CStrings; output is checked for null.
        let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
        if err != 0 || res.is_null() {
            return Err(InterpError::new(format!(
                "connect: getaddrinfo failed for '{}'",
                host
            )));
        }
        // Iterate through addrinfo results to support IPv4/IPv6 fallback
        let mut ret = -1i64;
        let mut ai = res;
        while !ai.is_null() && ret != 0 {
            // SAFETY: ai is a valid addrinfo from getaddrinfo linked list.
            unsafe {
                // Create a new socket for this address family
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
                        // Replace fd with the successfully connected one
                        libc::dup2(new_fd, fd as i32);
                        libc::close(new_fd);
                    } else {
                        libc::close(new_fd);
                    }
                }
                ai = (*ai).ai_next;
            }
        }
        // SAFETY: res is non-null and was returned by getaddrinfo, so freeaddrinfo is safe.
        unsafe { libc::freeaddrinfo(res) };
        // 0.31.22 Builtins 废除 sentinel：-1 拦截在 builtin 定义处
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

    pub(crate) fn builtin_bind(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("bind expects 2 arguments (fd, port)"));
        }
        let fd = match &args[0] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("bind: fd must be i32")),
        };
        let port = match &args[1] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("bind: port must be i32")),
        };
        // SAFETY: sockaddr_in is a POD struct; zeroed() is safe. bind() uses the validated
        // fd and a properly initialized sockaddr_in structure.
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = (port as u16).to_be();
        addr.sin_addr.s_addr = libc::INADDR_ANY;
        // SAFETY: fd is valid and sockaddr_in is initialized.
        let ret = unsafe {
            libc::bind(
                fd as i32,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as u32,
            )
        };
        // 0.31.22 Builtins 废除 sentinel：-1 拦截在 builtin 定义处
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

    pub(crate) fn builtin_listen(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("listen expects 2 arguments (fd, backlog)"));
        }
        let fd = match &args[0] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("listen: fd must be i32")),
        };
        let backlog = match &args[1] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("listen: backlog must be i32")),
        };
        // SAFETY: listen() uses a validated fd that came from a previous socket() call.
        let ret = unsafe { libc::listen(fd as i32, backlog as i32) };
        // 0.31.22 Builtins 废除 sentinel：-1 拦截在 builtin 定义处
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

    pub(crate) fn builtin_accept(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("accept expects 1 argument (fd)"));
        }
        let fd = match &args[0] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("accept: fd must be i32")),
        };
        // SAFETY: sockaddr_in is POD; zeroed() is safe. accept() fills in the
        // sockaddr with client info — the fd was validated by the interpreter.
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        let mut addr_len: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as u32;
        // SAFETY: accept() uses a validated fd; addr and addr_len are properly sized.
        let client_fd = unsafe {
            libc::accept(
                fd as i32,
                &mut addr as *mut _ as *mut libc::sockaddr,
                &mut addr_len,
            )
        };
        // 0.31.22 Builtins 废除 sentinel：-1 拦截在 builtin 定义处
        if client_fd < 0 {
            return Err(InterpError::new(format!(
                "accept() failed: fd={} (OS error: {})",
                fd,
                std::io::Error::last_os_error()
            )));
        }
        Ok(Value::Int(client_fd as i64))
    }

    pub(crate) fn builtin_send(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("send expects 2 arguments (fd, data)"));
        }
        let fd = match &args[0] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("send: fd must be i32")),
        };
        let data = match &args[1] {
            Value::String(s) => s.clone(),
            _ => return Err(InterpError::new("send: data must be string")),
        };
        // SAFETY: send() writes up to data.len() bytes from a Rust string's buffer,
        // which is guaranteed to be valid readable memory. fd was validated above.
        let sent = unsafe {
            libc::send(
                fd as i32,
                data.as_ptr() as *const libc::c_void,
                data.len(),
                0,
            )
        };
        // 0.31.22 Builtins 废除 sentinel：-1 拦截在 builtin 定义处
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

    pub(crate) fn builtin_recv(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("recv expects 2 arguments (fd, buf_size)"));
        }
        let fd = match &args[0] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("recv: fd must be i32")),
        };
        let buf_size = match &args[1] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("recv: buf_size must be i32")),
        };
        if buf_size <= 0 {
            return Err(InterpError::new("recv: buf_size must be positive"));
        }
        let mut buf: Vec<u8> = vec![0u8; buf_size as usize];
        // SAFETY: recv() writes into a Rust Vec's buffer which is guaranteed writable
        // for buf_size bytes. fd was validated above. Returns -1 on error.
        let n = unsafe {
            libc::recv(
                fd as i32,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf_size as usize,
                0,
            )
        };
        if n < 0 {
            // 0.31.22 Builtins 废除 sentinel：-1 拦截在 builtin 定义处
            return Err(InterpError::new(format!(
                "recv() failed: fd={}, buf_size={} (OS error: {})",
                fd,
                buf_size,
                std::io::Error::last_os_error()
            )));
        }
        if n == 0 {
            // Connection closed by peer — return empty string (not an error)
            return Ok(Value::String(String::new()));
        }
        // n > 0: n is guaranteed <= buf_size (recv reads at most buf_size bytes).
        // The buffer was allocated with buf_size bytes, so buf[..n] is valid.
        let n = n as usize;
        if n > buf.len() {
            // audit: clamp to buffer length as defense-in-depth against
            // platform-specific recv behavior.
            return Ok(Value::String(String::new()));
        }
        buf.truncate(n);
        Ok(Value::String(String::from_utf8_lossy(&buf).to_string()))
    }

    pub(crate) fn builtin_close_fd(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("close_fd expects 1 argument (fd)"));
        }
        let fd = match &args[0] {
            Value::Int(v) => *v,
            _ => return Err(InterpError::new("close_fd: fd must be i32")),
        };
        // SAFETY: close() uses a validated fd from a previous socket() or accept() call.
        let ret = unsafe { libc::close(fd as i32) };
        // 0.31.22 Builtins 废除 sentinel：-1 拦截在 builtin 定义处
        if ret < 0 {
            return Err(InterpError::new(format!(
                "close_fd() failed: fd={} (OS error: {})",
                fd,
                std::io::Error::last_os_error()
            )));
        }
        Ok(Value::Int(0))
    }
    // === HTTP builtins (implemented via libc socket + http parsing) ===
    fn http_connect(host: &str, port: i64) -> Result<i64, InterpError> {
        // SAFETY: socket() creates a TCP socket; integer arguments are constants from libc.
        let domain = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        if domain < 0 {
            return Err(InterpError::new("http: failed to create socket"));
        }
        let c_host = std::ffi::CString::new(host)
            .map_err(|e| InterpError::new(format!("http: invalid host: {}", e)))?;
        // SAFETY: zeroed() is safe for POD C structs.
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_family = libc::AF_UNSPEC;
        hints.ai_socktype = libc::SOCK_STREAM;
        let port_str = format!("{}", port);
        let c_port =
            std::ffi::CString::new(port_str).map_err(|_| InterpError::new("http: invalid port"))?;
        let mut res: *mut libc::addrinfo = std::ptr::null_mut();
        // SAFETY: getaddrinfo returns a linked list of addrinfo structs that we validate
        // for non-null. connect uses the first result. freeaddrinfo frees the list.
        let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
        if err != 0 || res.is_null() {
            // SAFETY: fd/domain was validated by prior socket/connect calls.
            unsafe { libc::close(domain) };
            return Err(InterpError::new(format!(
                "http: could not resolve host '{}'",
                host
            )));
        }
        // SAFETY: fd/domain and addrinfo result are validated before use.
        let ret = unsafe { libc::connect(domain, (*res).ai_addr, (*res).ai_addrlen) };
        // SAFETY: res is non-null and was returned by getaddrinfo, so freeaddrinfo is safe.
        unsafe { libc::freeaddrinfo(res) };
        if ret < 0 {
            // SAFETY: fd/domain was validated by prior socket/connect calls.
            unsafe { libc::close(domain) };
            return Err(InterpError::new(format!(
                "http: connection refused to '{}:{}'",
                host, port
            )));
        }
        Ok(domain as i64)
    }

    /// Wrapper for libc::send that retries on EINTR and handles closed connections.
    fn send_all(fd: i32, buf: *const libc::c_void, len: usize) -> Result<(), InterpError> {
        let mut sent: isize = 0;
        while (sent as usize) < len {
            // SAFETY: fd is valid, buf points to valid memory of at least len bytes.
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

    /// Read all data from a socket until the connection is closed.
    /// IN-C6: uses dynamic Vec growth (extend_from_slice), not a fixed 64KB buffer.
    /// Response data accumulates via extend_from_slice until the connection closes.
    fn recv_all_into(fd: i32, result: &mut Vec<u8>) -> Result<(), InterpError> {
        let mut chunk = vec![0u8; 32768];
        loop {
            // SAFETY: fd is valid, chunk is a writable slice.
            let n =
                unsafe { libc::recv(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len(), 0) };
            if n < 0 {
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
        Self::send_all(
            fd as i32,
            c_req.as_ptr() as *const libc::c_void,
            request.len(),
        )?;
        let mut buf = Vec::new();
        Self::recv_all_into(fd as i32, &mut buf)?;
        // SAFETY: fd/domain was validated by prior socket/connect calls.
        unsafe { libc::close(fd as i32) };
        if buf.is_empty() {
            return Err(InterpError::new("http: empty response"));
        }
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    pub(crate) fn builtin_http_get(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("http_get expects 1 argument (url)"));
        }
        let url = match &args[0] {
            Value::String(s) => s.clone(),
            _ => return Err(InterpError::new("http_get: url must be string")),
        };
        // CRITICAL #13 fix: SSRF protection — reject non-http schemes and
        // private/loopback addresses.
        Self::validate_http_url(&url)?;
        // Parse URL: http://host[:port][/path]
        let url = url.trim_start_matches("http://");
        let (host, rest) = url.split_once('/').unwrap_or((url, ""));
        let path = if rest.is_empty() {
            "/"
        } else {
            &format!("/{}", rest)
        };
        let (host, port) = if let Some((h, p)) = host.split_once(':') {
            let port: i64 = p
                .parse()
                .map_err(|_| InterpError::new("http_get: invalid port"))?;
            (h, port)
        } else {
            (host, 80)
        };
        Self::validate_host_ssrf(host)?;
        let fd = Self::http_connect(host, port)?;
        let request = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path, host
        );
        let response = Self::http_send_recv(fd, &request)?;
        // Extract body after \r\n\r\n
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b)
            .unwrap_or(&response);
        Ok(Value::String(body.to_string()))
    }

    /// CRITICAL #13: Validate URL scheme and reject non-http URLs to prevent
    /// SSRF via file://, gopher://, etc.
    fn validate_http_url(url: &str) -> Result<(), InterpError> {
        let lower = url.to_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") {
            return Ok(());
        }
        // Reject other schemes (file://, ftp://, gopher://, etc.)
        if lower.contains("://") {
            return Err(InterpError::new(
                "http_get/http_post: only http:// and https:// schemes are allowed",
            ));
        }
        // Allow bare host:port/path (implicit http://)
        Ok(())
    }

    /// CRITICAL #13: Block SSRF by rejecting private/loopback addresses.
    fn validate_host_ssrf(host: &str) -> Result<(), InterpError> {
        // Reject obvious private/internal hostnames
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
        // Block private IP ranges (simplified: check prefixes)
        let private_prefixes = [
            "127.", // loopback
            "10.",  // private A
            "172.16.", "172.17.", "172.18.", "172.19.", "172.20.", "172.21.", "172.22.", "172.23.",
            "172.24.", "172.25.", "172.26.", "172.27.", "172.28.", "172.29.", "172.30.",
            "172.31.",  // private B
            "192.168.", // private C
            "169.254.", // link-local
            "::1", "fc", "fd", // IPv6 loopback + ULA
        ];
        if private_prefixes.iter().any(|p| host.starts_with(p)) {
            return Err(InterpError::new(
                "http_get/http_post: SSRF protection — private/internal addresses are blocked",
            ));
        }
        Ok(())
    }

    pub(crate) fn builtin_http_post(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new(
                "http_post expects 2 arguments (url, body)",
            ));
        }
        let url = match &args[0] {
            Value::String(s) => s.clone(),
            _ => return Err(InterpError::new("http_post: url must be string")),
        };
        let body = match &args[1] {
            Value::String(s) => s.clone(),
            _ => return Err(InterpError::new("http_post: body must be string")),
        };
        // CRITICAL #13 fix: SSRF protection.
        Self::validate_http_url(&url)?;
        let url = url.trim_start_matches("http://");
        let (host, rest) = url.split_once('/').unwrap_or((url, ""));
        let path = if rest.is_empty() {
            "/"
        } else {
            &format!("/{}", rest)
        };
        let (host, port) = if let Some((h, p)) = host.split_once(':') {
            let port: i64 = p
                .parse()
                .map_err(|_| InterpError::new("http_post: invalid port"))?;
            (h, port)
        } else {
            (host, 80)
        };
        Self::validate_host_ssrf(host)?;
        let fd = Self::http_connect(host, port)?;
        let request = format!(
            "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n{}",
            path, host, body.len(), body
        );
        let response = Self::http_send_recv(fd, &request)?;
        let res_body = response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b)
            .unwrap_or(&response);
        Ok(Value::String(res_body.to_string()))
    }
}
