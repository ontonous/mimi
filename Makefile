.PHONY: test test-all test-fuzz test-fuzz-quick test-fuzz-ci

# Default: run all non-ignored tests
test:
	cargo test

# Run all tests including ignored (slow/requires-cc)
test-all:
	cargo test -- --include-ignored

# ============================================================
# Fuzz targets
# ============================================================

# Quick fuzz: run each proptest target with minimal iterations
test-fuzz-quick:
	PROPTEST_CASES=10 cargo test fuzz_ -- --nocapture

# Full fuzz: run each proptest target with standard iterations
test-fuzz:
	PROPTEST_CASES=100 cargo test fuzz_ -- --nocapture

# CI fuzz: aggressive iterations for continuous integration
test-fuzz-ci:
	PROPTEST_CASES=1000 cargo test fuzz_ 2>&1

# Run all fuzz corpus seed tests
test-fuzz-corpus:
	cargo test fuzz::corpus -- --nocapture

# Run dual-path consistency tests (requires cc)
test-fuzz-dual-path:
	cargo test fuzz::test_dual_path -- --ignored --nocapture

# ============================================================
# Quick smoke-test (no proptest, just corpus + regression)
# ============================================================
test-fuzz-regression:
	cargo test fuzz::corpus -- --nocapture
	cargo test fuzz::test_exhaustive -- --nocapture
	cargo test fuzz::test_cap -- --nocapture
	cargo test fuzz::test_ffi -- --nocapture
	cargo test target_parser -- --nocapture
	cargo test target_typechecker -- --nocapture
	cargo test target_interpreter -- --nocapture
	cargo test target_codegen -- --nocapture
