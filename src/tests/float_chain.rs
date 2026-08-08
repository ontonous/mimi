//! 0.35.3 L1 — SD-9 链式末端检查收敛（float_chain pass）回归测试。
//!
//! 覆盖三类语义保持探针：
//!   1. 链中产生非有限 → 末端检查 trap（收敛后仍必须 trap E0813）；
//!   2. 结果 dead（零消费）→ 检查保留（防止 DCE 丢失 trap 语义）；
//!   3. 中间值被比较消费 → 该观察点保留检查；
//!   4. 双后端对等（chain 收敛只发生在 codegen O1 路径，VM 逐点检查，
//!      可观测行为必须一致）。

use crate::tests::*;

/// 链中产生非有限（中间 relaying），末端被消费 → 必须 trap E0813。
/// 收敛前：mul 点 trap；收敛后：add 末端 trap——两者都是 E0813 abort。
#[test]
fn chain_middle_nonfinite_traps_at_chain_end() {
    let src = r#"
func main() -> i32 {
    let a = 1e308
    let b = a * 10.0
    let c = b * 2.0
    println(c)
    0
}
"#;
    // codegen: 收敛后中间检查下沉到末端，仍必须 trap
    let cg = compile_and_run(src);
    let cg_err = cg.err();
    let cg_err = cg_err.unwrap_or_else(|| panic!("codegen must trap on chain non-finite, got Ok"));
    assert!(
        cg_err.contains("E0813") || cg_err.contains("NaN/Inf"),
        "codegen chain trap missing E0813: {}",
        cg_err
    );
    // VM: 逐点检查，中间 b 就 trap
    let vm_err = run_source_bytecode_result(src)
        .expect_err("bytecode must trap on chain non-finite");
    assert!(
        vm_err.contains("E0813") || vm_err.contains("invalid floating-point"),
        "bytecode chain trap missing E0813: {}",
        vm_err
    );
}

/// 结果 dead（绑定后从未消费）→ 检查必须保留：表达式求值 trap 是语言的
/// 语义部分，不能因 DCE 消失（0.35.3 修复的回归：`let y = x * 2.0` 后
/// y 未用，先前链收敛误删检查导致 Inf 偷偷通过）。
#[test]
fn dead_float_result_keeps_check() {
    let src = r#"
func main() -> i32 {
    let x = 1e308 * 10.0
    let y = x * 2.0
    0
}
"#;
    let cg_err = compile_and_run(src)
        .err()
        .unwrap_or_else(|| panic!("codegen dead-result must still trap, got Ok"));
    assert!(
        cg_err.contains("E0813") || cg_err.contains("NaN/Inf"),
        "codegen dead-result trap missing E0813: {}",
        cg_err
    );
    let vm_err = run_source_bytecode_result(src)
        .expect_err("bytecode dead-result must trap");
    assert!(
        vm_err.contains("E0813") || vm_err.contains("invalid floating-point"),
        "bytecode dead-result trap missing E0813: {}",
        vm_err
    );
}

/// 中间值被比较消费（观察点）→ 该点检查保留：NaN 参与比较会改变行为，
/// 不能下沉到链末端。
#[test]
fn compared_middle_value_keeps_check() {
    let src = r#"
func main() -> i32 {
    let a = 1e308
    let b = a * 10.0
    if b > 0.0 {
        println(1)
    } else {
        println(0)
    }
    0
}
"#;
    // b = Inf，b > 0 为 true → println(1)。但 SD-9 语义：b 在比较前必须
    // trap（b 非有限）——VM 与 codegen 一致。
    let vm_err = run_source_bytecode_result(src)
        .expect_err("bytecode compared-middle must trap before compare");
    assert!(
        vm_err.contains("E0813") || vm_err.contains("invalid floating-point"),
        "bytecode compared-middle trap missing E0813: {}",
        vm_err
    );
    let cg_err = compile_and_run(src)
        .err()
        .unwrap_or_else(|| panic!("codegen compared-middle must trap, got Ok"));
    assert!(
        cg_err.contains("E0813") || cg_err.contains("NaN/Inf"),
        "codegen compared-middle trap missing E0813: {}",
        cg_err
    );
}

/// 链收敛在双后端对等下的可观测一致性：合法（有限）链在两端相同输出。
/// （trap 路径的对等由上面三个探针覆盖）
#[test]
fn finite_chain_dual_equivalent() {
    let src = r#"
func main() -> i32 {
    let alpha = 0.01
    let mut y = 0.0
    let mut x = 0.0
    let mut i = 0
    while i < 1000 {
        x = (i as f64) * 0.000001
        y = y + alpha * (x - y)
        i = i + 1
    }
    let out = (y * 1000000.0) as i64
    println(out)
    0
}
"#;
    let expected = "900";
    // 双后端对等：chain 收敛只发生在 codegen O1 路径；可观测输出必须一致
    let (_, interp_out) = run_source_with_stdout(src);
    assert_eq!(interp_out.trim(), expected, "interp stdout mismatch");
    let codegen_out = compile_and_run(src).expect("codegen failed");
    assert_eq!(codegen_out.trim(), expected, "codegen stdout mismatch");
    assert_eq!(interp_out.trim(), codegen_out.trim(), "dual-backend diverge");
}

/// ieee_float 块内的 op 不参与链收敛（其结果不在检查点集合），块外消费
/// 非有限 → 块外检查点 trap。收敛不得让 ieee 块成为"黑洞"。
#[test]
fn ieee_block_consumed_outside_still_traps() {
    let src = r#"
func main() -> i32 {
    let mut v = 0.0
    ieee_float {
        v = 1e308 * 10.0
    }
    let w = v * 2.0
    println(w)
    0
}
"#;
    let vm_err = run_source_bytecode_result(src)
        .expect_err("bytecode: Inf from ieee block consumed outside must trap");
    assert!(
        vm_err.contains("E0813") || vm_err.contains("invalid floating-point"),
        "bytecode ieee-consumed trap missing E0813: {}",
        vm_err
    );
    let cg_err = compile_and_run(src)
        .err()
        .unwrap_or_else(|| panic!("codegen: Inf from ieee block consumed outside must trap"));
    assert!(
        cg_err.contains("E0813") || cg_err.contains("NaN/Inf"),
        "codegen ieee-consumed trap missing E0813: {}",
        cg_err
    );
}