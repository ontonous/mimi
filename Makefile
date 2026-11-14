SHELL := /bin/bash
.DEFAULT_GOAL := help

# Auto-detect/provision LLVM 18 host wrapper via scripts/setup-llvm-wrapper.sh.
LLVM_SYS_181_PREFIX ?= /tmp/llvm-wrapper
export LLVM_SYS_181_PREFIX
DEV_ENV := ./scripts/dev-env.sh
TEST_THREADS ?= $(shell n=$$(nproc 2>/dev/null || echo 4); [ $$n -gt 16 ] && echo 16 || echo $$n)

.PHONY: help setup check fmt fmt-check clippy build \
	gates-fast gates plan-check ci-check ci-test ci-full ci \
	test test-all test-quick test-z3 test-scripts test-nextest \
	test-stress test-stress-heavy test-stress-fuzz \
	test-realworld test-realworld-cli test-build-race \
	test-dispatch-zero test-dogfood \
	test-fuzz-quick test-fuzz test-fuzz-ci test-fuzz-corpus test-fuzz-dual-path \
	test-typesoundness test-differential test-differential-ci test-fuzz-regression test-ffi-contract \
	ci-valgrind ci-sanitize ci-miri ci-cppcheck clean-temp

help:
	@echo "Mimi Developer Workflow Targets"
	@echo ""
	@echo "  Environment & Fast Checks:"
	@echo "    make setup         Initialize /tmp/llvm-wrapper for LLVM 18"
	@echo "    make check         cargo check --locked --all-targets"
	@echo "    make fmt           Run cargo fmt"
	@echo "    make fmt-check     Verify rustfmt without modifying files"
	@echo "    make clippy        Run cargo clippy --locked --all-targets -- -D warnings"
	@echo "    make gates-fast    Run ~5s static/docs/plan/core-python gates"
	@echo "    make gates         Run all static/audit/fmt/clippy gates (keep-going dashboard)"
	@echo "    make plan-check    Verify devdocs/V0.2/plan.toml structure & links"
	@echo ""
	@echo "  Testing:"
	@echo "    make test          Run suite (uses cargo-nextest if installed, else cargo test)"
	@echo "    make test-quick    Run L1 dual_ + L2 typecheck:: + ffi_ smoke checks"
	@echo "    make test-z3       Run Z3 verification tests single-threaded (arena OOM guard)"
	@echo "    make test-scripts  Run offline Python tooling & contract tests"
	@echo "    make test-nextest  Run test suite explicitly via cargo-nextest"
	@echo "    make ci            Run full local CI parity pipeline (gates + tests + perf)"
	@echo ""
	@echo "  Specialized Suites (preserved):"
	@echo "    make test-stress / test-realworld / test-dogfood / test-dispatch-zero / clean-temp"

setup:
	@$(DEV_ENV) true

check: setup
	$(DEV_ENV) cargo check --locked --all-targets

build: setup
	$(DEV_ENV) cargo build --locked --workspace

fmt:
	cargo fmt

fmt-check:
	cargo fmt -- --check

clippy: setup
	$(DEV_ENV) cargo clippy --locked --all-targets -- -D warnings

gates-fast:
	bash scripts/run-gates.sh --fast

gates: setup
	bash scripts/run-gates.sh --lint

plan-check:
	python3 devdocs/V0.2/tools/check_plan.py --ready

# Default: run all non-ignored tests (prefer cargo-nextest when available)
test: setup
	@if command -v cargo-nextest >/dev/null 2>&1 && [ "$${MIMI_NO_NEXTEST:-0}" != "1" ]; then \
		ulimit -v 20000000 && $(DEV_ENV) cargo nextest run --locked; \
	else \
		ulimit -v 20000000 && $(DEV_ENV) cargo test --locked -- --test-threads=$(TEST_THREADS); \
	fi

test-nextest: setup
	ulimit -v 20000000 && $(DEV_ENV) cargo nextest run --locked

# Run all tests including ignored (slow/requires-cc/valgrind)
test-all: setup
	$(DEV_ENV) cargo test --locked -- --include-ignored --test-threads=1

test-quick: setup
	$(DEV_ENV) cargo test --locked dual_ -- --test-threads=2
	$(DEV_ENV) cargo test --locked typecheck:: -- --test-threads=2
	$(DEV_ENV) cargo test --locked ffi_ -- --test-threads=2

test-z3: setup
	$(DEV_ENV) cargo test --locked v1_2_verification -- --test-threads=1

test-scripts:
	bash scripts/run-gates.sh --python-core

# ============================================================
# 0.1.7+ specialized suites (preserved)
# ============================================================

