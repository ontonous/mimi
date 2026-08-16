// Flow Event Storm smoke/heavy tests.
use super::{flow_chain_source, run_program};

#[test]
fn stress_flow_event_storm_smoke() {
    // 快速冒烟：200 次 Flow 转移事件，验证链式传输与最终状态正确。
    let source = flow_chain_source(200);
    let out = run_program(&source).expect("flow event storm smoke failed");
    assert_eq!(out.trim(), "200");
}

#[test]
#[ignore = "heavy: 5000 Flow transitions; run explicitly with --ignored"]
fn stress_flow_event_storm_heavy() {
    let source = flow_chain_source(5_000);
    let out = run_program(&source).expect("flow event storm heavy failed");
    assert_eq!(out.trim(), "5000");
}
