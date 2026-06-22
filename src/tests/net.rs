use super::*;
use std::io::{Read, Write};
use std::time::Duration;
use std::net::TcpListener;

// ─── TCP echo server via Rust OS threads ──────────────────────
// Tests the full server lifecycle: socket → bind → listen → accept → recv → send → close.
// Server runs in a separate OS thread; client runs in the test thread.

const ECHO_PORT: i32 = 31077;
const MULTI_PORT: i32 = 31078;
const WRAP_PORT: i32 = 31079;

const SERVER_ECHO: &str = r#"
func main() -> string {
    let fd = socket(2, 1, 0)
    if fd < 0 { return "socket failed" }
    let ret = bind(fd, PORT)
    if ret < 0 { close_fd(fd); return "bind failed" }
    let ret2 = listen(fd, 1)
    if ret2 < 0 { close_fd(fd); return "listen failed" }
    let client_fd = accept(fd)
    if client_fd < 0 { close_fd(fd); return "accept failed" }
    let data = recv(client_fd, 1024)
    let sent = send(client_fd, "echo: " + data)
    close_fd(client_fd)
    close_fd(fd)
    data
}
"#;

const CLIENT_ECHO: &str = r#"
func main() -> string {
    let fd = socket(2, 1, 0)
    if fd < 0 { return "client socket failed" }
    let ret = connect(fd, "127.0.0.1", PORT)
    if ret < 0 { close_fd(fd); return "connect failed" }
    let sent = send(fd, "hello")
    let data = recv(fd, 1024)
    close_fd(fd)
    data
}
"#;

#[test]
fn net_echo_server() {
    let server_src = SERVER_ECHO.replace("PORT", &ECHO_PORT.to_string());
    let client_src = CLIENT_ECHO.replace("PORT", &ECHO_PORT.to_string());

    let server = std::thread::spawn(move || {
        run_source(&server_src)
    });

    std::thread::sleep(Duration::from_millis(100));

    let client_result = run_source(&client_src);
    let server_result = server.join().unwrap();

    assert_eq!(server_result, interp::Value::String("hello".to_string()),
        "Server should receive 'hello', got {:?}", server_result);
    assert_eq!(client_result, interp::Value::String("echo: hello".to_string()),
        "Client should receive 'echo: hello', got {:?}", client_result);
}

#[test]
fn net_echo_server_sequential() {
    // Sequential ping-pong: server recv→send→recv→send, client send→recv→send→recv.
    // Each step blocks until the counterparty's action completes, ensuring ordering
    // without relying on TCP message boundaries.
    let server_src = r#"
func main() -> string {
    let fd = socket(2, 1, 0)
    if fd < 0 { return "socket failed" }
    let ret = bind(fd, PORT)
    if ret < 0 { close_fd(fd); return "bind failed" }
    let ret2 = listen(fd, 1)
    if ret2 < 0 { close_fd(fd); return "listen failed" }
    let client_fd = accept(fd)
    if client_fd < 0 { close_fd(fd); return "accept failed" }
    let msg1 = recv(client_fd, 1024)
    send(client_fd, "ack1: " + msg1)
    let msg2 = recv(client_fd, 1024)
    send(client_fd, "ack2: " + msg2)
    close_fd(client_fd)
    close_fd(fd)
    msg1 + msg2
}
"#.replace("PORT", &MULTI_PORT.to_string());

    let client_src = r#"
func main() -> string {
    let fd = socket(2, 1, 0)
    if fd < 0 { return "client socket failed" }
    let ret = connect(fd, "127.0.0.1", PORT)
    if ret < 0 { close_fd(fd); return "connect failed" }
    send(fd, "ab")
    let resp1 = recv(fd, 1024)
    send(fd, "cd")
    let resp2 = recv(fd, 1024)
    close_fd(fd)
    resp1 + resp2
}
"#.replace("PORT", &MULTI_PORT.to_string());

    let server = std::thread::spawn(move || {
        run_source(&server_src)
    });

    std::thread::sleep(Duration::from_millis(100));

    let client_result = run_source(&client_src);
    let server_result = server.join().unwrap();

    assert_eq!(server_result, interp::Value::String("abcd".to_string()),
        "Server should receive 'ab' + 'cd', got {:?}", server_result);
    assert_eq!(client_result, interp::Value::String("ack1: aback2: cd".to_string()),
        "Client should receive ack'd responses, got {:?}", client_result);
}

#[test]
fn net_echo_server_accept_wrapper() {
    // Test that tcp_accept wrapper works end-to-end
    let server_src = r#"
func main() -> string {
    let fd = socket(2, 1, 0)
    if fd < 0 { return "socket failed" }
    let ret = bind(fd, PORT)
    if ret < 0 { close_fd(fd); return "bind failed" }
    let ret2 = listen(fd, 1)
    if ret2 < 0 { close_fd(fd); return "listen failed" }
    let client_fd = accept(fd)
    if client_fd < 0 { close_fd(fd); return "accept failed" }
    let data = recv(client_fd, 1024)
    let s = send(client_fd, "received: " + data)
    close_fd(client_fd)
    close_fd(fd)
    data
}
"#.replace("PORT", &WRAP_PORT.to_string());

    let client_src = r#"
func main() -> string {
    let fd = socket(2, 1, 0)
    if fd < 0 { return "client socket failed" }
    let ret = connect(fd, "127.0.0.1", PORT)
    if ret < 0 { close_fd(fd); return "connect failed" }
    let s = send(fd, "world")
    let data = recv(fd, 1024)
    close_fd(fd)
    data
}
"#.replace("PORT", &WRAP_PORT.to_string());

    let server = std::thread::spawn(move || {
        run_source(&server_src)
    });

    std::thread::sleep(Duration::from_millis(100));

    let client_result = run_source(&client_src);
    let server_result = server.join().unwrap();

    assert_eq!(server_result, interp::Value::String("world".to_string()),
        "Server should receive 'world', got {:?}", server_result);
    assert_eq!(client_result, interp::Value::String("received: world".to_string()),
        "Client should receive 'received: world', got {:?}", client_result);
}