test-stress: setup
	$(DEV_ENV) cargo test --test stress

test-stress-heavy: setup
	$(DEV_ENV) cargo test --test stress -- --ignored

test-stress-fuzz: setup
	$(DEV_ENV) cargo test --test stress fuzz_ -- --nocapture

test-realworld: setup
	$(DEV_ENV) cargo test --test real_world -- --test-threads=4

test-realworld-cli: setup
	$(DEV_ENV) cargo test --test real_world_cli -- --test-threads=1

test-build-race: setup
	$(DEV_ENV) cargo test --test stress stress_parallel_mimi_build_no_archive_race -- --nocapture

test-dispatch-zero: setup
	$(DEV_ENV) python3 scripts/dispatch_stat.py check --zero

test-dogfood: setup
	$(DEV_ENV) cargo build --bin mimi
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

test-fuzz-quick: setup
	PROPTEST_CASES=10 $(DEV_ENV) cargo test fuzz_ -- --nocapture

test-fuzz: setup
	PROPTEST_CASES=100 $(DEV_ENV) cargo test fuzz_ -- --nocapture

test-fuzz-ci: setup
	PROPTEST_CASES=1000 $(DEV_ENV) cargo test fuzz_ 2>&1

test-fuzz-corpus: setup
	$(DEV_ENV) cargo test fuzz::corpus -- --nocapture

test-fuzz-dual-path: setup
	$(DEV_ENV) cargo test fuzz::test_dual_path -- --ignored --nocapture

test-typesoundness: setup
	PROPTEST_CASES=100 $(DEV_ENV) cargo test fuzz::target_typesoundness -- --nocapture 2>&1

test-differential: setup
	PROPTEST_CASES=100 $(DEV_ENV) cargo test fuzz::target_differential -- --nocapture --include-ignored 2>&1

test-differential-ci: setup
	PROPTEST_CASES=1000 $(DEV_ENV) cargo test fuzz::target_differential 2>&1

test-fuzz-regression: setup
	$(DEV_ENV) cargo test fuzz::corpus -- --nocapture
	$(DEV_ENV) cargo test fuzz::test_exhaustive -- --nocapture
	$(DEV_ENV) cargo test fuzz::test_cap -- --nocapture
	$(DEV_ENV) cargo test fuzz::test_ffi -- --nocapture
	$(DEV_ENV) cargo test target_parser -- --nocapture
	$(DEV_ENV) cargo test target_typechecker -- --nocapture
	$(DEV_ENV) cargo test target_interpreter -- --nocapture
	$(DEV_ENV) cargo test target_codegen -- --nocapture

test-ffi-contract: setup
	PROPTEST_CASES=100 $(DEV_ENV) cargo test fuzz::target_ffi_contract -- --nocapture --include-ignored 2>&1

# ============================================================
# CI gates (aligned with .github/workflows/ci.yml)
# ============================================================

ci-check: setup
	bash scripts/run-gates.sh --lint --fail-fast

ci-test: setup
	ulimit -v 20000000 && $(DEV_ENV) cargo test --locked -- --test-threads=$(TEST_THREADS)

ci: ci-check test-quick test-z3 ci-test
	bash scripts/perf-gate.sh

ci-valgrind: setup
	$(DEV_ENV) cargo test codegen_e2e -- --test-threads=1 --include-ignored
	$(DEV_ENV) cargo test dual_backend -- --test-threads=1 --include-ignored

ci-sanitize: setup
	RUSTFLAGS="-Z sanitizer=address" $(DEV_ENV) cargo test codegen_e2e -- --test-threads=1 --include-ignored 2>&1 | tail -3
	RUSTFLAGS="-Z sanitizer=undefined" $(DEV_ENV) cargo test codegen_e2e -- --test-threads=1 --include-ignored 2>&1 | tail -3

ci-miri: setup
	$(DEV_ENV) cargo miri test interp ffi -- --test-threads=4

ci-cppcheck:
	cppcheck --enable=all --inconclusive --suppress=missingIncludeSystem src/runtime/mimi_runtime.c 2>&1 || true

ci-full: ci-check ci-test
	$(MAKE) ci-valgrind 2>/dev/null || echo "[SKIP] valgrind not available"
	$(MAKE) ci-cppcheck 2>/dev/null || echo "[SKIP] cppcheck not available"

clean-temp:
	rm -f /tmp/mimi-*-dogfood /tmp/perf_gate_* /tmp/mimi_runtime_test.a
	find . -type d -name "__pycache__" -not -path "./.git/*" -exec rm -rf {} + 2>/dev/null || true
