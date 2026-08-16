// Real-thread net concurrency smoke.
//
// Unlike `mimi run` (bytecode VM sequential spawn/await), the native binary
// path uses real worker threads. This test locks the concurrent multi-client
// TCP echo shape from projects/mimichat-modern into the stress suite.

use super::build_and_run_native;

#[test]
fn stress_native_multi_client_tcp_echo() {
    if std::env::var("CARGO_BIN_EXE_mimi").is_err()
        && !std::path::Path::new("target/debug/mimi").exists()
    {
        eprintln!("SKIP: mimi binary not available");
        return;
    }
    let source = r#"
use std::net;

func echo_handler(client: i64) -> i32 {
    let data = tcp_recv(client, 4096)
    if data.is_err() {
        close_fd(client)
        return 1
    }
    let msg = data.unwrap()
    let sr = tcp_send(client, "echo:" + msg)
    if sr.is_err() {
        close_fd(client)
        return 2
    }
    close_fd(client)
    0
}

func server_echo(port: i32, ready: Channel<i64>) -> i32 {
    let s = tcp_listen(port, 8)
    if s.is_err() { return 1 }
    let fd = s.unwrap()
    channel_send(ready, 1)
    let mut done = 0
    while done < 3 {
        let c = tcp_accept(fd)
        if c.is_err() {
            close_fd(fd)
            return 2
        }
        let client = c.unwrap()
        let _ = spawn echo_handler(client)
        done = done + 1
    }
    close_fd(fd)
    0
}

func client_echo(port: i32, id: i32) -> i32 {
    let c = tcp_connect("127.0.0.1", port)
    if c.is_err() { return 3 }
    let fd = c.unwrap()
    let tag = "hello-" + to_string(id)
    let _ = tcp_send(fd, tag)
    let data = tcp_recv(fd, 4096)
    if data.is_err() {
        close_fd(fd)
        return 4
    }
    let msg = data.unwrap()
    close_fd(fd)
    if msg == "echo:" + tag { 0 } else { 5 }
}

func main() -> i32 {
    let net_port = 19235
    let net_ready = channel_new()
    let net_server = spawn server_echo(net_port, net_ready)
    let _ = channel_recv(net_ready)
    let net_c1 = spawn client_echo(net_port, 1)
    let net_c2 = spawn client_echo(net_port, 2)
    let net_c3 = spawn client_echo(net_port, 3)
    let net_srv = await net_server
    let net_r1 = await net_c1
    let net_r2 = await net_c2
    let net_r3 = await net_c3
    channel_drop(net_ready)
    if net_srv != 0 || net_r1 != 0 || net_r2 != 0 || net_r3 != 0 {
        return 1
    }
    println("native-net-ok")
    0
}
"#;

    let stdout = build_and_run_native(source).expect("native multi-client TCP echo failed");
    assert!(
        stdout.contains("native-net-ok"),
        "expected native-net-ok in stdout, got {:?}",
        stdout
    );
}
