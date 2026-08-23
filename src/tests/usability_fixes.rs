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

// ── 6. trap 双后端等价（0.39.136）：rc=1 + 保留 trap 前 stdout + E 码诊断 ──
// 此前 native 走 abort()：SIGABRT rc=134 且丢弃缓冲 stdout，与 VM 干净退出
// 分歧。现在两后端同为 rc=1、stdout 保留、stderr 带 [E08xx] 码。
// （lib 面：VM stdout 保留 + 双后端 rc/stderr；二进制 stdout 保留面由
// tests/trap_semantics.rs 集成测试锁定。）
fn assert_trap_dual(src: &str, pre_trap_stdout: &str, ecode: &str, vm_frag: &str) {
    // VM 面：错误（原始 InterpError 无码，CLI 层才加 [E08xx]）+ 保留输出。
    // run_source_with_stdout 在 trap 时 panic，这里直接驱动 VM 以便错误路径
    // 下仍能取回已捕获的 stdout。
    let file = crate::tests::parse(src);
    let prog = crate::interp::bytecode::BytecodeCompiler::new()
        .compile_file(&file)
        .expect("bytecode compile failed");
    let mut vm = crate::interp::bytecode::BytecodeVM::new(prog);
    vm.enable_stdout_capture();
    let run = vm.run_value();
    assert!(run.is_err(), "program must trap");
    assert!(
        format!("{}", run.unwrap_err()).contains(vm_frag),
        "VM trap must mention '{vm_frag}'"
    );
    let vm_out = vm.take_stdout();
    assert_eq!(
        vm_out.trim(),
        pre_trap_stdout,
        "VM must preserve pre-trap stdout"
    );
    if !can_link() {
        return;
    }
    // native 面：compile harness 对非零退出返回 "exit code …, stderr: …"。
    // rc=1 + stderr E 码一次锁定。
    let native_err =
        crate::tests::checked_codegen_compile_and_run(src).expect_err("native program must trap");
    assert!(
        native_err.contains("exit code Some(1)"),
        "native trap must exit cleanly (rc=1), got: {native_err}"
    );
    assert!(
        native_err.contains(ecode),
        "native trap diagnostic must carry {ecode}: {native_err}"
    );
}

// ── 7. unit 载荷容器 ABI（0.39.136）：Result<(), E> / Option<()> ──
// 根因：mimi_type_to_llvm(unit)=None 毒化整个容器 lowering → 声明回退 i64
// 签名而函数体发射结构体返回（跨发射器 ABI 错位）→ 调用方 inttoptr 解引用
// 垃圾段错误 / 显示 Some(0)。修复后双后端必须全形状等价。

/// 跨函数返回 Result<(), string> + println（此前 native 段错误 rc=139）。
#[test]
fn usability_result_unit_payload_cross_func_dual() {
    assert_dual(
        r#"
func mk() -> Result<(), string> { Ok(()) }
func main() -> i32 {
    let r = mk()
    println(r)
    0
}
"#,
        "Ok(())",
    );
}

/// match Result<(), string> 的 Ok(_)/Err 臂（此前 E0713 literal-pattern 错误）。
#[test]
fn usability_result_unit_match_arms_dual() {
    assert_dual(
        r#"
func mk() -> Result<(), string> { Ok(()) }
func main() -> i32 {
    let r = mk()
    match r {
        Ok(_) => println("ok-arm")
        Err(e) => println(e)
    }
    0
}
"#,
        "ok-arm",
    );
}

/// 直接调用 + 注解变量两种绑定形态（此前分别印 0 与正确值——分发依赖静态类型串）。
#[test]
fn usability_result_unit_direct_and_annotated_dual() {
    assert_dual(
        r#"
func mk() -> Result<(), i32> { Ok(()) }
func main() -> i32 {
    println(mk())
    let r: Result<(), i32> = Ok(())
    println(r)
    0
}
"#,
        "Ok(())\nOk(())",
    );
}

/// Option<()> 显示透明：Some(()) 不再印 Some(0)，None 正常。
#[test]
fn usability_option_unit_display_dual() {
    assert_dual(
        r#"
func main() -> i32 {
    let a: Option<()> = Some(())
    let b: Option<()> = None
    println(a)
    println(b)
    0
}
"#,
        "Some(())\nNone()",
    );
}

#[test]
fn usability_trap_div_zero_dual_semantics() {
    assert_trap_dual(
        r#"
func main() -> i32 {
    let a = 10
    let b = 0
    println("before")
    let x = a / b
    println(x)
    0
}
"#,
        "before",
        "E0801",
        "division by zero",
    );
}

#[test]
fn usability_trap_overflow_dual_semantics() {
    assert_trap_dual(
        r#"
func main() -> i32 {
    println("before")
    let big = 9223372036854775807
    let x = big + 1
    println(x)
    0
}
"#,
        "before",
        "E0802",
        "overflow",
    );
}
