from pathlib import Path
import os
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts" / "verify-ordinary-map-v2-l3.sh"


def bash_function(runner_text, function_name):
    match = re.search(
        rf"(?ms)^{re.escape(function_name)}\(\) \{{\n.*?^\}}\n",
        runner_text,
    )
    if match is None:
        raise AssertionError(f"could not find Bash function {function_name}")
    return match.group(0)


class OrdinaryMapAnyV2L3RunnerContractTests(unittest.TestCase):
    def test_dynamic_string_owner_chain_summary_is_exact_and_remains_narrow(self):
        runner = RUNNER.read_text(encoding="utf-8")

        self.assertIn('if [[ "$case_name" == "dynamic-string-keys" ]]; then', runner)
        self.assertIn(
            "'route_profile=ordinary-map-owned-any-v2-computed-string-key-owner-chain-v1'",
            runner,
        )
        self.assertIn("'global_map_any_v2_production_ready=false'", runner)
        self.assertIn("'zero_reachable_required=true'", runner)
        self.assertIn("'expected_stdout=true\\ntrue\\nfalse\\ntrue\\n'", runner)

    def test_strict_runner_pins_external_target_and_locked_serial_builds(self):
        runner = RUNNER.read_text(encoding="utf-8")

        self.assertIn('requires an explicit external CARGO_TARGET_DIR', runner)
        self.assertIn('CARGO_TARGET_DIR="$(realpath -m -- "$CARGO_TARGET_DIR")"', runner)
        self.assertIn('Map v2 L3 Cargo target cannot be inside the evidence directory', runner)
        self.assertIn('Map v2 L3 evidence cannot be inside the Cargo target directory', runner)
        self.assertIn('export CARGO_BUILD_JOBS=1', runner)
        self.assertIn('export CARGO_INCREMENTAL=0', runner)
        self.assertIn('export CARGO_TERM_COLOR=never', runner)
        self.assertEqual(runner.count("    --locked\n"), 7)

    def test_evidence_binds_target_environment_toolchain_and_cargo_config(self):
        runner = RUNNER.read_text(encoding="utf-8")

        self.assertIn("effective.CARGO_TARGET_DIR=%q", runner)
        self.assertIn("env.%s=%q", runner)
        self.assertIn("sha256sum -- \"$resolved_tool\"", runner)
        self.assertIn('"$CARGO_COMMAND" --version --verbose', runner)
        self.assertIn('"$RUSTC_COMMAND" --version --verbose', runner)
        self.assertIn('rustup which "$tool"', runner)
        self.assertIn("llvm_config_libdir=%s", runner)
        self.assertIn("llvm_library_realpath=%s", runner)
        self.assertIn('"${CARGO_HOME:-${HOME:-}/.cargo}/config.toml"', runner)
        self.assertIn(".cargo/config.toml", runner)

    def test_each_sequential_stage_binds_source_environment_and_tool_identity(self):
        runner = RUNNER.read_text(encoding="utf-8")

        self.assertEqual(runner.count("begin_stage_context "), 7)
        self.assertEqual(runner.count("finish_stage_context "), 7)
        self.assertIn("source.before.manifest.sha256", runner)
        self.assertIn("source.after.manifest.sha256", runner)
        self.assertIn("tools.before.txt", runner)
        self.assertIn("tools.after.txt", runner)
        self.assertIn("source_snapshot_stable=true", runner)
        self.assertIn("tool_identity_stable=true", runner)
        self.assertIn("effective.CARGO_PROFILE_DEV_DEBUG=%q", runner)
        self.assertIn("effective.CARGO_PROFILE_TEST_DEBUG=%q", runner)
        self.assertIn("exported_environment_sha256=%s", runner)
        self.assertIn("export CARGO_PROFILE_DEV_DEBUG=0", runner)
        self.assertIn("export CARGO_PROFILE_TEST_DEBUG=0", runner)
        self.assertIn('printf \'effective.CARGO_TARGET_DIR=%q\\n\' "$CARGO_TARGET_DIR"', runner)

    def test_unmatched_fixture_results_are_exact_in_artifact_and_plain_runs(self):
        runner = RUNNER.read_text(encoding="utf-8")

        self.assertIn('"$reported_count" != 4', runner)
        self.assertIn(r"4\ passed\;\ 0\ failed", runner)
        for test_name in (
            "ordinary_map_any_v2_unmatched_dynamic_string_keys_has_active_semantic_coverage",
            "ordinary_map_any_v2_unmatched_record_unit_has_active_semantic_coverage",
            "ordinary_map_any_v2_unmatched_is_empty_control_has_active_semantic_coverage",
            "ordinary_map_any_v2_unmatched_clone_has_active_semantic_coverage",
        ):
            self.assertIn(test_name, runner)
        self.assertIn('"test $expected_test ... ok"', runner)
        self.assertIn("record_unmatched_test_result_validation", runner)
        self.assertIn("unmatched-fixtures-test-result-validation.txt", runner)
        self.assertIn("required Cargo test-result receipt is missing or failed", runner)

    def test_plain_mode_validates_exactly_one_test_and_rejects_zero_test_success(self):
        runner = RUNNER.read_text(encoding="utf-8")
        self.assertEqual(runner.count('run_plain_single_cargo_test "$'), 6)
        self.assertIn(
            'validate_single_cargo_test_result "$captured_output" "$test_name" "$label"',
            runner,
        )

        validator = bash_function(runner, "validate_single_cargo_test_result")
        test_name = "required_exact_test"
        passing_output = (
            "running 1 test\n"
            f"test {test_name} ... ok\n\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
            "42 filtered out; finished in 0.00s\n"
        )
        false_pass_output = (
            "running 0 tests\n\n"
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; "
            "43 filtered out; finished in 0.00s\n"
        )
        with tempfile.TemporaryDirectory() as temp_dir:
            output_path = Path(temp_dir) / "cargo-output.txt"
            for output, expected_status in ((passing_output, 0), (false_pass_output, 1)):
                with self.subTest(expected_status=expected_status):
                    output_path.write_text(output, encoding="utf-8")
                    result = subprocess.run(
                        [
                            "bash",
                            "-c",
                            validator
                            + '\nvalidate_single_cargo_test_result "$1" "$2" contract-test',
                            "bash",
                            str(output_path),
                            test_name,
                        ],
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    self.assertEqual(result.returncode, expected_status, result.stderr)

    def test_suite_log_cannot_alias_artifact_or_cargo_target_tree(self):
        runner = RUNNER.read_text(encoding="utf-8")
        self.assertIn("Map v2 L3 suite log cannot alias the evidence tree", runner)
        self.assertIn("Map v2 L3 suite log cannot be inside the Cargo target tree", runner)

        path_check = bash_function(runner, "path_is_within")
        log_check = bash_function(runner, "validate_log_path_is_external")
        with tempfile.TemporaryDirectory() as temp_dir:
            base = Path(temp_dir)
            root = base / "repo"
            artifact = base / "evidence"
            target = base / "cargo-target"
            script = (
                f'ROOT_DIR="$1"; artifact_dir="$2"; CARGO_TARGET_DIR="$3"\n'
                + path_check
                + log_check
                + '\nvalidate_log_path_is_external "$4"'
            )

            def check_path(candidate):
                return subprocess.run(
                    [
                        "bash",
                        "-c",
                        script,
                        "bash",
                        str(root),
                        str(artifact),
                        str(target),
                        str(candidate),
                    ],
                    capture_output=True,
                    text=True,
                    check=False,
                )

            self.assertNotEqual(check_path(artifact / "suite.log").returncode, 0)
            self.assertNotEqual(check_path(artifact / "stage-context" / "tools.before.txt").returncode, 0)
            self.assertNotEqual(check_path(target / "runner.log").returncode, 0)
            self.assertNotEqual(check_path(root / "runner.log").returncode, 0)
            self.assertEqual(check_path(base / "external.log").returncode, 0)

    def test_rustc_selection_is_bound_and_ambiguous_overrides_fail_closed(self):
        runner = RUNNER.read_text(encoding="utf-8")
        self.assertIn('export RUSTC="$RUSTC_COMMAND"', runner)
        self.assertIn("effective.RUSTC=%q", runner)
        self.assertIn("effective.RUSTC_WRAPPER=<unset-and-rejected-if-inherited>", runner)
        self.assertIn("effective.RUSTC_WORKSPACE_WRAPPER=<unset-and-rejected-if-inherited>", runner)
        self.assertIn("cargo_config_policy=fail-closed-no-applicable-config-files", runner)
        self.assertIn("sha256sum -- \"$resolved\"", runner)

        override_check = bash_function(runner, "reject_rustc_environment_overrides")
        with tempfile.TemporaryDirectory() as temp_dir:
            for variable in (
                "RUSTC",
                "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER",
                "CARGO_BUILD_RUSTC",
                "CARGO_BUILD_RUSTC_WRAPPER",
                "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            ):
                with self.subTest(override=variable):
                    environment = os.environ.copy()
                    environment[variable] = "custom-rustc-or-wrapper"
                    inherited = subprocess.run(
                        [
                            "bash",
                            "-c",
                            override_check + "\nreject_rustc_environment_overrides",
                            "bash",
                        ],
                        env=environment,
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    self.assertNotEqual(inherited.returncode, 0)

            config_functions = (
                bash_function(runner, "cargo_config_candidates")
                + bash_function(runner, "reject_cargo_config_sources")
            )
            root = Path(temp_dir) / "repo"
            cargo_home = Path(temp_dir) / "cargo-home"
            root.mkdir()
            cargo_home.mkdir()
            config_test = (
                'reject_cargo_config_sources "$1" "$2"'
            )
            base_command = [
                "bash",
                "-c",
                config_functions + "\n" + config_test,
                "bash",
                str(root),
                str(cargo_home),
            ]
            self.assertEqual(
                subprocess.run(base_command, capture_output=True, text=True, check=False).returncode,
                0,
            )
            project_config = root / ".cargo" / "config.toml"
            project_config.parent.mkdir()
            project_config.write_text("[build]\nrustc = '/custom/rustc'\n", encoding="utf-8")
            configured = subprocess.run(
                base_command,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(configured.returncode, 0, configured.stderr)

    def test_final_and_early_exit_receipts_are_fail_closed_and_manifest_last(self):
        runner = RUNNER.read_text(encoding="utf-8")

        self.assertIn("result=INCOMPLETE", runner)
        self.assertIn("completion=early_exit", runner)
        self.assertIn("reason=%s\\n", runner)
        self.assertIn("trap 'EXIT_REASON=SIGINT; exit 130' INT", runner)
        self.assertIn("trap 'EXIT_REASON=SIGTERM; exit 143' TERM", runner)
        self.assertIn("completion=complete", runner)
        self.assertIn("stage.map-owner.test_result_validation=%s", runner)
        self.assertIn("stage.unmatched-fixtures.test_result_validation=%s", runner)
        self.assertEqual(runner.count("snapshot_status=0"), 1)
        self.assertEqual(runner.count("validation_status=0"), 1)
        self.assertLess(
            runner.index('} > "$artifact_dir/suite-result.txt"', runner.rindex("final_exit_status=")),
            runner.rindex("write_artifact_manifest"),
        )

    def test_source_snapshot_rejects_symlinked_inputs(self):
        runner = RUNNER.read_text(encoding="utf-8")

        self.assertIn("source tree contains an unsupported symlink", runner)
        self.assertIn("Cargo config tree contains an unsupported symlink", runner)
        self.assertIn("build or toolchain input must not be a symlink", runner)
        self.assertIn("Cargo.toml Cargo.lock build.rs scripts/verify-ordinary-map-v2-l3.sh", runner)


if __name__ == "__main__":
    unittest.main()
