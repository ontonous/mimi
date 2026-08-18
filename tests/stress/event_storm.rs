// Flow Event Storm smoke/heavy tests.
use super::{build_and_run_native, flow_chain_source, run_program};

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

/// Generate a native-compiled Flow program that performs `n` positive-state
/// transitions through a tail-recursive driver function.
///
/// The recursive driver gives each iteration a fresh linear state binding, so
/// it is accepted by the checker while still letting the native codegen run a
/// real 10,000,000-event storm without source expansion.
fn flow_tailrec_native_source(n: i64) -> String {
    format!(
        r#"flow Counter {{
    state Zero {{ count: i32 }}
    state Positive {{ count: i32 }}
    transition inc(Zero) -> Positive {{
        return Positive {{ count: self.count + 1 }}
    }}
    transition inc(Positive) -> Positive {{
        return Positive {{ count: self.count + 1 }}
    }}
}}

func runP(s: Positive, n: i32) -> i32 {{
    if n == 0 {{
        let c = s.count
        drop(s)
        c
    }} else {{
        runP(Counter::inc(s), n - 1)
    }}
}}

func main() -> i32 {{
    let s0 = Counter::inc(Zero {{ count: 0 }})
    println(runP(s0, {n}))
    0
}}
"#
    )
}

#[test]
fn stress_flow_event_storm_native_smoke() {
    // PR gate scale: 100,000 events through the native-compiled Flow driver.
    let source = flow_tailrec_native_source(100_000);
    let out = build_and_run_native(&source).expect("100K flow event storm native smoke failed");
    assert_eq!(out.trim(), "100001");
}

#[test]
#[ignore = "heavy: 10,000,000 Flow transitions through native tail-recursive driver; run explicitly with --ignored"]
fn stress_flow_event_storm_10m_native_heavy() {
    // DoD: single Flow instance receives 10,000,000 events with deterministic
    // ordering and no state tearing. Native codegen turns the tail-recursive
    // driver into a tight loop, so this stays fast enough for nightly heavy CI.
    let source = flow_tailrec_native_source(10_000_000);
    let out = build_and_run_native(&source).expect("10M flow event storm native failed");
    // One initial Zero -> Positive transition plus 10,000,000 Positive -> Positive.
    assert_eq!(out.trim(), "10000001");
}
