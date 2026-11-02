from __future__ import annotations

import hashlib
import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/verify-actor-two-fixture-current.sh"


class ActorTwoFixtureRunnerSourceContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source = RUNNER.read_text(encoding="utf-8")

    def test_route_tests_record_the_production_main_thread_stack_budget(self) -> None:
        self.assertIn("TEST_THREAD_STACK_BYTES=8388608", self.source)
        self.assertIn("effective.RUST_MIN_STACK=%s\\n", self.source)
        self.assertIn('env RUST_MIN_STACK="$TEST_THREAD_STACK_BYTES"', self.source)
        self.assertIn('printf \'RUST_MIN_STACK=%s\\nLLVM_SYS_181_PREFIX=%s\\n\'', self.source)

    def test_legacy_owner_audit_script_is_a_required_source_input(self) -> None:
        required_sources_start = self.source.index("local -a required_sources=(")
        required_sources_end = self.source.index("\n    )", required_sources_start)
        required_sources = self.source[required_sources_start:required_sources_end]
        self.assertIn("scripts/audit-mir-legacy-owners.sh", required_sources)

    def test_required_inputs_are_validated_and_hashed_by_before_after_snapshots(self) -> None:
        inventory_start = self.source.index("source_input_inventory() {")
        inventory_end = self.source.index("\n}\n\nsource_snapshot()", inventory_start)
        inventory = self.source[inventory_start:inventory_end]
        self.assertIn('if [[ ! -f "$required_path" || -L "$required_path" ]]', inventory)
        self.assertIn("printf '%s\\0' \"$required_path\"", inventory)

        snapshot_start = self.source.index("source_snapshot() {")
        snapshot_end = self.source.index("\n}\n\nvalidate_cargo_target_configuration()", snapshot_start)
        snapshot = self.source[snapshot_start:snapshot_end]
        self.assertIn('! sha256sum -- "$source_path" >> "$snapshot_tmp"', snapshot)
        self.assertIn('! cmp -s "$inventory_path" "$verify_inventory_path"', snapshot)

    def test_legacy_owner_audit_is_hashed_in_both_outer_source_snapshots(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "snapshot-root"
            inventory = self._portable_inventory(root, local_context={})
            audit_path = "scripts/audit-mir-legacy-owners.sh"
            self.assertIn(f"{audit_path}\0".encode("utf-8"), inventory)

            inventory_start = self.source.index("source_input_inventory() {")
            inventory_end = self.source.index("\n}\n\nsource_snapshot()", inventory_start) + 2
            snapshot_start = self.source.index("source_snapshot() {")
            snapshot_end = self.source.index("\n}\n\nvalidate_cargo_target_configuration()", snapshot_start) + 2
            functions = self.source[inventory_start:inventory_end] + "\n" + self.source[snapshot_start:snapshot_end]
            command = (
                f"cd {shlex.quote(str(root))}\n"
                f"ROOT_DIR={shlex.quote(str(root))}\n"
                'SCRIPT_RELATIVE="scripts/verify-actor-two-fixture-current.sh"\n'
                + functions
                + "\nsource_snapshot source.before.inputs.nul source.before.sha256 && "
                + "source_snapshot source.after.inputs.nul source.after.sha256\n"
            )
            result = subprocess.run(
                ["bash", "-c", command],
                cwd=root,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))

            expected_hash = hashlib.sha256((root / audit_path).read_bytes()).hexdigest()
            expected_row = f"{expected_hash}  {audit_path}".encode("utf-8")
            for suffix in ("before", "after"):
                captured_inventory = (root / f"source.{suffix}.inputs.nul").read_bytes()
                captured_hashes = (root / f"source.{suffix}.sha256").read_bytes().splitlines()
                self.assertIn(f"{audit_path}\0".encode("utf-8"), captured_inventory)
                self.assertIn(expected_row, captured_hashes)

    def _portable_inventory(self, root: Path, *, local_context: dict[str, str]) -> bytes:
        roots = ("src", "tests", "std")
        for name in roots:
            (root / name).mkdir(parents=True, exist_ok=True)
            (root / name / "portable-input.txt").write_text(name, encoding="utf-8")
        required = (
            "Cargo.toml",
            "Cargo.lock",
            "build.rs",
            "scripts/verify-actor-two-fixture-current.sh",
            "scripts/full_route_census.py",
            "scripts/actor_census_build_receipt.py",
            "scripts/generate_route_census_authority.py",
            "scripts/audit-mir-legacy-owners.sh",
            "tests/scripts/test_full_route_census.py",
            "tests/scripts/test_actor_census_build_receipt.py",
            "tests/scripts/test_actor_two_fixture_runner_contract.py",
            "src/core/mir/actor_tests.rs",
            "src/core/mir/route.rs",
            "src/core/resolved/tests.rs",
            "src/main/canonical_dispatch.rs",
            "src/tests/dual_backend.rs",
            "tests/real_world_cli.rs",
            "tests/real_world/actor_nested_func_capture.mimi",
            "tests/real_world/actor_nested_func_shadow.mimi",
        )
        for relative in required:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("portable test input\n", encoding="utf-8")
        for relative, content in local_context.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")

        start = self.source.index("source_input_inventory() {")
        end = self.source.index("\n}\n\nsource_snapshot()", start) + 2
        function = self.source[start:end]
        command = (
            f"cd {shlex.quote(str(root))}\n"
            f"ROOT_DIR={shlex.quote(str(root))}\n"
            'SCRIPT_RELATIVE="scripts/verify-actor-two-fixture-current.sh"\n'
            + function
            + "\nsource_input_inventory\n"
        )
        result = subprocess.run(
            ["bash", "-c", command],
            cwd=root,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        return result.stdout

    def test_local_agent_and_devdocs_context_does_not_change_portable_source_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            absent = self._portable_inventory(root / "absent", local_context={})
            agent_only = self._portable_inventory(
                root / "agent-only", local_context={"AGENTS.md": "local instructions\n"}
            )
            all_context = self._portable_inventory(
                root / "all-context",
                local_context={
                    "AGENTS.md": "different local instructions\n",
                    "devdocs/v0.41/actor-closure-0.1.11.md": "untracked actor notes\n",
                    "devdocs/V0.2/09-0.1.11交接门禁.md": "untracked hand-off\n",
                },
            )

        self.assertEqual(absent, agent_only)
        self.assertEqual(absent, all_context)
        for optional_path in (
            "AGENTS.md",
            "devdocs/v0.41/actor-closure-0.1.11.md",
            "devdocs/V0.2/09-0.1.11交接门禁.md",
        ):
            self.assertNotIn(optional_path.encode("utf-8"), absent)

    def test_optional_context_is_recorded_separately_from_portable_source_hashes(self) -> None:
        self.assertIn("portable contract is defined by the versioned runner", self.source)
        self.assertIn("non-contract review context", self.source)
        self.assertIn("optional_review_context_excluded_from_portable_source_identity", self.source)
        self.assertIn("local_context.%s=present_sha256=", self.source)
        self.assertIn("local_context.%s=absent", self.source)
        required_sources_start = self.source.index("local -a required_sources=(")
        required_sources_end = self.source.index("\n    )", required_sources_start)
        required_sources = self.source[required_sources_start:required_sources_end]
        for optional_path in (
            "AGENTS.md",
            "devdocs/v0.41/actor-closure-0.1.11.md",
            "devdocs/V0.2/09-0.1.11交接门禁.md",
        ):
            self.assertNotIn(optional_path, required_sources)

    def test_clean_checkout_without_local_agent_or_hand_off_context_is_supported(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "clean-checkout"
            inventory = self._portable_inventory(root, local_context={})
        self.assertTrue(inventory)
        for optional_path in (
            "AGENTS.md",
            "devdocs/v0.41/actor-closure-0.1.11.md",
            "devdocs/V0.2/09-0.1.11交接门禁.md",
        ):
            self.assertFalse((root / optional_path).exists())
            self.assertNotIn(f"{optional_path}\0".encode("utf-8"), inventory)

    def test_census_runner_binds_two_reports_to_one_source_receipt(self) -> None:
        self.assertIn("--build-receipt", self.source)
        self.assertEqual(self.source.count("--build-receipt \"$ARTIFACT_DIR/commands/current-source-mimi-build/build-receipt.json\""), 2)
        for required_fragment in (
            "capture-source-snapshot",
            "source.before.json",
            "source.after.json",
            "cargo-environment.json",
            "rustup which cargo",
            "capture-cargo-identity",
            "cargo-identity.before.json",
            "cargo-identity.after.json",
            "capture-cargo-config",
            "cargo-config.before.json",
            "cargo-config.after.json",
            "fresh-target.txt",
            "build-receipt.json",
            "source-scope-reconciliation.json",
            "source_snapshot_end",
            "scripts/audit-mir-legacy-owners.sh",
        ):
            self.assertIn(required_fragment, self.source)
        self.assertIn("record_build_helper environment-capture", self.source)
        self.assertIn('"$gate_dir/$evidence_name.argv.nul"', self.source)
        self.assertIn("outer_before", self.source)
        self.assertIn("build_receipt.reconcile_source_manifests(", self.source)

    def test_census_requires_one_independent_v4_manifest_before_cargo_and_reuses_it(self) -> None:
        preflight = self.source.index("# Require a caller-reviewed expectation contract")
        cargo_version = self.source.index("cargo --version")
        first_expensive_gate = self.source.index("record_gate checker-owner-effects-l1")
        capture_start = self.source.index("capture_environment() {")
        capture_end = self.source.index("\n}\n", capture_start) + 2
        capture_environment = self.source[capture_start:capture_end]
        self.assertNotIn("cargo --version", capture_environment)
        self.assertLess(preflight, cargo_version)
        self.assertLess(preflight, first_expensive_gate)
        for required_fragment in (
            "MIMI_ACTOR_TWO_FIXTURE_EXPECTATIONS must name an independent schema-v4 expectations manifest",
            "census.load_expectations(manifest_path, root)",
            "set(declared) != source_paths",
            "expectation.get(\"class\") == \"unmapped\"",
            "expectation.get(\"source_sha256\") != source.get(\"sha256\")",
            "census.expectation_source_contract_valid_for_preflight(expectation)",
            "census.expectation_paths_requiring_runtime_authority(bound)",
            '"runtime_authority_pending_count"',
            "--expectations \"$CENSUS_EXPECTATIONS_PATH\"",
            "--report \"$ARTIFACT_DIR/full-route-census.initial.json\"",
            "bootstrap_observation authority bundles",
            "_provided_expectations_identity_matches(",
            "expected_sha256=expectations_sha256",
        ):
            self.assertIn(required_fragment, self.source)
        self.assertEqual(
            self.source.count('--expectations "$CENSUS_EXPECTATIONS_PATH"'), 2
        )
        self.assertNotIn("--bootstrap-report", self.source)
        initial_args = self.source[
            self.source.index("initial_census_args=("):
            self.source.index("generator_args=(")
        ]
        strict_args = self.source[
            self.source.index("strict_census_args=("):
            self.source.index(")\n\ninitial_census_ok=", self.source.index("strict_census_args=("))
        ]
        self.assertIn("--expectations \"$CENSUS_EXPECTATIONS_PATH\"", initial_args)
        self.assertIn("--expectations \"$CENSUS_EXPECTATIONS_PATH\"", strict_args)
        self.assertIn("--check", strict_args)

    def test_missing_expectations_exits_before_invoking_cargo(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_root = Path(temporary)
            cargo_bin = temporary_root / "bin"
            cargo_bin.mkdir()
            cargo_log = temporary_root / "cargo-invocations.log"
            cargo_shim = cargo_bin / "cargo"
            cargo_shim.write_text(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CENSUS_TEST_CARGO_LOG\"\n"
                "exit 91\n",
                encoding="utf-8",
            )
            cargo_shim.chmod(0o755)
            artifact_dir = temporary_root / "evidence"
            env = os.environ.copy()
            env.pop("MIMI_ACTOR_TWO_FIXTURE_EXPECTATIONS", None)
            env["MIMI_ACTOR_TWO_FIXTURE_ARTIFACT_DIR"] = str(artifact_dir)
            env["CENSUS_TEST_CARGO_LOG"] = str(cargo_log)
            env["PATH"] = f"{cargo_bin}{os.pathsep}{env.get('PATH', '')}"

            result = subprocess.run(
                ["bash", str(RUNNER)],
                cwd=ROOT,
                env=env,
                capture_output=True,
                text=True,
                check=False,
                timeout=60,
            )

        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("independent schema-v4 expectations", result.stderr)
        self.assertFalse(cargo_log.exists(), "Cargo must not be executed before preflight")

    def test_actor_strict_report_rejects_bootstrap_generation_mode(self) -> None:
        validator_start = self.source.index("bundle = mapping(report.get(\"generated_authority_bundle\"))")
        validator_end = self.source.index("for path, (test_id, expected_stdout) in expected.items():", validator_start)
        validator = self.source[validator_start:validator_end]
        self.assertIn('bundle.get("generation_mode") != "strict_authority"', validator)
        self.assertIn("Actor gate rejects bootstrap_observation authority bundles", validator)

    def test_census_receipt_is_created_only_after_build_source_snapshots(self) -> None:
        before = self.source.index("source-before-capture")
        cargo = self.source.index('"${cargo_env[@]}" "$cargo_path" "${cargo_args[@]}"')
        after = self.source.index("source-after-capture", cargo)
        receipt = self.source.index("receipt_command=(", after)
        self.assertLess(before, cargo)
        self.assertLess(cargo, after)
        self.assertLess(after, receipt)

    def test_build_failure_reasons_append_under_noclobber(self) -> None:
        build_start = self.source.index("record_build() {")
        build_end = self.source.index("\nrecord_python_step() {", build_start)
        build = self.source[build_start:build_end]
        self.assertIn("set -o noclobber", self.source)
        self.assertIn(
            'printf \'FAIL current-source mimi build exit=%s\\n\' "$status" >> "$gate_dir/validation.txt"',
            build,
        )
        self.assertIn(
            'printf \'FAIL source-bound Mimi build receipt did not validate\\n\' \\\n                >> "$gate_dir/validation.txt"',
            build,
        )

    def test_selected_cargo_identity_and_config_are_captured_around_build(self) -> None:
        identity_before = self.source.index("cargo-identity-before-capture")
        config_before = self.source.index("cargo-config-before-capture")
        cargo = self.source.index('"${cargo_env[@]}" "$cargo_path" "${cargo_args[@]}"')
        identity_after = self.source.index("cargo-identity-after-capture", cargo)
        config_after = self.source.index("cargo-config-after-capture", cargo)
        self.assertLess(identity_before, cargo)
        self.assertLess(config_before, cargo)
        self.assertLess(cargo, identity_after)
        self.assertLess(cargo, config_after)

    def test_receipt_build_uses_the_rustup_selected_cargo_binary(self) -> None:
        build_start = self.source.index("record_build() {")
        cargo_invocation = self.source.index('"${cargo_env[@]}" "$cargo_path" "${cargo_args[@]}"', build_start)
        prologue = self.source[build_start:cargo_invocation]
        self.assertIn('cargo_path="$(rustup which cargo)"', prologue)
        self.assertIn('realpath -e -- "$cargo_path"', prologue)
        self.assertIn('"${cargo_path##*/}" != cargo', prologue)
        self.assertNotIn('cargo_path="$(command -v cargo)"', prologue)


if __name__ == "__main__":
    unittest.main()
