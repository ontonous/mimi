//! 0.1.9 Phase E — 语义、验证、教核（0.39.80+）。
//!
//! 0.39.81：MutexGuard 恰一次 unlock——guard 为线性资源，必须且只能被
//! `mutex_unlock` 消费一次（双重解锁 E0304、泄漏 E0256）；`mutex_get`/
//! `mutex_set` 借用 guard（读取/写入后仍需解锁）。

use super::*;

/// 正集：lock → get（借用）→ set（借用）→ unlock（消费），双后端。
#[test]
fn phase_e_mutex_guard_lock_use_unlock_dual() {
    let src = r#"
func main() -> i32 {
    let m = mutex_new(5)
    let g = mutex_lock(m)
    let v = mutex_get(g)
    mutex_set(g, v + 1)
    mutex_unlock(g)
    let g2 = mutex_lock(m)
    let v2 = mutex_get(g2)
    println(v2)
    mutex_unlock(g2)
    mutex_drop(m)
    0
}
"#;
    check_source(src).expect("lock/get/set/unlock must check");
    if !can_link() {
        return;
    }
    let expected = "6";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 负集：双重 unlock → E0304（guard 已被消费）。
#[test]
fn phase_e_mutex_guard_double_unlock_rejected() {
    let errs = check_source(
        r#"
func main() -> i32 {
    let m = mutex_new(5)
    let g = mutex_lock(m)
    mutex_unlock(g)
    mutex_unlock(g)
    drop(m)
    0
}
"#,
    )
    .expect_err("double unlock must be rejected");
    assert!(
        errs.iter()
            .any(|d| d.code.as_deref() == Some(crate::diagnostic::codes::E0304)),
        "double unlock must be E0304, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 负集：guard 泄漏（未 unlock 就返回）→ E0256。
#[test]
fn phase_e_mutex_guard_leak_rejected() {
    let errs = check_source(
        r#"
func main() -> i32 {
    let m = mutex_new(5)
    let g = mutex_lock(m)
    drop(m)
    0
}
"#,
    )
    .expect_err("guard leak must be rejected");
    assert!(
        errs.iter()
            .any(|d| d.code.as_deref() == Some(crate::diagnostic::codes::E0256)),
        "guard leak must be E0256, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 正集：guard 可整体转移（move）后由新绑定解锁。
#[test]
fn phase_e_mutex_guard_move_then_unlock() {
    check_source(
        r#"
func main() -> i32 {
    let m = mutex_new(5)
    let g = mutex_lock(m)
    let h = g
    let v = mutex_get(h)
    mutex_unlock(h)
    drop(v)
    mutex_drop(m)
    0
}
"#,
    )
    .expect("guard move then unlock must check");
}

/// 正集：guard 经函数整体转移并在函数内解锁。
#[test]
fn phase_e_mutex_guard_cross_function_unlock() {
    check_source(
        r#"
func read_and_unlock(g: MutexGuard<i64>) -> i64 {
    let v = mutex_get(g)
    mutex_unlock(g)
    v
}
func main() -> i32 {
    let m = mutex_new(42)
    let g = mutex_lock(m)
    let v = read_and_unlock(g)
    drop(v)
    mutex_drop(m)
    0
}
"#,
    )
    .expect("guard cross-function unlock must check");
}
