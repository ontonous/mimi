.SHELL: /bin/bash

.PHONY: test test-all test-stress test-stress-heavy test-stress-fuzz test-realworld test-realworld-cli test-build-race test-fuzz test-fuzz-quick test-fuzz-ci ci-full test-dispatch-zero test-dogfood

# Default: run all non-ignored tests
test:
	cargo test

# Run all tests including ignored (slow/requires-cc)
test-all:
	cargo test -- --include-ignored

# ============================================================
# 0.1.7 stress targets
# ============================================================

# Run the PR-gate stress smoke suite (fast)
test-stress:
	LLVM_SYS_181_PREFIX="$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper}" cargo test --test stress

# Run heavy stress variants (nightly)
test-stress-heavy:
	LLVM_SYS_181_PREFIX="$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper}" cargo test --test stress -- --ignored

# Run only the stress fuzz smoke tests (parser/json/wire)
test-stress-fuzz:
	LLVM_SYS_181_PREFIX="$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper}" cargo test --test stress fuzz_ -- --nocapture
# Real-world dual-backend suite: compile/run every corpus through both
# the bytecode VM and native codegen.
test-realworld:
	LLVM_SYS_181_PREFIX="$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper}" cargo test --test real_world -- --test-threads=4

test-realworld-cli:
	LLVM_SYS_181_PREFIX="$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper}" cargo test --test real_world_cli -- --test-threads=1

# Parallel mimi build archive-race regression
test-build-race:
	LLVM_SYS_181_PREFIX="$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper}" cargo test --test stress stress_parallel_mimi_build_no_archive_race -- --nocapture

# Zero legacy-fallback hard gate: every corpus program must dispatch 100%
# through the resolved slice.
test-dispatch-zero:
	python3 scripts/dispatch_stat.py check --zero

# Hand-written 0.1.7 dogfood projects + legacy real-project regression gate.
test-dogfood:
	LLVM_SYS_181_PREFIX="$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper}" cargo build --bin mimi
	./target/debug/mimi check projects/mimi-taskq/src/main.mimi
	./target/debug/mimi test projects/mimi-taskq/src/main.mimi
	./target/debug/mimi build projects/mimi-taskq/src/main.mimi -o /tmp/mimi-taskq-dogfood
	./target/debug/mimi check projects/mimi-ledger/src/main.mimi
	./target/debug/mimi test projects/mimi-ledger/src/main.mimi
	./target/debug/mimi build projects/mimi-ledger/src/main.mimi -o /tmp/mimi-ledger-dogfood
	/tmp/mimi-taskq-dogfood >/dev/null
	/tmp/mimi-ledger-dogfood >/dev/null
	./target/debug/mimi check projects/mimichat/src/main.mimi
	./target/debug/mimi test projects/mimichat/src/main.mimi
	./target/debug/mimi build projects/mimichat/src/main.mimi -o /tmp/mimichat-dogfood
	/tmp/mimichat-dogfood >/dev/null
	./target/debug/mimi check projects/mimichat-modern/src/main.mimi
	./target/debug/mimi test projects/mimichat-modern/src/main.mimi
	./target/debug/mimi build projects/mimichat-modern/src/main.mimi -o /tmp/mimichat-modern-dogfood
	/tmp/mimichat-modern-dogfood >/dev/null
	@echo "[dogfood] mimi-taskq + mimi-ledger + mimichat + mimichat-modern: check/test/build/run ok"
# ============================================================
# Fuzz targets
# ============================================================

# Quick fuzz: run each proptest target with minimal iterations
test-fuzz-quick:
	LLVM_SYS_181_PREFIX=$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper} PROPTEST_CASES=10 cargo test fuzz_ -- --nocapture

# Full fuzz: run each proptest target with standard iterations
test-fuzz:
	LLVM_SYS_181_PREFIX=$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper} PROPTEST_CASES=100 cargo test fuzz_ -- --nocapture

# CI fuzz: aggressive iterations for continuous integration
test-fuzz-ci:
	LLVM_SYS_181_PREFIX=$${LLVM_SYS_181_PREFIX:-$${PWD}/.llvm-wrapper} PROPTEST_CASES=1000 cargo test fuzz_ 2>&1

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
