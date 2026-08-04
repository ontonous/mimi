.SHELL: /bin/bash

.PHONY: test test-all test-fuzz test-fuzz-quick test-fuzz-ci ci-full

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

# Run type-soundness property tests
test-typesoundness:
	PROPTEST_CASES=100 cargo test fuzz::target_typesoundness -- --nocapture 2>&1

# Run differential fuzzer (random program generation, compare interp vs codegen)
test-differential:
	PROPTEST_CASES=100 cargo test fuzz::target_differential -- --nocapture --include-ignored 2>&1

test-differential-ci:
	PROPTEST_CASES=1000 cargo test fuzz::target_differential 2>&1

# ============================================================
# CI gates
# ============================================================

ci-check:
	cargo check
	cargo clippy --all-targets -- -D warnings
	cargo fmt -- --check
	python3 scripts/check_language_docs.py
	python3 scripts/check_unsafe_safety.py

ci-test:
	cargo test -- --test-threads=4

ci-valgrind:
	cargo test codegen_e2e dual_backend -- --test-threads=1 --include-ignored

ci-sanitize:
	RUSTFLAGS="-Z sanitizer=address" cargo test codegen_e2e -- --test-threads=1 --include-ignored 2>&1 | tail -3
	RUSTFLAGS="-Z sanitizer=undefined" cargo test codegen_e2e -- --test-threads=1 --include-ignored 2>&1 | tail -3

ci-miri:
	cargo miri test interp ffi -- --test-threads=4

ci-cppcheck:
	cppcheck --enable=all --inconclusive --suppress=missingIncludeSystem src/runtime/mimi_runtime.c 2>&1 || true

test-ffi-contract:
	PROPTEST_CASES=100 cargo test fuzz::target_ffi_contract -- --nocapture --include-ignored 2>&1

ci-full: ci-check ci-test
	$(MAKE) ci-valgrind 2>/dev/null || echo "[SKIP] valgrind not available"
	$(MAKE) ci-cppcheck 2>/dev/null || echo "[SKIP] cppcheck not available"

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
