//! 0.1.9 Phase D — cap 与 std（0.39.71+）。
//!
//! - 0.39.71：`make_token()` 全局唯一 token id。
//! - 0.39.72：线性 SystemToken 能力类型（move-only、非 Copy）——make_token 返回
//!   SystemToken，token_id(t: SystemToken) -> i64 消费 t 取唯一 id。use-after-move → E0304，
//!   泄漏 → E0256（旧端失效 = move 后旧绑定不可用）。

use super::*;

/// 正集：`token_id(make_token())` 连续取两两不同的唯一 id（VM + native）。
/// 注意：进程内单调计数器跨测试共享（VM static / native static），故不断言
/// 具体值，只断言三值两两不同（程序自检打印 distinct）。
#[test]
fn phase_d_make_token_globally_unique_dual() {
    let src = r#"
func main() -> i32 {
    let a = token_id(make_token())
    let b = token_id(make_token())
    let c = token_id(make_token())
    if a != b && b != c && a != c { println("distinct") } else { println("collision") }
    0
}
"#;
    check_source(src).expect("make_token/token_id must check");
    if !can_link() {
        return;
    }
    let expected = "distinct";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 正集：SystemToken 可显式 drop（消费能力）。
#[test]
fn phase_d_token_drop_ok() {
    check_source(
        r#"
func main() -> i32 {
    let t = make_token()
    drop(t)
    0
}
"#,
    )
    .expect("SystemToken must be droppable (explicit consume)");
}

/// 负集：SystemToken move 后旧绑定再使用 → E0304（旧端失效）。
#[test]
fn phase_d_token_use_after_move_rejected() {
    let errs = check_source(
        r#"
func main() -> i32 {
    let t = make_token()
    let t2 = t
    let id = token_id(t)
    drop(id)
    0
}
"#,
    )
    .expect_err("SystemToken use-after-move must be rejected");
    assert!(
        errs.iter()
            .any(|d| { d.code.as_deref() == Some(crate::diagnostic::codes::E0304) }),
        "SystemToken use-after-move must be E0304, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 负集：SystemToken 弃置不消费（`let _ = t`）→ 拒绝（E0304 或 E0256）。
#[test]
fn phase_d_token_leak_rejected() {
    let errs = check_source(
        r#"
func main() -> i32 {
    let t = make_token()
    let _ = t
    0
}
"#,
    )
    .expect_err("SystemToken discard without consume must be rejected");
    assert!(
        errs.iter().any(|d| {
            d.code.as_deref() == Some(crate::diagnostic::codes::E0304)
                || d.code.as_deref() == Some(crate::diagnostic::codes::E0256)
        }),
        "SystemToken discard must be rejected (E0304/E0256), got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 正集：SystemToken 可跨函数整体转移（直通 + 消费），双后端。
#[test]
fn phase_d_token_cross_function_move_dual() {
    let src = r#"
func take(t: SystemToken) -> i32 { let id = token_id(t); drop(id); 1 }
func main() -> i32 {
    let t = make_token()
    let n = take(t)
    println(n)
    0
}
"#;
    check_source(src).expect("SystemToken cross-function move must check");
    if !can_link() {
        return;
    }
    let expected = "1";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// Phase D (0.39.73)：TokenChannel —— SystemToken 跨通道整体转移（跨任务 move）。
/// 通道本身可 Copy（共享，同 Channel）；只有 SystemToken 线性（旧端失效）。
/// 语义：send 消费 t（move 入通道），recv 返回全新 SystemToken 义务。

/// 正集：send/recv 双后端直通，token 唯一 id 沿通道往返。
#[test]
fn phase_d_token_channel_send_recv_dual() {
    let src = r#"
func main() -> i32 {
    let ch = token_channel_new()
    let t = make_token()
    token_channel_send(ch, t)
    let u = token_channel_recv(ch)
    let a = token_id(u)
    let t2 = make_token()
    token_channel_send(ch, t2)
    let u2 = token_channel_recv(ch)
    let b = token_id(u2)
    if a != b { println("distinct") } else { println("collision") }
    drop(ch)
    drop(a)
    drop(b)
    0
}
"#;
    check_source(src).expect("token channel send/recv must check");
    if !can_link() {
        return;
    }
    let expected = "distinct";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 负集：send 后旧 token 绑定再使用 → E0304（旧端失效）。
#[test]
fn phase_d_token_channel_use_after_send_rejected() {
    let errs = check_source(
        r#"
func main() -> i32 {
    let ch = token_channel_new()
    let t = make_token()
    token_channel_send(ch, t)
    let id = token_id(t)
    drop(ch)
    drop(id)
    0
}
"#,
    )
    .expect_err("token use after channel send must be rejected");
    assert!(
        errs.iter()
            .any(|d| { d.code.as_deref() == Some(crate::diagnostic::codes::E0304) }),
        "token use-after-send must be E0304, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 正集：通道可 Copy/共享（同一通道多 token 往返），只有 token 线性。
#[test]
fn phase_d_token_channel_shared_copyable() {
    check_source(
        r#"
func main() -> i32 {
    let ch = token_channel_new()
    let ch2 = ch
    let t = make_token()
    token_channel_send(ch, t)
    let u = token_channel_recv(ch2)
    let id = token_id(u)
    drop(ch)
    drop(ch2)
    drop(id)
    0
}
"#,
    )
    .expect("TokenChannel must be copyable/shared; only SystemToken is linear");
}

/// Phase D (0.39.74)：SystemToken × Phase C 线性泛型 + 容器组合集成。
/// token 是首个真实线性能力消费者——须与 `linear T` transfer-only 面无缝工作。

/// 正集：SystemToken 经 `linear T` 泛型直通（transfer-only），双后端。
#[test]
fn phase_d_system_token_through_linear_generic_dual() {
    let src = r#"
func pass<linear T>(x: T) -> T { x }
func main() -> i32 {
    let t = make_token()
    let d = pass(t)
    let id = token_id(d)
    drop(id)
    0
}
"#;
    check_source(src).expect("SystemToken through linear T must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "", "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), "", "native");
}

/// 正集：List<SystemToken> 构建 + 定向头提取 + 整体 drop，双后端。
#[test]
fn phase_d_system_token_list_composition_dual() {
    let src = r#"
func main() -> i32 {
    let a = make_token()
    let b = make_token()
    let v = [a, b]
    let t0 = v[0]
    let i = token_id(t0)
    drop(v)
    drop(i)
    0
}
"#;
    check_source(src).expect("List<SystemToken> must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "", "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), "", "native");
}

/// 正集：List<SystemToken> 整体经 `linear T` 泛型直通，之后仍可提取元素。
#[test]
fn phase_d_system_token_list_through_linear_generic() {
    check_source(
        r#"
func pass<linear T>(x: T) -> T { x }
func main() -> i32 {
    let a = make_token()
    let b = make_token()
    let v = [a, b]
    let d = pass(v)
    let t0 = d[0]
    let i = token_id(t0)
    drop(d)
    drop(i)
    0
}
"#,
    )
    .expect("List<SystemToken> through linear T must check");
}

/// 负集：SystemToken 经 Free-T 泛型 → E0432（线性种类不匹配）。
#[test]
fn phase_d_system_token_free_t_generic_rejected() {
    let errs = check_source(
        r#"
func id_free<T>(x: T) -> T { x }
func main() -> i32 {
    let t = make_token()
    let d = id_free(t)
    let id = token_id(d)
    drop(id)
    0
}
"#,
    )
    .expect_err("SystemToken through Free-T must be rejected");
    assert!(
        errs.iter()
            .any(|d| { d.code.as_deref() == Some(crate::diagnostic::codes::E0432) }),
        "SystemToken through Free-T must be E0432, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// Phase D (0.39.75)：收 cap 的 fs/env API——read_file_guarded / get_env_guarded。
/// SystemToken 作为能力门禁被调用消费（每次授权一次受保护操作）；未持 token →
/// E0242；用后旧绑定 → E0304。

/// 正集：read_file_guarded(path, t) 读文件，token 能力门禁，双后端。
#[test]
fn phase_d_read_file_guarded_dual() {
    let dir = std::env::temp_dir().join(format!("mimi_phase_d_guarded_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let data = dir.join("data.txt");
    std::fs::write(&data, "guarded-content").expect("write guarded data");
    let src = format!(
        r#"
func main() -> i32 {{
    let t = make_token()
    let r = read_file_guarded("{path}", t)
    match r {{
        Ok(s) => {{ println(s) }}
        Err(e) => {{ println("ERR"); println(e) }}
    }}
    0
}}
"#,
        path = data.display()
    );
    check_source(&src).expect("read_file_guarded must check");
    if !can_link() {
        return;
    }
    let expected = "guarded-content";
    let (_v, interp) = checked_run_source_with_stdout(&src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(&src).expect("native");
    assert_eq!(native.trim(), expected, "native");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 正集：get_env_guarded(name, t) 读环境变量，token 能力门禁，双后端。
/// 用 PATH（恒存在）避免并行测试的 env 变异。
#[test]
fn phase_d_get_env_guarded_dual() {
    let src = r#"
func main() -> i32 {
    let t = make_token()
    let r = get_env_guarded("PATH", t)
    match r {
        Ok(s) => { if len(s) > 0 { println("env-ok") } else { println("env-empty") } }
        Err(e) => { println("ERR") }
    }
    0
}
"#;
    check_source(src).expect("get_env_guarded must check");
    if !can_link() {
        return;
    }
    let expected = "env-ok";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 负集：无 token 调受保护 API → E0242。
#[test]
fn phase_d_guarded_api_requires_token() {
    let errs = check_source(
        r#"
func main() -> i32 {
    let r = read_file_guarded("/tmp/nonexistent", 42)
    0
}
"#,
    )
    .expect_err("guarded API without token must be rejected");
    assert!(
        errs.iter()
            .any(|d| { d.code.as_deref() == Some(crate::diagnostic::codes::E0242) }),
        "guarded API without token must be E0242, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 负集：token 经受保护调用后被消费，旧绑定再使用 → E0304。
#[test]
fn phase_d_guarded_api_consumes_token() {
    let errs = check_source(
        r#"
func main() -> i32 {
    let t = make_token()
    let r = read_file_guarded("/tmp/nonexistent", t)
    let id = token_id(t)
    0
}
"#,
    )
    .expect_err("token use after guarded call must be rejected");
    assert!(
        errs.iter()
            .any(|d| { d.code.as_deref() == Some(crate::diagnostic::codes::E0304) }),
        "token use-after-guarded-call must be E0304, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// Phase D (0.39.76)：收 cap 的 net API——http_get_guarded(url, t)。
/// 网络 I/O 在本环境被 SSRF 保护阻断，故只锁类型/线性面（check 通过、
/// 缺 token E0242、消费后复用 E0304）；运行时接线以 native 可编译为证。

/// 正集：http_get_guarded 类型/线性面可 check（url string + SystemToken 门禁）。
#[test]
fn phase_d_http_get_guarded_checks() {
    check_source(
        r#"
func main() -> i32 {
    let t = make_token()
    let r = http_get_guarded("http://example.com/x", t)
    drop(r)
    0
}
"#,
    )
    .expect("http_get_guarded must type-check");
}

/// 负集：无 token 调 http_get_guarded → E0242。
#[test]
fn phase_d_http_get_guarded_requires_token() {
    let errs = check_source(
        r#"
func main() -> i32 {
    let r = http_get_guarded("http://example.com/x", 1)
    0
}
"#,
    )
    .expect_err("http_get_guarded without token must be rejected");
    assert!(
        errs.iter()
            .any(|d| { d.code.as_deref() == Some(crate::diagnostic::codes::E0242) }),
        "http_get_guarded without token must be E0242, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 负集：token 经受保护调用后被消费，旧绑定再使用 → E0304。
#[test]
fn phase_d_http_get_guarded_consumes_token() {
    let errs = check_source(
        r#"
func main() -> i32 {
    let t = make_token()
    let r = http_get_guarded("http://example.com/x", t)
    let id = token_id(t)
    drop(r)
    drop(id)
    0
}
"#,
    )
    .expect_err("token use after http_get_guarded must be rejected");
    assert!(
        errs.iter()
            .any(|d| { d.code.as_deref() == Some(crate::diagnostic::codes::E0304) }),
        "token use-after-http_get_guarded must be E0304, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}
