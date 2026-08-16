// ============================================================
// 0.1.7 Phase 0 — high-stress reliability integration tests
// ============================================================
//
// These tests provide the initial `tests/stress/` harness mandated by
// `devdocs/v0.37/high-stress-testing-spec.md`.  Heavy night-run variants are
// marked `#[ignore]`; the non-ignored smoke cases must stay fast enough for
// the PR gate.

#[path = "stress/mod.rs"]
mod stress;