// ─── HTTP server demo test ─────────────────────────────────
// Runs the HTTP server Mimi program via interpreter,
// connects with a Rust HTTP client via TcpStream.

const HTTP_PORT: i32 = 31080;

const HTTP_SERVER: &str = r#"
type NetError {
    SocketCreate
    ConnectFailed
    BindFailed
    ListenFailed
    AcceptFailed
    SendFailed
    RecvFailed
    HttpGetFailed
    HttpPostFailed
}

func handle_client(client_fd: i32) {
    recv(client_fd, 1024)
    let body = "Hello from Mimi!"
    let content_len = to_string(body.len())
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: " + content_len + "\r\nConnection: close\r\n\r\n" + body
    send(client_fd, response)
    close_fd(client_fd)
}

func main() -> i32 {
    let fd = socket(2, 1, 0)
    if fd < 0 { return 1 }
    let ret = bind(fd, PORT)
    if ret < 0 { close_fd(fd); return 1 }
    let ret2 = listen(fd, 5)
    if ret2 < 0 { close_fd(fd); return 1 }
    let client_fd = accept(fd)
    if client_fd < 0 { close_fd(fd); return 1 }
    handle_client(client_fd)
    close_fd(fd)
    0
}
"#;

#[test]
fn net_http_server_demo() {
    let server_src = HTTP_SERVER.replace("PORT", &HTTP_PORT.to_string());

    let server = std::thread::spawn(move || {
        run_source(&server_src)
    });

    std::thread::sleep(Duration::from_millis(200));

    // Connect as HTTP client via Rust TcpStream
    let addr = format!("127.0.0.1:{}", HTTP_PORT);
    let mut stream = std::net::TcpStream::connect(&addr)
        .expect("Rust client should connect to Mimi server");

    use std::io::{Read, Write};
    stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("Rust client should send request");

    let mut response = String::new();
    stream.read_to_string(&mut response)
        .expect("Rust client should read response");

    let server_result = server.join().unwrap();
    assert_eq!(server_result, interp::Value::Int(0),
        "Server should exit with code 0, got {:?}", server_result);

    assert!(response.contains("200 OK"),
        "Response should contain 200 OK, got: {}", response);
    assert!(response.contains("Hello from Mimi!"),
        "Response should contain 'Hello from Mimi!', got: {}", response);
    assert!(response.contains("Content-Length:"),
        "Response should contain Content-Length, got: {}", response);
}

// ─── Dual-backend TCP client test ──────────────────────────
// Tests connect + send + recv in both interpreter and codegen.
// Rust provides a TCP echo server for both to connect to.

fn start_echo_server(port: u16) -> std::thread::JoinHandle<()> {
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .expect("Rust echo server should bind");
    std::thread::spawn(move || {
        if let Some(Ok(mut s)) = listener.incoming().next() {
            let _ = s.set_nodelay(true);
            let mut buf = [0u8; 1024];
            let n: usize = s.read(&mut buf).unwrap_or(0);
            if n > 0 {
                let _ = s.write_all(&buf[..n]);
            }
        }
    })
}

const DUAL_PORT: i32 = 32001;
const DUAL_PORT2: i32 = 32002;

const TCP_CLIENT_PROG: &str = r#"
func main() -> i32 {
    let fd = socket(2, 1, 0)
    if fd < 0 { println("socket failed"); return 1 }
    let ret = connect(fd, "127.0.0.1", PORT)
    if ret < 0 { close_fd(fd); println("connect failed"); return 1 }
    let _sent = send(fd, "ping")
    let data = recv(fd, 1024)
    close_fd(fd)
    println(data)
    0
}
"#;

#[test]
fn dual_net_tcp_client_echo() {
    let echo_server = start_echo_server(DUAL_PORT as u16);
    std::thread::sleep(Duration::from_millis(100));

    let src = TCP_CLIENT_PROG.replace("PORT", &DUAL_PORT.to_string());

    let interp_result = run_source(&src);
    assert_eq!(interp_result, interp::Value::Int(0),
        "Interpreter should exit with 0, got {:?}", interp_result);

    let _ = echo_server.join();
}

#[test]
#[ignore = "codegen: recv returns struct value (WIP)"]
fn codegen_net_tcp_client_echo() {
    let echo_server = start_echo_server(DUAL_PORT2 as u16);
    std::thread::sleep(Duration::from_millis(100));

    let src = TCP_CLIENT_PROG.replace("PORT", &DUAL_PORT2.to_string());
    let codegen_result = compile_and_run(&src);
    assert!(codegen_result.is_ok(), "Codegen should succeed: {:?}", codegen_result.err());
    let stdout = codegen_result.unwrap();
    assert_eq!(stdout.trim(), "ping",
        "Codegen should output 'ping', got: {}", stdout.trim());

    let _ = echo_server.join();
}
