//! 0.39.135 可用性修复回归（L1 双后端等价 + L2 fail-closed）
//!
//! 来源：全特性真实可用性探针（AI 评测，0.39.134 后）。四项 P0 分歧
//! 与一项检查器漏洞在此锁定：
//! 1. newtype 透明化（VM 构造器恒等化）：注解解包 / 显示 / 标量实参三面；
//! 2. 泛型单态化记录按值 ABI：`pass<Plain>(p)` 后字段读取不再垃圾值；
//! 3. actor 方法返回 string：mailbox 结果 blob 不再返回悬垂指针
//!    （内核卡 e18 形状）；
//! 4. runs-Flow actor 的普通方法调用：方法优先于转移分发；
//! 5. E0444：session 载荷必须为整数标量（fail-closed）。

use crate::tests::{
    can_link, check_source, checked_run_source_with_stdout, run_source_with_stdout,
};

/// 双后端对拍 helper：check → interp stdout → native stdout 三方一致。
fn assert_dual(src: &str, expected: &str) {
    check_source(src).unwrap_or_else(|diags| {
        panic!(
            "checker rejected source:\n{}",
            diags
                .iter()
                .map(|d| format!("{}", d))
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    let (_v, interp_out) = run_source_with_stdout(src);
    assert_eq!(interp_out.trim(), expected, "interp stdout mismatch");
    if !can_link() {
        return;
    }
    let native =
        crate::tests::checked_codegen_compile_and_run(src).expect("native compile+run failed");
    assert_eq!(native.trim(), expected, "native stdout mismatch");
}

// ── 1a. newtype 注解解包：`let v: i32 = u`（此前 VM E0800 崩溃）──
#[test]
fn usability_newtype_annotated_unwrap_dual() {
    assert_dual(
        r#"
newtype UserId = i32
func main() -> i32 {
    let u = UserId(42)
    let v: i32 = u
    println(v)
    0
}
"#,
        "42",
    );
}

// ── 1b. newtype 显示透明：println(u) 两后端同印载荷 ──
#[test]
fn usability_newtype_display_transparent_dual() {
    assert_dual(
        r#"
newtype UserId = i32
func main() -> i32 {
    let u = UserId(42)
    println(u)
    0
}
"#,
        "42",
    );
}

// ── 1c. newtype 标量参数直传（此前 VM value_to_f64 崩溃）──
#[test]
fn usability_newtype_scalar_param_passthrough_dual() {
    assert_dual(
        r#"
newtype UserId = i32
func take(x: i32) -> i32 { x * 2 }
func main() -> i32 {
    let u = UserId(21)
    println(take(u))
    0
}
"#,
        "42",
    );
}

// ── 2. 泛型单态化记录按值传递（此前 q.v 读到地址位垃圾）──
#[test]
fn usability_generic_record_byvalue_mono_dual() {
    assert_dual(
        r#"
type Plain { v: i32 }
func pass<T>(x: T) -> T { x }
func main() -> i32 {
    let p = Plain { v: 7 }
    let q = pass(p)
    println(q.v)
    0
}
"#,
        "7",
    );
}

// ── 3. actor 方法返回 string（内核卡 e18 形状；此前 native 空串）──
#[test]
fn usability_actor_string_return_dual() {
    assert_dual(
        r#"
actor Greeter {
    func greet(name: string) -> string {
        "hi " + name
    }
}
func main() -> i32 {
    let a = Greeter.spawn()
    println(a.greet("mimi"))
    0
}
"#,
        "hi mimi",
    );
}

// ── 4. runs-Flow actor 普通方法调用（此前 VM E0800 no-transition）──
#[test]
fn usability_runs_flow_actor_plain_method_dual() {
    assert_dual(
        r#"
flow Ledger {
    state Open { total: i32 }
    transition bump(Open) -> Open {
        return Open { total: self.total + 1 }
    }
}
actor LedgerWorker runs Ledger {
    func ping() -> i32 { 1 }
}
func main() -> i32 {
    let w = LedgerWorker.spawn()
    println(w.ping())
    0
}
"#,
        "1",
    );
}

// ── 5. E0444：session 载荷必须整数标量（fail-closed 负例）──
#[test]
fn usability_session_payload_non_integer_rejected() {
    for payload in ["string", "f64", "bool"] {
        let src = format!(
            "session Bad = !{p} . ?{p} . end\nfunc main() -> i32 {{ 0 }}\n",
            p = payload
        );
        let diags = check_source(&src).expect_err("non-integer session payload must be rejected");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("E0444")),
            "expected E0444 for payload '{}', got: {:?}",
            payload,
            diags
        );
    }
    // 整数载荷合法（i32/i64 混用）。
    let ok = "session Good = !i64 . ?i32 . end\nfunc main() -> i32 { 0 }\n";
    check_source(ok).expect("integer session payloads must check");
}
