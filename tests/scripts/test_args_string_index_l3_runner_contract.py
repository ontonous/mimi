from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/verify-args-string-index-ownership-l3.sh"
TARGET_TEST = ROOT / "tests/args_string_index_ownership.rs"


class ArgsStringIndexL3RunnerContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.runner = RUNNER.read_text(encoding="utf-8")
        cls.target = TARGET_TEST.read_text(encoding="utf-8")

    def test_runner_requires_explicit_strict_external_environment(self) -> None:
        for required in (
            "MIMI_REQUIRE_VALGRIND",
            "MIMI_L3_VALGRIND",
            "MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR",
            "must already be a canonical path",
            "record_tool rustup-cargo \"$rustup_path\" which cargo",
            "record_tool rustup-rustc \"$rustup_path\" which rustc",
            "effective_sha256=",
            "effective_realpath=",
            "RUSTC_WRAPPER and RUSTC_WORKSPACE_WRAPPER must be unset",
            "RUSTUP_TOOLCHAIN=%s",
            "refusing to overwrite existing test-evidence path",
            "refusing to overwrite existing runner-evidence path",
            'runner_dir="${test_artifact_dir}.runner"',
        ):
            self.assertIn(required, self.runner)
        self.assertNotIn('test_artifact_dir="${TMPDIR', self.runner)
        self.assertNotIn('MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR="${TMPDIR', self.runner)

    def test_runner_uses_one_exact_ignored_test_and_a_fresh_target(self) -> None:
        cargo_args = self.runner.split("cargo_arguments=(", 1)[1].split("\n)", 1)[0]
        for required in (
            "--test \"$TEST_TARGET\" \"$TEST_NAME\" --",
            "--exact --ignored --nocapture --test-threads=1",
            "TEST_TARGET=args_string_index_ownership",
            "TEST_NAME=args_string_index_ownership_is_strict_memcheck_clean",
        ):
            self.assertIn(required, cargo_args + self.runner)
        self.assertIn('target_dir="$runner_dir/cargo-target"', self.runner)
        self.assertIn('[[ ! -e "$target_dir" && ! -L "$target_dir" ]] ', self.runner)
        self.assertIn('CARGO_TARGET_DIR="$target_dir"', self.runner)
        self.assertIn('RUSTC="$rustc_path"', self.runner)
        self.assertIn("env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER", self.runner)

    def test_inventory_covers_each_control_and_case_capture(self) -> None:
        for case_id in (
            "args-list-only-control",
            "index-outlives-parent-list-return",
            "index-copy-cleanup-preserves-returned-list",
        ):
            for suffix in (
                ".mimi",
                "-native",
                ".build.stdout",
                ".build.stderr",
                ".route-receipt.txt",
                ".memcheck.log",
                ".memcheck.stdout",
                ".memcheck.stderr",
                ".memcheck-summary.txt",
            ):
                self.assertIn(f'"$case_id{suffix}"', self.runner)
                self.assertIn(case_id, self.target)
        for retained in (
            "valgrind-version.stdout",
            "valgrind-version.stderr",
            "tool-paths.txt",
            "expected-output.bin",
            "test-input.txt",
        ):
            self.assertIn(retained, self.runner)
            self.assertIn(retained, self.target)

    def test_runner_checks_test_result_memcheck_and_route_provenance(self) -> None:
        for required in (
            "test $TEST_NAME ... ok",
            "test result: ok\\. 1 passed; 0 failed; 0 ignored;",
            "definitely lost",
            "indirectly lost",
            "possibly lost",
            "still reachable: 0 bytes in 0 blocks",
            "ERROR SUMMARY: 0 errors from 0 contexts",
            "MIMI_ROUTE_CENSUS canonical-native-build",
            "0 fell back to legacy",
            "route_identity_digest",
            "build-canonical-v1",
        ):
            self.assertIn(required, self.runner)

    def test_snapshots_bind_source_roots_manifests_test_helper_and_runner(self) -> None:
        for required in (
            "find src tests std -type f ! -path '*/__pycache__/*' -print0",
            "Cargo.toml Cargo.lock build.rs",
            ".cargo/config .cargo/config.toml",
            "rust-toolchain rust-toolchain.toml",
            "tests/args_string_index_ownership.rs",
            "tests/support/memcheck_summary.rs",
            "scripts/verify-args-string-index-ownership-l3.sh",
            "source-$1.inputs.nul",
            "source-$1.sha256sums",
            "cmp -s \"$runner_dir/source-before.sha256sums\" \"$runner_dir/source-after.sha256sums\"",
        ):
            self.assertIn(required, self.runner)

    def test_cargo_result_expression_requires_a_single_named_test(self) -> None:
        self.assertRegex(self.runner, re.compile(r"test_line_count=.*grep -Ec '\^test "))
        self.assertIn('[[ "$test_line_count" == 1 ]]', self.runner)
        self.assertIn('[[ "$result_line_count" == 1 ]]', self.runner)


if __name__ == "__main__":
    unittest.main()
