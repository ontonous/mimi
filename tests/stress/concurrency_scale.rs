// spawn/await scale smoke/heavy tests.
use super::{run_program, spawn_sum_source};

#[test]
fn stress_concurrency_scale_smoke() {
    let source = spawn_sum_source(50);
    let out = run_program(&source).expect("concurrency scale smoke failed");
    assert_eq!(out.trim(), "1225");
}

#[test]
#[ignore = "heavy: 500 spawn/await; run explicitly with --ignored"]
fn stress_concurrency_scale_heavy() {
    let source = spawn_sum_source(500);
    let out = run_program(&source).expect("concurrency scale heavy failed");
    assert_eq!(out.trim(), "124750");
}

#[test]
#[ignore = "heavy: 1000 spawn/await; run explicitly with --ignored"]
fn stress_concurrency_scale_thousand() {
    let source = spawn_sum_source(1000);
    let out = run_program(&source).expect("concurrency scale thousand failed");
    assert_eq!(out.trim(), "499500");
}
