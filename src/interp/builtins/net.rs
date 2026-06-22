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
            return Err(InterpError::new("socket expects 3 arguments (domain, type, protocol)"));
        }
        let domain = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("socket: domain must be i32")) };
        let type_ = match &args[1] { Value::Int(v) => *v, _ => return Err(InterpError::new("socket: type must be i32")) };
        let protocol = match &args[2] { Value::Int(v) => *v, _ => return Err(InterpError::new("socket: protocol must be i32")) };
        // SAFETY: libc::socket is safe per POSIX when arguments are valid integers;
        // we validate types above. Returns -1 on error, which we propagate.
        let fd = unsafe { libc::socket(domain as i32, type_ as i32, protocol as i32) };
        if fd >= 0 {
            // Set SO_REUSEADDR so bind works immediately after close (TIME_WAIT avoidance)
            let reuse: libc::c_int = 1;
            unsafe {
                libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR,
                    &reuse as *const _ as *const libc::c_void, std::mem::size_of_val(&reuse) as libc::socklen_t);
            }
        }
        Ok(Value::Int(fd as i64))
    }

    pub(crate) fn builtin_connect(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 3 {
            return Err(InterpError::new("connect expects 3 arguments (fd, host, port)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("connect: fd must be i32")) };
        let host = match &args[1] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("connect: host must be string")) };
        let port = match &args[2] { Value::Int(v) => *v, _ => return Err(InterpError::new("connect: port must be i32")) };
        let c_host = std::ffi::CString::new(host.as_str())
            .map_err(|e| InterpError::new(format!("connect: invalid host: {}", e)))?;
        // SAFETY: zeroed() is safe for POD structs like addrinfo/sockaddr_in.
        // getaddrinfo allocates memory that we check for null before use.
        // connect uses validated fd and the res pointer we receive from getaddrinfo.
        // freeaddrinfo frees memory allocated by getaddrinfo — safe as long as res is
        // non-null and was returned by getaddrinfo (both checked above).
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_family = libc::AF_UNSPEC;
        hints.ai_socktype = libc::SOCK_STREAM;
        let port_str = format!("{}", port);
        let c_port = std::ffi::CString::new(port_str)
            .map_err(|_| InterpError::new("connect: invalid port"))?;
        let mut res: *mut libc::addrinfo = std::ptr::null_mut();
        let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
        if err != 0 || res.is_null() {
            return Err(InterpError::new(format!("connect: getaddrinfo failed for '{}'", host)));
        }
        let ret = unsafe { libc::connect(fd as i32, (*res).ai_addr, (*res).ai_addrlen) };
        unsafe { libc::freeaddrinfo(res) };
        if ret == 0 {
            // Disable Nagle's algorithm for responsive small-message communication
            let nodelay: libc::c_int = 1;
            unsafe {
                libc::setsockopt(fd as i32, libc::IPPROTO_TCP, libc::TCP_NODELAY,
                    &nodelay as *const _ as *const libc::c_void,
                    std::mem::size_of_val(&nodelay) as libc::socklen_t);
            }
        }
        Ok(Value::Int(ret as i64))
    }

    pub(crate) fn builtin_bind(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("bind expects 2 arguments (fd, port)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("bind: fd must be i32")) };
        let port = match &args[1] { Value::Int(v) => *v, _ => return Err(InterpError::new("bind: port must be i32")) };
        // SAFETY: sockaddr_in is a POD struct; zeroed() is safe. bind() uses the validated
        // fd and a properly initialized sockaddr_in structure.
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = (port as u16).to_be();
        addr.sin_addr.s_addr = libc::INADDR_ANY as u32;
        let ret = unsafe { libc::bind(fd as i32, &addr as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in>() as u32) };
        Ok(Value::Int(ret as i64))
    }

    pub(crate) fn builtin_listen(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("listen expects 2 arguments (fd, backlog)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("listen: fd must be i32")) };
        let backlog = match &args[1] { Value::Int(v) => *v, _ => return Err(InterpError::new("listen: backlog must be i32")) };
        // SAFETY: listen() uses a validated fd that came from a previous socket() call.
        let ret = unsafe { libc::listen(fd as i32, backlog as i32) };
        Ok(Value::Int(ret as i64))
    }

    pub(crate) fn builtin_accept(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("accept expects 1 argument (fd)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("accept: fd must be i32")) };
        // SAFETY: sockaddr_in is POD; zeroed() is safe. accept() fills in the
        // sockaddr with client info — the fd was validated by the interpreter.
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        let mut addr_len: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as u32;
        // SAFETY: accept() uses a validated fd; addr and addr_len are properly sized.
        let client_fd = unsafe { libc::accept(fd as i32, &mut addr as *mut _ as *mut libc::sockaddr, &mut addr_len) };
        Ok(Value::Int(client_fd as i64))
    }

    pub(crate) fn builtin_send(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("send expects 2 arguments (fd, data)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("send: fd must be i32")) };
        let data = match &args[1] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("send: data must be string")) };
        // SAFETY: send() writes up to data.len() bytes from a Rust string's buffer,
        // which is guaranteed to be valid readable memory. fd was validated above.
        let sent = unsafe { libc::send(fd as i32, data.as_ptr() as *const libc::c_void, data.len(), 0) };
        Ok(Value::Int(sent as i64))
    }

    pub(crate) fn builtin_recv(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("recv expects 2 arguments (fd, buf_size)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("recv: fd must be i32")) };
        let buf_size = match &args[1] { Value::Int(v) => *v, _ => return Err(InterpError::new("recv: buf_size must be i32")) };
        if buf_size <= 0 {
            return Err(InterpError::new("recv: buf_size must be positive"));
        }
        let mut buf: Vec<u8> = vec![0u8; buf_size as usize];
        // SAFETY: recv() writes into a Rust Vec's buffer which is guaranteed writable
        // for buf_size bytes. fd was validated above. Returns -1 on error.
        let n = unsafe { libc::recv(fd as i32, buf.as_mut_ptr() as *mut libc::c_void, buf_size as usize, 0) };
        if n <= 0 {
            return Ok(Value::String(String::new()));
        }
        buf.truncate(n as usize);
        Ok(Value::String(String::from_utf8_lossy(&buf).to_string()))
    }

    pub(crate) fn builtin_close_fd(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("close_fd expects 1 argument (fd)"));
        }
        let fd = match &args[0] { Value::Int(v) => *v, _ => return Err(InterpError::new("close_fd: fd must be i32")) };
        // SAFETY: close() uses a validated fd from a previous socket() or accept() call.
        let ret = unsafe { libc::close(fd as i32) };
        Ok(Value::Int(ret as i64))
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
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_family = libc::AF_UNSPEC;
        hints.ai_socktype = libc::SOCK_STREAM;
        let port_str = format!("{}", port);
        let c_port = std::ffi::CString::new(port_str)
            .map_err(|_| InterpError::new("http: invalid port"))?;
        let mut res: *mut libc::addrinfo = std::ptr::null_mut();
        // SAFETY: getaddrinfo returns a linked list of addrinfo structs that we validate
        // for non-null. connect uses the first result. freeaddrinfo frees the list.
        let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
        if err != 0 || res.is_null() {
            unsafe { libc::close(domain) };
            return Err(InterpError::new(format!("http: could not resolve host '{}'", host)));
        }
        let ret = unsafe { libc::connect(domain, (*res).ai_addr, (*res).ai_addrlen) };
        unsafe { libc::freeaddrinfo(res) };
        if ret < 0 {
            unsafe { libc::close(domain) };
            return Err(InterpError::new(format!("http: connection refused to '{}:{}'", host, port)));
        }
        Ok(domain as i64)
    }

    fn http_send_recv(fd: i64, request: &str) -> Result<String, InterpError> {
        let c_req = std::ffi::CString::new(request)
            .map_err(|e| InterpError::new(format!("http: invalid request: {}", e)))?;
        // SAFETY: send() writes from a CString buffer (null-terminated, valid memory).
        // recv() writes into a Rust Vec buffer (valid writable memory, 64KB).
        // close() uses the validated fd from http_connect().
        unsafe { libc::send(fd as i32, c_req.as_ptr() as *const libc::c_void, request.len(), 0) };
        let mut buf: Vec<u8> = vec![0u8; 65536];
        let n = unsafe { libc::recv(fd as i32, buf.as_mut_ptr() as *mut libc::c_void, 65536, 0) };
        unsafe { libc::close(fd as i32) };
        if n <= 0 {
            return Err(InterpError::new("http: empty response"));
        }
        buf.truncate(n as usize);
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    pub(crate) fn builtin_http_get(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 1 {
            return Err(InterpError::new("http_get expects 1 argument (url)"));
        }
        let url = match &args[0] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("http_get: url must be string")) };
        // Parse URL: http://host[:port][/path]
        let url = url.trim_start_matches("http://");
        let (host, rest) = url.split_once('/').unwrap_or((url, ""));
        let path = if rest.is_empty() { "/" } else { &format!("/{}", rest) };
        let (host, port) = if let Some((h, p)) = host.split_once(':') {
            let port: i64 = p.parse().map_err(|_| InterpError::new("http_get: invalid port"))?;
            (h, port)
        } else {
            (host, 80)
        };
        let fd = Self::http_connect(host, port)?;
        let request = format!("GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n", path, host);
        let response = Self::http_send_recv(fd, &request)?;
        // Extract body after \r\n\r\n
        let body = response.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&response);
        Ok(Value::String(body.to_string()))
    }

    pub(crate) fn builtin_http_post(&self, args: Vec<Value>) -> Result<Value, InterpError> {
        if args.len() != 2 {
            return Err(InterpError::new("http_post expects 2 arguments (url, body)"));
        }
        let url = match &args[0] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("http_post: url must be string")) };
        let body = match &args[1] { Value::String(s) => s.clone(), _ => return Err(InterpError::new("http_post: body must be string")) };
        let url = url.trim_start_matches("http://");
        let (host, rest) = url.split_once('/').unwrap_or((url, ""));
        let path = if rest.is_empty() { "/" } else { &format!("/{}", rest) };
        let (host, port) = if let Some((h, p)) = host.split_once(':') {
            let port: i64 = p.parse().map_err(|_| InterpError::new("http_post: invalid port"))?;
            (h, port)
        } else {
            (host, 80)
        };
        let fd = Self::http_connect(host, port)?;
        let request = format!(
            "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n{}",
            path, host, body.len(), body
        );
        let response = Self::http_send_recv(fd, &request)?;
        let res_body = response.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&response);
        Ok(Value::String(res_body.to_string()))
    }
}
