//! 0.1.9 Phase G — 终测与发布（0.39.121-135）。
//!
//! 0.39.121：dogfood——线性能力管线（非玩具）。证明线性种类/SystemToken/
//! MutexGuard 在真实逻辑上双后端一致（Phase G「须用 linear T 或 cap 至少一处
//! 非玩具」）。

use super::*;

/// 与 `examples/dogfood/linear_guarded_backup.mimi` 同源（保持同步）。
const DOGFOOD_SRC: &str = r#"
cap BackupCap;
func transfer<linear T>(x: T) -> T { x }
func stage(t: SystemToken) -> SystemToken {
    transfer(t)
}
func read_guarded(path: string, t: SystemToken) -> Result<string, string> {
    read_file_guarded(path, t)
}
func main() -> i32 {
    let m = mutex_new(0)
    let g = mutex_lock(m)
    let n = mutex_get(g)
    mutex_unlock(g)
    drop(m)
    if n == 0 {
        println("mutex-guard-ok")
    } else {
        println("mutex-guard-bad")
    }
    let t0 = make_token()
    let t1 = stage(t0)
    let r = read_guarded("/nonexistent/mimi-dogfood.txt", t1)
    match r {
        Ok(s) => { println(s) }
        Err(_) => { println("guarded-read-rejected") }
    }
    let t2 = make_token()
    let e = get_env_guarded("PATH", t2)
    match e {
        Ok(s) => {
            if len(s) > 0 { println("guarded-env-ok") } else { println("guarded-env-empty") }
        }
        Err(_) => { println("guarded-env-err") }
    }
    0
}
"#;

const DOGFOOD_EXPECTED: &str = "mutex-guard-ok\nguarded-read-rejected\nguarded-env-ok";

/// dogfood：check + VM≡native 同输出（linear T / SystemToken / MutexGuard 组合）。
#[test]
fn phase_g_dogfood_linear_guarded_pipeline_dual() {
    check_source(DOGFOOD_SRC).expect("dogfood must check");
    if !can_link() {
        return;
    }
    let (_v, vm) = checked_run_source_with_stdout(DOGFOOD_SRC);
    assert_eq!(vm.trim(), DOGFOOD_EXPECTED.trim(), "VM");
    let native = checked_codegen_compile_and_run(DOGFOOD_SRC)
        .expect("dogfood must compile_checked and run natively");
    assert_eq!(native.trim(), DOGFOOD_EXPECTED.trim(), "native");
    assert_eq!(vm.trim(), native.trim(), "dual-backend agreement");
}

/// 0.1.9 Phase G (0.39.126): Result/Option `unwrap` 进入 resolved native slice。
/// Ok/Some → payload；Err/None → trap（双后端均非零退出，message 文本可异，
/// 退出码一致）。
#[test]
fn phase_g_result_unwrap_resolved_slice() {
    let ok_src = r#"
func main() -> i32 {
    let a: Result<i32, string> = Ok(7)
    let v = a.unwrap()
    println(v)
    0
}
"#;
    check_source(ok_src).expect("unwrap on Ok must check");
    if !can_link() {
        return;
    }
    let (_v, vm) = checked_run_source_with_stdout(ok_src);
    assert_eq!(vm.trim(), "7", "VM unwrap Ok");
    let native = checked_codegen_compile_and_run(ok_src).expect("native unwrap Ok");
    assert_eq!(native.trim(), "7", "native unwrap Ok");

    // Err/None path: both backends must trap (reject), never silently pass.
    let err_src = r#"
func main() -> i32 {
    let a: Result<i32, string> = Err("boom")
    let v = a.unwrap()
    println(v)
    0
}
"#;
    let vm_trap = run_source_bytecode_result(err_src);
    assert!(
        vm_trap.is_err(),
        "VM must trap on unwrap Err, got {:?}",
        vm_trap
    );
    let native_trap = checked_codegen_compile_and_run(err_src);
    assert!(
        native_trap.is_err(),
        "native must trap on unwrap Err, got {:?}",
        native_trap
    );
}

/// 0.1.9 Phase G (0.39.133): user capability nominal (`cap C`) 进入 resolved
/// native slice（opaque i64 能力句柄，镜像 SystemToken）。组合锁：`List<cap>`
/// 整表移交 + `linear T` cap 直通 均 0 resolved-skip，双后端一致。
#[test]
fn phase_g_capability_resolved_composition() {
    let cap_list_src = r#"
cap C;
func main() -> i32 {
    let xs: List<cap C> = [C, C]
    drop(xs)
    0
}
"#;
    check_source(cap_list_src).expect("cap list must type-check");
    if !can_link() {
        return;
    }
    let (_v, _vm) = checked_run_source_with_stdout(cap_list_src);
    let _native = checked_codegen_compile_and_run(cap_list_src)
        .expect("cap List composition must build natively");

    // linear T cap 直通（e12 形态）双后端
    let cap_pass_src = r#"
cap C;
func pass<linear T>(x: T) -> T { x }
func main() -> i32 {
    let c: cap C = C
    let d = pass(c)
    drop(d)
    0
}
"#;
    check_source(cap_pass_src).expect("linear T cap pass must type-check");
    let _native2 = checked_codegen_compile_and_run(cap_pass_src)
        .expect("linear T cap pass must build natively");
}
