from __future__ import annotations

import base64
import json
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
import full_route_census as census


def _actor_helper_source() -> str:
    source = (census.ROOT / census.ACTOR_HELPER_TEST_PATH).read_text(encoding="utf-8")
    span = census._rust_named_function_span(source, census.ACTOR_HELPER_NAME)
    if span is None:
        raise AssertionError("repository Actor helper is not a top-level function")
    return source[span[0] : span[1]]


EXPECTED_REJECTION_ROUTE = {
    "disposition": "rejected",
    "classification_status": "observed",
    "reason": "expected diagnostic",
}


class InventoryTests(unittest.TestCase):
    def test_manifest_contains_tracked_missing_and_untracked_sources(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "tests/real_world").mkdir(parents=True)
            (root / "std").mkdir()
            (root / "docs/staging").mkdir(parents=True)
            (root / "target/ignored").mkdir(parents=True)
            (root / "tests/real_world/app.mimi").write_text(
                "func main() -> i32 { 0 }\n", encoding="utf-8"
            )
            (root / "std/list.mimi").write_text(
                "pub func size(xs: List<i32>) -> i32 { 0 }\n", encoding="utf-8"
            )
            (root / "docs/staging/known-gap-example.mimi").write_text(
                "// known gap: filename and comment do not define an expectation\n"
                "func main() { }\n",
                encoding="utf-8",
            )
            (root / "target/ignored/generated.mimi").write_text(
                "func main() { }\n", encoding="utf-8"
            )
            try:
                (root / "tests/real_world/alias.mimi").symlink_to("app.mimi")
                (root / "linked-stdlib").symlink_to(root / "std", target_is_directory=True)
            except OSError:
                pass

            records = census.enumerate_sources(
                root,
                ["std/list.mimi", "tests/real_world/deleted.mimi", "target/intentional.mimi"],
            )
            excluded_symlinks = census.source_symlink_exclusions(root)

        self.assertEqual(
            [row["path"] for row in records],
            [
                "docs/staging/known-gap-example.mimi",
                "std/list.mimi",
                "target/intentional.mimi",
                "tests/real_world/alias.mimi",
                "tests/real_world/app.mimi",
                "tests/real_world/deleted.mimi",
            ],
        )
        by_path = {row["path"]: row for row in records}
        self.assertEqual(by_path["std/list.mimi"]["category"], "stdlib_source")
        self.assertEqual(by_path["std/list.mimi"]["target_kind"], "shared_library_probe")
        self.assertEqual(by_path["docs/staging/known-gap-example.mimi"]["target_kind"], "executable")
        self.assertEqual(by_path["target/intentional.mimi"]["presence_kind"], "missing_tracked_path")
        self.assertFalse(by_path["target/intentional.mimi"]["present"])
        self.assertTrue(by_path["target/intentional.mimi"]["tracked_by_git"])
        self.assertEqual(by_path["tests/real_world/alias.mimi"]["presence_kind"], "symlink")
        self.assertEqual(by_path["tests/real_world/app.mimi"]["category"], "real_world_test")
        self.assertEqual(by_path["tests/real_world/app.mimi"]["target_kind"], "executable")
        self.assertNotIn("target/ignored/generated.mimi", [row["path"] for row in records])
        self.assertEqual(
            {item["kind"] for item in excluded_symlinks},
            {"directory_symlink_not_traversed", "mimi_file_symlink_not_followed"},
        )

    def test_git_inventory_records_exact_tracked_path_list(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            completed = subprocess.CompletedProcess(
                args=["git", "ls-files"],
                returncode=0,
                stdout=b"src/a.mimi\0tests/b.mimi\0",
                stderr=b"",
            )
            with patch.object(census.subprocess, "run", return_value=completed):
                paths, identity = census.tracked_mimi_paths(root)
        self.assertEqual(paths, ["src/a.mimi", "tests/b.mimi"])
        self.assertEqual(identity["status"], "available")
        self.assertEqual(identity["path_count"], 2)
        self.assertEqual(identity["paths_sha256"], census._manifest_paths_hash(paths))

    def test_source_scope_accounts_for_candidates_and_preserves_archival_copies(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            contents = {
                "tests/fixtures/known_rejected_shape.mimi": "func main() { }\n",
                "nested/evidence/active.mimi": "func main() { }\n",
                "devdocs/archive/copy.mimi": "func main() { }\n",
                "evidence/run/copy.mimi": "func main() { }\n",
                "target/tracked.mimi": "func main() { }\n",
                "target/untracked.mimi": "func main() { }\n",
            }
            for name, content in contents.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")
            tracked = [
                "devdocs/archive/copy.mimi",
                "devdocs/archive/deleted.mimi",
                "target/tracked.mimi",
            ]
            sources, exclusions, scope = census.enumerate_source_scope(root, tracked)
            source_paths = [row["path"] for row in sources]
            exclusion_by_path = {row["path"]: row for row in exclusions}

            self.assertEqual(
                source_paths,
                [
                    "nested/evidence/active.mimi",
                    "target/tracked.mimi",
                    "tests/fixtures/known_rejected_shape.mimi",
                ],
            )
            self.assertEqual(
                set(exclusion_by_path),
                {
                    "devdocs/archive/copy.mimi",
                    "devdocs/archive/deleted.mimi",
                    "evidence/run/copy.mimi",
                },
            )
            self.assertTrue(scope["exactly_once"])
            self.assertEqual(scope["candidate_count"], 3)
            self.assertEqual(scope["excluded_count"], 3)
            self.assertTrue(all(row["route_candidate"] for row in sources))
            self.assertTrue(
                all(not row["route_candidate"] for row in exclusions)
            )
            self.assertEqual(
                exclusion_by_path["devdocs/archive/copy.mimi"]["sha256"],
                census.sha256_bytes(contents["devdocs/archive/copy.mimi"].encode()),
            )
            self.assertEqual(
                exclusion_by_path["devdocs/archive/deleted.mimi"]["presence_kind"],
                "missing_tracked_path",
            )
            self.assertEqual(
                exclusion_by_path["evidence/run/copy.mimi"]["scope_classification"],
                "preserved_evidence_source_copy",
            )
            self.assertTrue((root / "evidence/run/copy.mimi").is_file())
            self.assertTrue((root / "devdocs/archive/copy.mimi").is_file())
            self.assertFalse((root / "target/untracked.mimi").relative_to(root).as_posix() in source_paths)

    def test_tracked_source_behind_symlinked_parent_is_blocked_and_gates_fail(self) -> None:
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as outside:
            root = Path(temporary)
            external = Path(outside)
            (external / "foo.mimi").write_text(
                "func main() { println(99) }\n", encoding="utf-8"
            )
            (root / "target").symlink_to(external, target_is_directory=True)

            sources, exclusions, scope = census.enumerate_source_scope(
                root, ["target/foo.mimi"]
            )
            symlink_exclusions = census.source_symlink_exclusions(root)
            blockers = census.tracked_symlink_parent_blockers(
                root, ["target/foo.mimi"]
            )
            record = sources[0]
            gate = StrictGateTests._gate(
                ["target/foo.mimi"],
                [StrictGateTests._record("target/foo.mimi")],
                excluded_symlinks=tuple(blockers),
                source_scope=scope,
            )

        self.assertEqual([row["path"] for row in sources], ["target/foo.mimi"])
        self.assertEqual(record["presence_kind"], "blocked_by_symlinked_parent")
        self.assertFalse(record["present"])
        self.assertFalse(record["buildable"])
        self.assertIsNone(record["sha256"])
        self.assertEqual(record["symlink_parent_path"], "target")
        self.assertTrue(scope["exactly_once"])
        self.assertEqual(len(exclusions), 0)
        self.assertIn(
            ("target", "directory_symlink_not_traversed"),
            {(item["path"], item["kind"]) for item in symlink_exclusions},
        )
        self.assertEqual(blockers[0]["kind"], "tracked_mimi_path_parent_symlink_not_followed")
        self.assertFalse(
            gate["checks"]["no_tracked_mimi_paths_through_symlinked_parents"]
        )
        self.assertEqual(
            gate["tracked_symlink_parent_blockers"][0]["tracked_paths"],
            ["target/foo.mimi"],
        )

    def test_actor_fixtures_receive_nonempty_hash_and_test_authority_mappings(self) -> None:
        root = census.ROOT
        sources = [
            census.source_record(root / relative, root)
            for relative in census.ACTOR_FALLBACK_INPUTS
        ]
        declared, defaults = census.add_actor_default_expectations({}, sources, root)
        bound, coverage = census.bind_expectations_to_inventory(declared, sources, root)

        self.assertEqual(
            {item["path"] for item in defaults}, set(census.ACTOR_FALLBACK_INPUTS)
        )
        self.assertEqual(coverage["explicit_count"], 2)
        self.assertEqual(coverage["mapped_count"], 2)
        self.assertTrue(coverage["exact_bijection"])
        for relative in census.ACTOR_FALLBACK_INPUTS:
            with self.subTest(path=relative):
                self.assertEqual(bound[relative]["class"], "required_compile")
                self.assertTrue(bound[relative]["reason"])
                self.assertEqual(bound[relative]["source_hash_status"], "matched")
                self.assertTrue(bound[relative]["authority_valid"])
                self.assertEqual(
                    census._captured_bytes(bound[relative]["expected_executable_stdout"]),
                    census.ACTOR_EXPECTED_STDOUT[relative],
                )
                self.assertEqual(
                    bound[relative]["authority"]["test_id"],
                    census.actor_fixture_test_contracts(root)[relative]["test_id"],
                )

    def test_actor_default_expectation_requires_an_active_test_and_exact_call(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            relative = census.ACTOR_FALLBACK_INPUTS[0]
            source_path = root / relative
            source_path.parent.mkdir(parents=True)
            source_path.write_text("func main() { println(15) }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            test_id = "canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback"
            authority = root / "tests/real_world_cli.rs"
            authority.parent.mkdir(parents=True, exist_ok=True)
            authority.write_text(
                "#[test]\n"
                '#[ignore = "disabled"]\n'
                f"fn {test_id}() {{\n"
                '    assert_original_actor_cli_routes("actor_nested_func_capture.mimi", b"15\\n", false);\n'
                "}\n",
                encoding="utf-8",
            )

            contracts = census.actor_fixture_test_contracts(root)
            declared, defaults = census.add_actor_default_expectations({}, [source], root)

        self.assertFalse(contracts[relative]["authority_valid"])
        self.assertNotIn(relative, declared)
        self.assertEqual(defaults, [])

    def test_actor_authority_rejects_should_panic_test_semantics(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            relative = census.ACTOR_FALLBACK_INPUTS[0]
            source_path = root / relative
            source_path.parent.mkdir(parents=True)
            source_path.write_text("func main() { println(15) }\n", encoding="utf-8")
            authority = root / census.ACTOR_HELPER_TEST_PATH
            authority.parent.mkdir(parents=True, exist_ok=True)
            test_id = census.ACTOR_AUTHORITY_TEST_IDS[relative]
            authority.write_text(
                _actor_helper_source()
                + "\n#[test]\n#[should_panic]\n"
                + f"fn {test_id}() {{\n"
                + '    assert_original_actor_cli_routes("actor_nested_func_capture.mimi", b"15\\n", false);\n'
                + "}\n",
                encoding="utf-8",
            )

            contract = census.actor_fixture_test_contracts(root)[relative]
            parsed_span = census._rust_test_function_span(
                authority.read_text(encoding="utf-8"), test_id
            )

        self.assertFalse(contract["authority_valid"])
        self.assertIsNone(parsed_span)

    def test_actor_authority_ignores_calls_in_comments_and_strings_and_rejects_cfg_any(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            relative = census.ACTOR_FALLBACK_INPUTS[0]
            source_path = root / relative
            source_path.parent.mkdir(parents=True)
            source_path.write_text("func main() { println(15) }\n", encoding="utf-8")
            test_id = "canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback"
            authority = root / "tests/real_world_cli.rs"
            authority.parent.mkdir(parents=True, exist_ok=True)
            call = (
                'assert_original_actor_cli_routes('
                '"actor_nested_func_capture.mimi", b"15\\n", false);'
            )
            cases = {
                "line_comment": f"// {call}",
                "block_comment": f"/* {call} */",
                "raw_string": f'let _ignored = r#"{call}"#;',
                "nested_dead_branch": f"if false {{ {call} }}",
                "cfg_disabled_statement": f"#[cfg(any())]\n{call}",
                "cfg_disabled_binding_statement": (
                    f"#[cfg(any())]\nlet _ignored = {call[:-1]};"
                ),
                "unreachable_after_return": f"return;\n{call}",
                "conditional_setup_before_call": f"let enabled = true;\n{call}",
            }
            for label, body in cases.items():
                with self.subTest(authority=label):
                    authority.write_text(
                        "#[test]\n"
                        f"fn {test_id}() {{\n"
                        f"    {body}\n"
                        "}\n",
                        encoding="utf-8",
                    )
                    contracts = census.actor_fixture_test_contracts(root)
                    self.assertFalse(contracts[relative]["authority_valid"])

            authority.write_text(
                "#[cfg(any())]\n"
                "\n"
                "#[test]\n"
                f"fn {test_id}() {{\n    {call}\n}}\n",
                encoding="utf-8",
            )
            inactive_contract = census.actor_fixture_test_contracts(root)[relative]

            authority.write_text(
                "#[cfg_attr(not(any()), ignore)]\n"
                "#[test]\n"
                f"fn {test_id}() {{\n    {call}\n}}\n",
                encoding="utf-8",
            )
            conditional_ignore_contract = census.actor_fixture_test_contracts(root)[relative]

            authority.write_text(
                _actor_helper_source()
                + "\n#[test]\n"
                + f"fn {test_id}() {{\n"
                + f"    {call}\n"
                + "}\n",
                encoding="utf-8",
            )
            active_contract = census.actor_fixture_test_contracts(root)[relative]

        self.assertFalse(inactive_contract["authority_valid"])
        self.assertFalse(conditional_ignore_contract["authority_valid"])
        self.assertTrue(active_contract["authority_valid"])

    def test_actor_authority_rejects_a_noop_shared_helper(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            relative = census.ACTOR_FALLBACK_INPUTS[0]
            fixture = root / relative
            fixture.parent.mkdir(parents=True)
            fixture.write_text("func main() { println(15) }\n", encoding="utf-8")
            test_id = census.ACTOR_AUTHORITY_TEST_IDS[relative]
            authority = root / census.ACTOR_HELPER_TEST_PATH
            authority.parent.mkdir(parents=True, exist_ok=True)
            authority.write_text(
                "fn assert_original_actor_cli_routes(_: &str, _: &[u8], _: bool) {}\n"
                "#[test]\n"
                f"fn {test_id}() {{\n"
                '    assert_original_actor_cli_routes("actor_nested_func_capture.mimi", b"15\\n", false);\n'
                "}\n",
                encoding="utf-8",
            )

            contract = census.actor_fixture_test_contracts(root)[relative]

        self.assertFalse(contract["helper_valid"])
        self.assertFalse(contract["authority_valid"])
        self.assertNotEqual(contract["helper_sha256"], census.ACTOR_HELPER_SHA256)

    def test_actor_helper_is_resistant_to_shadowed_std_names_and_macros(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            relative = census.ACTOR_FALLBACK_INPUTS[0]
            fixture = root / relative
            fixture.parent.mkdir(parents=True)
            fixture.write_text("func main() { println(15) }\n", encoding="utf-8")
            authority = root / census.ACTOR_HELPER_TEST_PATH
            authority.parent.mkdir(parents=True, exist_ok=True)
            shadowing = r'''macro_rules! assert { ($($arg:tt)*) => {{}} }
macro_rules! assert_eq { ($($arg:tt)*) => {{}} }
macro_rules! format { ($($arg:tt)*) => {{ "forged" }} }
macro_rules! eprintln { ($($arg:tt)*) => {{}} }
struct Command;
struct String;
mod fs {}
fn project_root() -> &'static str { "forged-root" }
fn mimi_bin() -> &'static str { "forged-mimi" }
'''
            test_id = census.ACTOR_AUTHORITY_TEST_IDS[relative]
            authority.write_text(
                shadowing
                + _actor_helper_source()
                + "\n#[test]\n"
                + f"fn {test_id}() {{\n"
                + '    assert_original_actor_cli_routes("actor_nested_func_capture.mimi", b"15\\n", false);\n'
                + "}\n",
                encoding="utf-8",
            )
            contract = census.actor_fixture_test_contracts(root)[relative]

        self.assertTrue(contract["helper_valid"])
        self.assertTrue(contract["authority_valid"])

    def test_actor_named_authority_must_own_the_assertion_inside_its_active_test(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            relative = census.ACTOR_FALLBACK_INPUTS[0]
            source_path = root / relative
            source_path.parent.mkdir(parents=True)
            source_path.write_text("func main() { println(15) }\n", encoding="utf-8")
            test_id = census.ACTOR_AUTHORITY_TEST_IDS[relative]
            authority = root / "tests/real_world_cli.rs"
            authority.parent.mkdir(parents=True, exist_ok=True)
            call = (
                'assert_original_actor_cli_routes('
                '"actor_nested_func_capture.mimi", b"15\\n", false);'
            )

            authority.write_text(
                "#![cfg(any())]\n"
                "#[test]\n"
                f"fn {test_id}() {{ {call} }}\n",
                encoding="utf-8",
            )
            crate_disabled = census.actor_fixture_test_contracts(root)[relative]

            authority.write_text(
                "#[test]\n"
                f"fn {test_id}() {{}}\n"
                "#[test]\n"
                f"fn unrelated_test() {{ {call} }}\n",
                encoding="utf-8",
            )
            assertion_in_other_test = census.actor_fixture_test_contracts(root)[relative]

        self.assertFalse(crate_disabled["authority_valid"])
        self.assertFalse(assertion_in_other_test["authority_valid"])


class ActorAuthorityExecutionTests(unittest.TestCase):
    def _temporary_actor_project(self, root: Path) -> tuple[list[dict], dict]:
        (root / "Cargo.toml").write_text(
            '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        for path in census.ACTOR_FALLBACK_INPUTS:
            source = root / path
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text("func main() { println(1) }\n", encoding="utf-8")
        test_lines = []
        for path in census.ACTOR_FALLBACK_INPUTS:
            test_id = census.ACTOR_AUTHORITY_TEST_IDS[path]
            fixture = Path(path).name
            stdout = census.ACTOR_EXPECTED_STDOUT[path].decode("ascii")
            encoded_stdout = json.dumps(stdout)
            test_lines.extend([
                "#[test]",
                f"fn {test_id}() {{",
                f"    assert_original_actor_cli_routes({json.dumps(fixture)}, b{encoded_stdout}, false);",
                "}",
            ])
        authority = root / "tests/real_world_cli.rs"
        authority.parent.mkdir(parents=True, exist_ok=True)
        authority.write_text(
            _actor_helper_source() + "\n" + "\n".join(test_lines) + "\n",
            encoding="utf-8",
        )
        sources = [census.source_record(root / path, root) for path in census.ACTOR_FALLBACK_INPUTS]
        return sources, census.actor_fixture_test_contracts(root)

    @staticmethod
    def _successful_test_output(test_id: str) -> bytes:
        return (
            f"running 1 test\ntest {test_id} ... ok\n\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
            "0 filtered out; finished in 0.00s\n"
        ).encode()

    def test_actor_runner_executes_exact_tests_and_retains_hash_bound_bin_logs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources, contracts = self._temporary_actor_project(root)
            logs = root / "artifacts/logs"
            logs.mkdir(parents=True)
            completed = [
                subprocess.CompletedProcess(
                    ["cargo", "test"],
                    0,
                    self._successful_test_output(
                        census.ACTOR_AUTHORITY_TEST_IDS[path]
                    ),
                    b"",
                )
                for path in census.ACTOR_FALLBACK_INPUTS
            ]
            cargo_authority = census.cargo_test_toolchain_authority(root)

            with patch.object(census.subprocess, "run", side_effect=completed) as run:
                execution = census._run_actor_authority_tests(
                    root, logs, sources, contracts, 30.0, cargo_authority
                )

            self.assertEqual(run.call_count, 2)
            self.assertEqual(execution["status"], "passed")
            self.assertEqual(
                [case["command"] for case in execution["cases"]],
                [
                    [
                        cargo_authority["cargo"]["path"], "test", "--test", "real_world_cli",
                        census.ACTOR_AUTHORITY_TEST_IDS[path], "--", "--exact"
                    ]
                    for path in census.ACTOR_FALLBACK_INPUTS
                ],
            )
            for case in execution["cases"]:
                self.assertTrue(case["execution_valid"])
                self.assertEqual(
                    case["cargo_toolchain_identity_sha256"],
                    cargo_authority["identity_sha256"],
                )
                self.assertTrue(case["cargo_execution"]["stable"])
                self.assertEqual(case["fixture_sha256_before"], case["fixture_sha256_expected"])
                self.assertEqual(case["fixture_sha256_after"], case["fixture_sha256_expected"])
                self.assertEqual(case["test_source_sha256_before"], case["test_source_sha256_expected"])
                self.assertEqual(case["test_source_sha256_after"], case["test_source_sha256_expected"])
                for capture in (case["stdout_raw"], case["stderr_raw"]):
                    self.assertEqual(Path(capture["path"]).suffix, ".bin")
                    self.assertTrue(census._raw_sidecar_matches(logs, capture))

    def test_actor_runner_rejects_nonzero_and_incorrect_test_output(self) -> None:
        invalid_first_results = (
            (
                "nonzero",
                1,
                b"running 1 test\ntest canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback ... FAILED\n",
            ),
            (
                "wrong-test-result",
                0,
                b"running 1 test\ntest canonical_actor_capture_original_uses_default_and_explicit_cli_without_fallback ... FAILED\n",
            ),
        )
        for label, returncode, stdout in invalid_first_results:
            with self.subTest(result=label), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                sources, contracts = self._temporary_actor_project(root)
                logs = root / "artifacts/logs"
                logs.mkdir(parents=True)
                failed = subprocess.CompletedProcess(["cargo", "test"], returncode, stdout, b"runtime failure\n")
                passed = subprocess.CompletedProcess(
                    ["cargo", "test"],
                    0,
                    self._successful_test_output(
                        census.ACTOR_AUTHORITY_TEST_IDS[census.ACTOR_FALLBACK_INPUTS[1]]
                    ),
                    b"",
                )
                cargo_authority = census.cargo_test_toolchain_authority(root)
                with patch.object(census.subprocess, "run", side_effect=[failed, passed]):
                    execution = census._run_actor_authority_tests(
                        root, logs, sources, contracts, 30.0, cargo_authority
                    )

                self.assertEqual(execution["status"], "failed")
                self.assertFalse(execution["cases"][0]["execution_valid"])
                self.assertTrue(execution["cases"][1]["execution_valid"])


class CargoAuthorityTests(unittest.TestCase):
    def _root(self, root: Path) -> None:
        (root / "Cargo.toml").write_text(
            '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )

    def test_path_cargo_shim_is_never_invoked_or_accepted_as_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._root(root)
            shim_dir = root / "shim"
            shim_dir.mkdir()
            shim = shim_dir / "cargo"
            shim.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' '{\"packages\":[],\"workspace_members\":[]}'\n"
                "printf '%s\\n' 'running 1 test'\n"
                "printf '%s\\n' 'test test_route_contract ... ok'\n"
                "printf '%s\\n' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'\n",
                encoding="utf-8",
            )
            shim.chmod(0o755)
            old_path = census.os.environ.get("PATH", "")
            with patch.dict(census.os.environ, {"PATH": f"{shim_dir}:{old_path}"}):
                authority = census.cargo_test_toolchain_authority(root)
                with patch.object(census.subprocess, "run") as run:
                    result = census._run_cargo_authority_command(
                        [authority["cargo"]["path"], "metadata", "--format-version", "1", "--no-deps"],
                        root,
                        1.0,
                        authority,
                    )

        self.assertFalse(authority["policy"]["valid"])
        self.assertEqual(authority["cargo"]["path"], str(shim))
        self.assertEqual(run.call_count, 0)
        self.assertNotEqual(result["run_status"], "completed")
        self.assertFalse(result["cargo_execution"]["stable"])

    def test_native_path_cargo_shim_is_not_accepted_as_the_rustup_proxy(self) -> None:
        native_true = shutil.which("true")
        if not native_true or not census._native_executable_magic(Path(native_true)):
            self.skipTest("host does not expose a native `true` executable for the shim probe")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._root(root)
            shim_dir = root / "shim"
            shim_dir.mkdir()
            shim = shim_dir / "cargo"
            shim.symlink_to(Path(native_true).resolve())
            old_path = census.os.environ.get("PATH", "")
            with patch.dict(census.os.environ, {"PATH": f"{shim_dir}:{old_path}"}):
                authority = census.cargo_test_toolchain_authority(root)
                with patch.object(census.subprocess, "run") as run:
                    result = census._run_cargo_authority_command(
                        [authority["cargo"]["path"], "metadata", "--format-version", "1"],
                        root,
                        1.0,
                        authority,
                    )

        self.assertTrue(authority["cargo"]["native_executable"])
        self.assertFalse(authority["policy"]["valid"])
        self.assertTrue(any(
            "cargo_is_not_the_selected_rustup_proxy" in issue
            for issue in authority["policy"]["issues"]
        ))
        self.assertEqual(run.call_count, 0)
        self.assertNotEqual(result["run_status"], "completed")

    def test_path_change_after_resolution_cannot_switch_to_cargo_shim(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._root(root)
            shim_dir = root / "shim"
            shim_dir.mkdir()
            shim = shim_dir / "cargo"
            shim.write_text("#!/bin/sh\necho forged\n", encoding="utf-8")
            shim.chmod(0o755)
            authority = census.cargo_test_toolchain_authority(root)
            old_path = census.os.environ.get("PATH", "")
            with patch.dict(census.os.environ, {"PATH": f"{shim_dir}:{old_path}"}):
                with patch.object(census.subprocess, "run") as run:
                    result = census._run_cargo_authority_command(
                        [authority["cargo"]["path"], "test", "--no-run"],
                        root,
                        1.0,
                        authority,
                    )

        self.assertEqual(result["command"][0], authority["cargo"]["path"])
        self.assertNotEqual(result["command"][0], str(shim))
        self.assertEqual(run.call_count, 0)
        self.assertFalse(result["cargo_execution"]["stable"])
        self.assertNotEqual(result["run_status"], "completed")

    def test_rustc_runner_target_and_cargo_config_overrides_fail_closed(self) -> None:
        blocked = (
            "CARGO_HOME",
            "RUSTC",
            "RUSTC_WRAPPER",
            "RUSTFLAGS",
            "RUSTUP_HOME",
            "CARGO_BUILD_TARGET",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
        )
        for name in blocked:
            with self.subTest(override=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                self._root(root)
                with patch.dict(census.os.environ, {name: "/tmp/fake-tool"}):
                    authority = census.cargo_test_toolchain_authority(root)
                    with patch.object(census.subprocess, "run") as run:
                        result = census._run_cargo_authority_command(
                            [authority["cargo"]["path"], "test", "--no-run"],
                            root,
                            1.0,
                            authority,
                        )
                self.assertFalse(authority["policy"]["valid"])
                self.assertEqual(run.call_count, 0)
                self.assertNotEqual(result["run_status"], "completed")

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._root(root)
            config = root / ".cargo" / "config.toml"
            config.parent.mkdir()
            config.write_text(
                '[build]\nrustc = "fake-rustc"\n\n'
                '[target.x86_64-unknown-linux-gnu]\nrunner = "fake-runner"\n',
                encoding="utf-8",
            )
            authority = census.cargo_test_toolchain_authority(root)

        self.assertFalse(authority["policy"]["valid"])
        self.assertTrue(any("cargo_config_build_override" in issue for issue in authority["policy"]["issues"]))
        self.assertTrue(any("cargo_config_target_override" in issue for issue in authority["policy"]["issues"]))

    def test_execution_evidence_binds_before_after_paths_hashes_and_toolchain(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._root(root)
            authority = census.cargo_test_toolchain_authority(root)
            cargo = census._executable_observation(authority["cargo"]["path"])
            rustc = census._executable_observation(authority["rustc"]["path"])
            evidence = census._cargo_execution_evidence(
                authority, cargo, rustc, cargo, rustc, stable=True
            )
            valid = census._cargo_execution_evidence_valid(
                evidence, authority, root=root
            )
            evidence["cargo"]["after"]["sha256"] = "0" * 64
            changed_hash = census._cargo_execution_evidence_valid(
                evidence, authority, root=root
            )

        self.assertTrue(valid)
        self.assertFalse(changed_hash)
        evidence = census._cargo_execution_evidence(
            authority, cargo, rustc, cargo, rustc, stable=True
        )
        evidence["rustup"]["after"]["sha256"] = "0" * 64
        self.assertFalse(
            census._cargo_execution_evidence_valid(evidence, authority, root=root)
        )

    def test_active_rustup_toolchain_and_selected_sysroot_rustc_are_pinned(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self._root(root)
            real_command_result = census._command_result
            observed_commands: list[tuple[list[str], str | None]] = []

            def observe_command(
                command: list[str], **kwargs: object
            ) -> dict[str, object]:
                observed_commands.append(
                    (list(command), str(kwargs.get("cwd")) if kwargs.get("cwd") else None)
                )
                return real_command_result(command, **kwargs)

            with patch.object(census, "_command_result", side_effect=observe_command):
                authority = census.cargo_test_toolchain_authority(root)

            toolchain = authority["rustup_toolchain"]
            selected_rustc = toolchain["selected_rustc"]
            self.assertTrue(authority["policy"]["valid"], authority["policy"]["issues"])
            self.assertTrue(toolchain["active_toolchain"])
            self.assertEqual(toolchain["cwd"], str(root))
            self.assertEqual(Path(selected_rustc["path"]).name, "rustc")
            self.assertEqual(selected_rustc["sha256"], census.sha256_file(Path(selected_rustc["resolved_path"])))
            self.assertEqual(
                toolchain["sysroot_path"],
                str(Path(selected_rustc["resolved_path"]).parent.parent),
            )
            self.assertEqual(len(observed_commands), 3)
            self.assertTrue(all(cwd == str(root) for _command, cwd in observed_commands))
            self.assertEqual(
                [command[1:] for command, _cwd in observed_commands],
                [
                    ["show", "active-toolchain"],
                    ["which", "cargo"],
                    ["which", "rustc"],
                ],
            )

            with patch.object(census, "_cargo_toolchain_identity_valid", return_value=True):
                manifest = census.toolchain_manifest(
                    root / "mimi", root=root, cargo_authority=authority
                )
            self.assertEqual(manifest["cargo"]["version"]["cwd"], str(root))
            self.assertEqual(manifest["rustc"]["version"]["cwd"], str(root))
            self.assertEqual(
                Path(manifest["rustc"]["version"]["command"][0]),
                Path(selected_rustc["path"]),
            )

    def test_rust_toolchain_override_is_in_source_snapshot_and_invalidates_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self._root(root)
            authority = census.cargo_test_toolchain_authority(root)
            override = root / "rust-toolchain.toml"
            override.write_text('[toolchain]\nchannel = "stable"\n', encoding="utf-8")

            snapshot = census.source_snapshot(root, [])
            authority_still_valid = census._cargo_toolchain_identity_valid(
                authority, root=root
            )

        self.assertIn("rust-toolchain.toml", snapshot["files"])
        self.assertFalse(authority_still_valid)


class WholeProgramRouteTests(unittest.TestCase):
    def test_route_parser_preserves_canonical_and_compatibility_dispositions(self) -> None:
        route = census._route_observation(
            "",
            "canonical route disposition: canonical (default-route) mir_digest=" + "a" * 64,
            0,
            "completed",
        )
        self.assertEqual(route["disposition"], "canonical")
        self.assertEqual(route["mir_digest"], "a" * 64)

        legacy = census._route_observation(
            "", "canonical route disposition: legacy (outside-migrated-profile)", 0, "completed"
        )
        self.assertEqual(legacy["disposition"], "compatibility")
        self.assertEqual(legacy["observations"][0]["raw_disposition"], "legacy")

    def test_rejection_is_preserved_as_noncanonical_disposition(self) -> None:
        route = census._route_observation(
            "",
            "error[MIR-COVERAGE-001]: default Canonical MIR route rejected: uncovered input",
            1,
            "completed",
        )
        self.assertEqual(route["disposition"], "rejected")
        self.assertEqual(route["classification_status"], "observed")

    def test_pre_route_diagnostics_accept_path_prefixed_and_ansi_error_forms(self) -> None:
        diagnostics = {
            "coded error with source path": (
                "/tmp/case.mimi has 1 type error(s):\n"
                "error[E0432] /tmp/case.mimi:2:3-8 invalid type\n"
                "\x1b[31merror\x1b[0m: type checking failed\n"
            ),
            "uncoded error with source path": (
                "error /tmp/case.mimi:2:3 expected `state` or `transition` in flow body\n"
            ),
            "ansi wrapped path diagnostic": (
                "\x1b[31merror\x1b[0m /tmp/case.mimi:2:3 unexpected token\n"
            ),
        }
        for label, stderr in diagnostics.items():
            with self.subTest(label=label):
                route = census._route_observation("", stderr, 1, "completed")
                self.assertEqual(route["disposition"], "rejected")
                self.assertEqual(route["classification_status"], "inferred_pre_route_rejection")
                self.assertNotEqual(route["classification_status"], "missing_route_telemetry")


class PerFunctionEligibilityTests(unittest.TestCase):
    def test_canonical_mir_named_roster_reconciles_owner_count_and_route_digest(self) -> None:
        digest = "a" * 64
        route = census._route_observation(
            "",
            "canonical route disposition: canonical (scalar-actor-domain-v1) mir_digest=" + digest,
            0,
            "completed",
        )
        details = census._function_observations(
            "",
            "info: canonical MIR native emitted owner='function:Counter::bump'\n"
            "info: canonical MIR native emitted owner='function:main'\n"
            "info: canonical MIR native compiled 2/2 function(s), 0 fell back; mir_digest="
            + digest,
            None,
            route,
        )
        self.assertEqual(details["status"], "complete_named")
        self.assertTrue(details["named_roster_complete"])
        self.assertEqual(details["counts"]["total_functions"], 2)
        self.assertEqual(details["route_mir_digest"], digest)
        self.assertEqual(
            [event["owner"] for event in details["functions"]],
            ["function:Counter::bump", "function:main"],
        )

    def test_canonical_mir_missing_duplicate_and_digest_mismatch_fail_closed(self) -> None:
        digest = "b" * 64
        route = census._route_observation(
            "",
            "canonical route disposition: canonical (default-route) mir_digest=" + digest,
            0,
            "completed",
        )
        cases = {
            "missing owner": (
                "info: canonical MIR native emitted owner='function:main'\n"
                "info: canonical MIR native compiled 2/2 function(s), 0 fell back; mir_digest="
                + digest
            ),
            "duplicate owner": (
                "info: canonical MIR native emitted owner='function:main'\n"
                "info: canonical MIR native emitted owner='function:main'\n"
                "info: canonical MIR native compiled 2/2 function(s), 0 fell back; mir_digest="
                + digest
            ),
            "wrong digest": (
                "info: canonical MIR native emitted owner='function:main'\n"
                "info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest="
                + "c" * 64
            ),
        }
        for reason, output in cases.items():
            with self.subTest(reason=reason):
                details = census._function_observations("", output, None, route)
                self.assertFalse(details["named_roster_complete"])

    def test_canonical_mir_fallback_count_or_resolved_stats_is_not_accepted(self) -> None:
        digest = "d" * 64
        route = census._route_observation(
            "",
            "canonical route disposition: canonical (scalar-actor-domain-v1) mir_digest=" + digest,
            0,
            "completed",
        )
        details = census._function_observations(
            "",
            "info: canonical MIR native emitted owner='function:main'\n"
            "info: canonical MIR native compiled 1/1 function(s), 1 fell back; mir_digest="
            + digest,
            {"program": "unexpected-resolved-stats"},
            route,
        )
        self.assertFalse(details["named_roster_complete"])
        self.assertTrue(details["has_fallback"])
        self.assertFalse(details["consistency"]["resolved_dispatch_stats_absent"])

    def test_named_roster_reconciles_when_every_function_event_is_present(self) -> None:
        details = census._function_observations(
            "",
            "\n".join(
                [
                    "info: resolved skip 'Actor::step': generics/qualified",
                    "warning: resolved emitter fallback for 'Actor::handler': unsupported expression",
                    "info: resolved emitter emitted 'main'",
                    "info: resolved emitter compiled 1/2 function(s), 1 fell back to legacy",
                ]
            ),
            {
                "program": "src-1",
                "total_functions": 3,
                "eligible": 2,
                "legacy_fallback": 1,
                "emit_failed": 0,
                "skip_reasons": {"generics/qualified": 1},
            },
        )
        self.assertEqual(details["status"], "complete_named")
        self.assertTrue(details["named_roster_complete"])
        self.assertFalse(details["functions"][0]["eligibility"] == "eligible")
        self.assertEqual(details["counts"]["compiled_resolved"], 1)
        self.assertTrue(details["has_fallback"])

    def test_ansi_wrapped_emitter_fallback_is_recorded_without_changing_source_line(self) -> None:
        fallback_line = (
            "\x1b[33mwarning: resolved emitter fallback for 'Actor::handler': "
            "unsupported expression\x1b[0m"
        )
        details = census._function_observations(
            "",
            "\n".join(
                [
                    fallback_line,
                    "info: resolved emitter emitted 'main'",
                    "info: resolved emitter compiled 1/2 function(s), 1 fell back to legacy",
                ]
            ),
            {
                "program": "ansi-fallback",
                "total_functions": 2,
                "eligible": 2,
                "legacy_fallback": 0,
                "emit_failed": 0,
                "skip_reasons": {},
            },
        )

        self.assertTrue(details["named_roster_complete"])
        self.assertTrue(details["has_fallback"])
        self.assertEqual(details["counts"]["named_emission_fallbacks"], 1)
        fallback = next(event for event in details["functions"] if event["fallback"])
        self.assertEqual(fallback["status"], "emission_fallback")
        self.assertEqual(fallback["function"], "Actor::handler")
        self.assertEqual(fallback["source_line"], fallback_line)

    def test_aggregate_success_count_never_fabricates_function_names(self) -> None:
        details = census._function_observations(
            "",
            "info: resolved skip 'Actor::step': generics/qualified\n"
            "info: resolved emitter compiled 2/2 function(s), 0 fell back to legacy\n",
            {
                "total_functions": 3,
                "eligible": 2,
                "legacy_fallback": 1,
                "emit_failed": 0,
                "skip_reasons": {"generics/qualified": 1},
            },
        )
        self.assertEqual(details["status"], "partial")
        self.assertFalse(details["named_roster_complete"])
        self.assertEqual(details["counts"]["unattributed_function_count"], 2)
        self.assertTrue(all(slot["function"] is None for slot in details["unattributed_function_slots"]))
        self.assertTrue(all(slot["identity_status"] == "name_not_emitted_by_compiler" for slot in details["unattributed_function_slots"]))


class BuildFailureClassificationTests(unittest.TestCase):
    def test_failure_classes_require_diagnostic_evidence(self) -> None:
        capability = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text="error[MIR-COVERAGE-001]: default Canonical MIR route rejected: no receipt",
            target_kind="executable",
            route={"disposition": "rejected", "classification_status": "observed"},
        )
        library = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text="shared library target is unsupported on this target",
            target_kind="shared_library_probe",
            route={"disposition": "unobserved"},
        )
        environment = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text="clang: command not found",
            target_kind="executable",
            route={"disposition": "unobserved"},
        )
        unknown = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text="error: native code generation failed",
            target_kind="executable",
            route={"disposition": "unobserved"},
        )

        self.assertEqual(capability["class"], "capability")
        self.assertEqual(library["class"], "library_target")
        self.assertEqual(environment["class"], "environment")
        self.assertEqual(unknown["class"], "unknown")
        self.assertEqual(unknown["status"], "unclassified")

    def test_native_capability_and_external_link_diagnostics_are_recognized(self) -> None:
        e0723 = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text=(
                "error[E0723] returning a value of type `Set<T>` from a native function "
                "is not yet supported\n"
            ),
            target_kind="executable",
            route={"disposition": "compatibility"},
        )
        protocol_abi = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text=(
                "error[NATIVE-GENERIC-PROTOCOL-ABI-001] /tmp/case.mimi:13:32-51 "
                "native codegen cannot emit checked generic ProtocolMethod\n"
            ),
            target_kind="executable",
            route={"disposition": "compatibility"},
        )
        w4_link = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text=(
                "ld.lld: error: undefined symbol: v02_checksum\n"
                ">>> referenced by w4_loopback_service\n"
                "collect2: error: ld returned 1 exit status\n"
            ),
            target_kind="executable",
            route={"disposition": "canonical"},
        )

        self.assertEqual(e0723["class"], "capability")
        self.assertEqual(protocol_abi["class"], "capability")
        self.assertEqual(w4_link["class"], "external_dependency")
        self.assertEqual(w4_link["status"], "classified")
        self.assertIn("undefined symbol: v02_checksum", w4_link["evidence"]["matched_text"])
        self.assertEqual(w4_link["evidence"]["symbol"], "v02_checksum")
        self.assertEqual(
            w4_link["evidence"]["linker_line"],
            "ld.lld: error: undefined symbol: v02_checksum",
        )

    def test_inferred_pre_route_source_errors_keep_first_and_all_raw_lines(self) -> None:
        stderr = (
            "error[E0432] /tmp/case.mimi:2:3-8 unknown name 'absent'\n"
            "error /tmp/case.mimi:5:1-4 expected an expression\n"
            "\x1b[31merror\x1b[0m /tmp/case.mimi:7:5-18 unexpected token\n"
            "\x1b[31merror\x1b[0m: type checking failed\n"
        )
        route = census._route_observation("", stderr, 1, "completed")
        failure = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text=stderr,
            target_kind="executable",
            route=route,
        )

        self.assertEqual(route["classification_status"], "inferred_pre_route_rejection")
        self.assertEqual(failure["class"], "source_diagnostic")
        self.assertEqual(failure["status"], "classified")
        self.assertEqual(failure["evidence"]["diagnostic_count"], 3)
        self.assertEqual(
            failure["evidence"]["first_diagnostic"],
            "error[E0432] /tmp/case.mimi:2:3-8 unknown name 'absent'",
        )
        self.assertEqual(
            failure["evidence"]["diagnostics"],
            [
                "error[E0432] /tmp/case.mimi:2:3-8 unknown name 'absent'",
                "error /tmp/case.mimi:5:1-4 expected an expression",
                "\x1b[31merror\x1b[0m /tmp/case.mimi:7:5-18 unexpected token",
            ],
        )

    def test_source_error_requires_inferred_pre_route_rejection_and_location(self) -> None:
        source_error = "error /tmp/case.mimi:3:1 unknown name\n"
        observed_route = census._route_observation(
            "",
            "canonical route disposition: legacy (outside-migrated-profile)\n" + source_error,
            1,
            "completed",
        )
        inferred_without_location = census._route_observation(
            "", "error: generic compiler failure\n", 1, "completed"
        )

        observed_failure = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text=source_error,
            target_kind="executable",
            route=observed_route,
        )
        unmatched_failure = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text="error: generic compiler failure\n",
            target_kind="executable",
            route=inferred_without_location,
        )

        self.assertEqual(observed_failure["class"], "unknown")
        self.assertEqual(unmatched_failure["class"], "unknown")


class LegacyOwnerAuditParserTests(unittest.TestCase):
    @staticmethod
    def _audit_text(
        *,
        missing_active: tuple[str, ...] = (),
        retirement_line: str | None = None,
        dynamic_probe_status: str = "not_run",
        extra_lines: tuple[str, ...] = (),
    ) -> str:
        condition_by_name = {
            name: f"remove {name} after its remaining scope is migrated"
            for name in census.ACTIVE_LEGACY_OWNER_NAMES
        }
        digest_by_name = {
            name: census.sha256_bytes(condition.encode("utf-8"))
            for name, condition in condition_by_name.items()
        }
        lines = ["schema=canonical-mir-legacy-owner-audit-v1"]
        for name in census.ACTIVE_LEGACY_OWNER_NAMES:
            if name in missing_active:
                continue
            condition = condition_by_name[name]
            lines.extend([
                f"owner={name} production_accessor_call_sites=0",
                f"owner={name} owner_deletion_condition_digest={digest_by_name[name]}",
                f"owner={name} production_access_status=sealed dependency_class=fixture-dependency "
                "owner_deletion_ready=0 owner_deletion_blocker=retained test compatibility surface",
                f"owner_deletion_condition={condition}",
            ])
        set_digest = "".join(
            f"{name}={digest_by_name[name] if name not in missing_active else ''}\n"
            for name in census.ACTIVE_LEGACY_OWNER_NAMES
        )
        default_retirement_line = (
            "retired_flow_dual_owners=physical;ast-owner=deleted;file-adapter=deleted;"
            "transition-vir-adapter=deleted;MIR-positive-negative-single-dual-evidence="
            + "+".join(census.RETIRED_FLOW_DUAL_PARITY_TESTS)
        )
        lines.extend([
            retirement_line if retirement_line is not None else default_retirement_line,
            f"owner_deletion_condition_set_digest={census.sha256_bytes(set_digest.encode('utf-8'))}",
            f"owner_count={len(census.ACTIVE_LEGACY_OWNER_NAMES)}",
            f"dynamic_probe_status={dynamic_probe_status}",
            "audit_status=ok",
            *extra_lines,
        ])
        return "\n".join(lines) + "\n"

    def test_active_and_physically_retired_rosters_are_separate(self) -> None:
        parsed = census._parse_legacy_owner_audit(self._audit_text())

        self.assertTrue(parsed["parsed_owner_set_complete"])
        self.assertEqual(
            [owner["name"] for owner in parsed["owners"]],
            list(census.ACTIVE_LEGACY_OWNER_NAMES),
        )
        self.assertEqual(
            [owner["name"] for owner in parsed["retired_owners"]],
            list(census.RETIRED_LEGACY_OWNER_NAMES),
        )
        self.assertTrue(all(owner["retirement_evidence_complete"] for owner in parsed["retired_owners"]))
        self.assertEqual(parsed["dynamic_probe_status"], "not_run")

    def test_repository_audit_script_output_matches_the_parser_contract(self) -> None:
        script = census.ROOT / "scripts/audit-mir-legacy-owners.sh"
        result = subprocess.run(
            ["bash", str(script)],
            cwd=census.ROOT,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", errors="replace"))
        parsed = census._parse_legacy_owner_audit(
            result.stdout.decode("utf-8", errors="replace")
        )
        self.assertTrue(parsed["parsed_owner_set_complete"])
        self.assertEqual(parsed["dynamic_probe_status"], "not_run")

    def test_each_missing_active_owner_fails_even_with_retirement_evidence(self) -> None:
        for missing in census.ACTIVE_LEGACY_OWNER_NAMES:
            with self.subTest(owner=missing):
                parsed = census._parse_legacy_owner_audit(
                    self._audit_text(missing_active=(missing,))
                )
                self.assertFalse(parsed["parsed_owner_set_complete"])

    def test_retirement_summary_and_declared_tests_fail_closed(self) -> None:
        valid = (
            "retired_flow_dual_owners=physical;ast-owner=deleted;file-adapter=deleted;"
            "transition-vir-adapter=deleted;MIR-positive-negative-single-dual-evidence="
            + "+".join(census.RETIRED_FLOW_DUAL_PARITY_TESTS)
        )
        for line in ("", valid.replace("file-adapter=deleted", "file-adapter=retained"),
                     valid.replace(census.RETIRED_FLOW_DUAL_PARITY_TESTS[1], "unknown_test")):
            with self.subTest(retirement_line=line):
                parsed = census._parse_legacy_owner_audit(
                    self._audit_text(retirement_line=line)
                )
                self.assertFalse(parsed["retired_owner_evidence_complete"])
                self.assertFalse(parsed["parsed_owner_set_complete"])

        dynamic = census._parse_legacy_owner_audit(
            self._audit_text(dynamic_probe_status="passed")
        )
        self.assertFalse(dynamic["retired_owner_evidence_complete"])

    def test_duplicate_fields_summaries_and_unknown_owners_fail_closed(self) -> None:
        base = self._audit_text()
        duplicate_field = census._parse_legacy_owner_audit(
            base.replace(
                "owner_count=2\n",
                f"owner={census.ACTIVE_LEGACY_OWNER_NAMES[0]} production_accessor_call_sites=0\nowner_count=2\n",
            )
        )
        self.assertTrue(duplicate_field["duplicate_owner_fields"])
        self.assertFalse(duplicate_field["parsed_owner_set_complete"])

        retirement = base.splitlines()[1 + 4 * len(census.ACTIVE_LEGACY_OWNER_NAMES)]
        duplicate_summary = census._parse_legacy_owner_audit(
            self._audit_text(extra_lines=(retirement,))
        )
        self.assertFalse(duplicate_summary["retired_owner_evidence_complete"])

        unknown = census._parse_legacy_owner_audit(
            self._audit_text(extra_lines=("owner=UnexpectedOwner production_accessor_call_sites=0",))
        )
        self.assertIn("UnexpectedOwner", unknown["unknown_owner_names"])
        self.assertFalse(unknown["parsed_owner_set_complete"])

    def test_malformed_active_and_reintroduced_retired_owner_rows_fail_closed(self) -> None:
        malformed_rows = (
            f"owner={census.ACTIVE_LEGACY_OWNER_NAMES[0]} "
            "production_accessor_call_sites=not-a-number",
            f"owner={census.ACTIVE_LEGACY_OWNER_NAMES[0]} garbage",
        )
        for row in malformed_rows:
            with self.subTest(row=row):
                malformed = census._parse_legacy_owner_audit(
                    self._audit_text(extra_lines=(row,))
                )
                self.assertTrue(malformed["malformed_owner_rows"])
                self.assertFalse(malformed["parsed_owner_set_complete"])

        reintroduced = census._parse_legacy_owner_audit(
            self._audit_text(extra_lines=(
                f"owner={census.RETIRED_LEGACY_OWNER_NAMES[0]} status=retired",
            ))
        )
        self.assertTrue(reintroduced["malformed_owner_rows"])
        self.assertFalse(reintroduced["parsed_owner_set_complete"])


class StrictGateTests(unittest.TestCase):
    @staticmethod
    def _owner_audit() -> dict:
        owners = []
        for name in census.ACTIVE_LEGACY_OWNER_NAMES:
            condition = f"remove {name} after its remaining scope is migrated"
            owners.append({
                "name": name,
                "production_accessor_call_sites": 0,
                "production_access_status": "sealed",
                "dependency_class": "fixture-dependency",
                "owner_deletion_ready": False,
                "owner_deletion_blocker": "retained compatibility evidence",
                "deletion_condition": condition,
                "deletion_condition_sha256": census.sha256_bytes(condition.encode("utf-8")),
            })
        retired_owners = [
            {
                "name": name,
                "status": "physically_retired",
                "retirement_evidence_kind": "audit_script_source_checks",
                "retirement_evidence_complete": True,
                "declared_parity_test_names": list(census.RETIRED_FLOW_DUAL_PARITY_TESTS),
                "dynamic_probe_status": "not_run",
            }
            for name in census.RETIRED_LEGACY_OWNER_NAMES
        ]
        return {
            "accepted_as_complete_inventory": True,
            "owner_count_reported": len(owners),
            "owner_deletion_condition_set_sha256": "e" * 64,
            "deletion_condition_digests_match_text": True,
            "parsed_owner_set_complete": True,
            "retired_owner_evidence_complete": True,
            "dynamic_probe_status": "not_run",
            "unknown_owner_names": [],
            "duplicate_owner_fields": [],
            "malformed_condition_rows": [],
            "malformed_owner_rows": [],
            "owners": owners,
            "retired_owners": retired_owners,
        }

    @staticmethod
    def _record(path: str, *, route: str = "canonical", fallback: bool = False) -> dict:
        stdout = b"out"
        stderr = b"err"
        stem = census.sha256_bytes(path.encode("utf-8"))[:12]
        native_stdout = census.ACTOR_EXPECTED_STDOUT.get(path, b"")
        native_stdout_capture = census._raw_capture(
            native_stdout, f"{stem}.native.stdout.bin"
        )
        native_stderr_capture = census._raw_capture(
            b"", f"{stem}.native.stderr.bin"
        )
        return {
            "path": path,
            "category": "test_source",
            "present": True,
            "tracked_by_git": False,
            "run_status": "completed",
            "exit_code": 0,
            "route": {
                "disposition": route,
                "classification_status": "observed",
            },
            "failure_classification": {"class": "none", "status": "not_applicable"},
            "expectation": {
                "class": "required_compile",
                "status": "matched",
                "explicit_expectation": True,
                "matched": True,
                "source_hash_status": "matched",
                "source_hash_matches": True,
                "authority_status": "valid",
                "authority_valid": True,
            },
            "per_function": {"named_roster_complete": True, "has_fallback": fallback},
            "output_artifact": {"sha256": "a" * 64, "bytes": 6},
            "stdout_raw": {
                "path": f"{stem}.stdout.bin",
                "bytes": len(stdout),
                "sha256": census.sha256_bytes(stdout),
                "base64": base64.b64encode(stdout).decode(),
            },
            "stderr_raw": {
                "path": f"{stem}.stderr.bin",
                "bytes": len(stderr),
                "sha256": census.sha256_bytes(stderr),
                "base64": base64.b64encode(stderr).decode(),
            },
            "native_executable_run": {
                "run_status": "completed",
                "exit_code": 0,
                "stdout_raw": native_stdout_capture,
                "stderr_raw": native_stderr_capture,
            },
            "source_stable_for_input": True,
        }

    @classmethod
    def _gate(
        cls,
        paths: list[str],
        records: list[dict],
        *,
        missing_manifest_paths: tuple[str, ...] = (),
        tracked_inventory: dict | None = None,
        expectation_contract: dict | None = None,
        excluded_symlinks: tuple[dict, ...] = (),
        source_scope: dict | None = None,
        sidecar_overrides: dict[str, bytes | None | str] | None = None,
        sidecar_root_symlink: bool = False,
        root: Path | None = None,
    ) -> dict:
        actor_manifest = [
            {
                **census.source_record(census.ROOT / path, census.ROOT),
                "tracked_by_git": False,
            }
            for path in census.ACTOR_FALLBACK_INPUTS
        ]
        actor_records = [cls._record(path) for path in census.ACTOR_FALLBACK_INPUTS]
        manifest_by_path = {row["path"]: row for row in actor_manifest}
        record_by_path = {row["path"]: row for row in actor_records}
        for path, record in zip(paths, records):
            manifest_by_path[path] = {
                **manifest_by_path.get(path, {}),
                "path": path,
                "present": True,
                "tracked_by_git": False,
            }
            record_by_path[path] = record
        effective_root = root
        if effective_root is None:
            proof_roots = {
                Path(proof["cargo_metadata"]["cwd"])
                for record in records
                for expectation in [record.get("expectation")]
                if isinstance(expectation, dict)
                for proof in [expectation.get("test_execution_proof")]
                if isinstance(proof, dict)
                and isinstance(proof.get("cargo_metadata"), dict)
                and isinstance(proof["cargo_metadata"].get("cwd"), str)
            }
            if len(proof_roots) == 1:
                effective_root = next(iter(proof_roots))
        for path in missing_manifest_paths:
            manifest_by_path[path] = {"path": path, "present": False, "tracked_by_git": True}
        manifest = list(manifest_by_path.values())
        all_records = list(record_by_path.values())
        tracked_paths = sorted(
            row["path"] for row in manifest if row.get("tracked_by_git")
        )
        tracked = tracked_inventory or {
            "status": "available",
            "path_count": len(tracked_paths),
            "paths_sha256": census._manifest_paths_hash(tracked_paths),
        }
        actor_contracts = census.actor_fixture_test_contracts(census.ROOT)
        actor_cargo_authority = census.cargo_test_toolchain_authority(census.ROOT)
        actor_cargo_observation = census._executable_observation(
            actor_cargo_authority["cargo"]["path"]
        )
        actor_rustc_observation = census._executable_observation(
            actor_cargo_authority["rustc"]["path"]
        )
        actor_cargo_execution = census._cargo_execution_evidence(
            actor_cargo_authority,
            actor_cargo_observation,
            actor_rustc_observation,
            actor_cargo_observation,
            actor_rustc_observation,
            stable=True,
        )
        actor_execution_cases = []
        for index, path in enumerate(census.ACTOR_FALLBACK_INPUTS, 1):
            source = next(row for row in actor_manifest if row["path"] == path)
            contract = actor_contracts[path]
            test_id = census.ACTOR_AUTHORITY_TEST_IDS[path]
            authority = contract.get("authority")
            assertion = (
                authority.get("assertion")
                if isinstance(authority, dict)
                else None
            )
            assertion_sha256 = (
                assertion.get("sha256")
                if isinstance(assertion, dict)
                else None
            )
            stdout = (
                f"running 1 test\ntest {test_id} ... ok\n\n"
                "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
                "0 filtered out; finished in 0.00s\n"
            ).encode()
            actor_execution_cases.append({
                "path": path,
                "test_id": test_id,
                "authority_path": "tests/real_world_cli.rs",
                "command": [
                    actor_cargo_authority["cargo"]["path"], "test", "--test",
                    "real_world_cli", test_id, "--", "--exact"
                ],
                "cargo_toolchain_identity_sha256": actor_cargo_authority["identity_sha256"],
                "cargo_execution": actor_cargo_execution,
                "cwd": str(census.ROOT),
                "run_status": "completed",
                "exit_code": 0,
                "fixture_sha256_expected": source["sha256"],
                "fixture_sha256_before": source["sha256"],
                "fixture_sha256_after": source["sha256"],
                "test_source_sha256_expected": assertion_sha256,
                "test_source_sha256_before": assertion_sha256,
                "test_source_sha256_after": assertion_sha256,
                "helper_sha256_expected": census.ACTOR_HELPER_SHA256,
                "helper_sha256_observed": census.ACTOR_HELPER_SHA256,
                "stdout_raw": census._raw_capture(
                    stdout, f"actor-test-{index:02d}.stdout.bin"
                ),
                "stderr_raw": census._raw_capture(
                    b"", f"actor-test-{index:02d}.stderr.bin"
                ),
            })
        actor_execution = {
            "status": "passed",
            "cases": actor_execution_cases,
        }
        actors = census._actor_fallback_evidence(
            manifest,
            all_records,
            actor_contracts,
            actor_execution,
            root=census.ROOT,
            cargo_authority=actor_cargo_authority,
        )
        owner_stdout = census._raw_capture(
            b"owner audit output\n", "legacy-owner-audit.stdout.bin"
        )
        owner_stderr = census._raw_capture(
            b"", "legacy-owner-audit.stderr.bin"
        )
        owner_audit = cls._owner_audit()
        owner_audit["stdout_raw"] = owner_stdout
        owner_audit["stderr_raw"] = owner_stderr
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as outside:
            logs_root = Path(temporary) / "logs"
            logs_root.mkdir()
            sidecar_write_root = logs_root
            if sidecar_root_symlink:
                sidecar_write_root = Path(outside) / "logs"
                sidecar_write_root.mkdir()
                logs_root.rmdir()
                logs_root.symlink_to(sidecar_write_root, target_is_directory=True)
            sidecar_bytes: dict[str, bytes] = {}
            for record in all_records:
                captures = [record["stdout_raw"], record["stderr_raw"]]
                expectation = record.get("expectation")
                proof = (
                    expectation.get("test_execution_proof")
                    if isinstance(expectation, dict)
                    else None
                )
                if isinstance(proof, dict):
                    for field in ("target_build", "exact_test_run"):
                        command_record = proof.get(field)
                        if isinstance(command_record, dict):
                            captures.extend([
                                command_record.get("stdout_raw"),
                                command_record.get("stderr_raw"),
                            ])
                native = record.get("native_executable_run")
                if isinstance(native, dict):
                    captures.extend([native["stdout_raw"], native["stderr_raw"]])
                for capture in captures:
                    raw = census._captured_bytes(capture)
                    if raw is not None:
                        sidecar_bytes[capture["path"]] = raw
            sidecar_bytes[owner_stdout["path"]] = census._captured_bytes(owner_stdout)
            sidecar_bytes[owner_stderr["path"]] = census._captured_bytes(owner_stderr)
            for actor_test in actor_execution_cases:
                for key in ("stdout_raw", "stderr_raw"):
                    capture = actor_test[key]
                    sidecar_bytes[capture["path"]] = census._captured_bytes(capture)
            for name, raw in sidecar_bytes.items():
                if not isinstance(name, str) or Path(name).name != name or not name.endswith(".bin"):
                    continue
                override = (sidecar_overrides or {}).get(name, raw)
                if override is None:
                    continue
                if override == "symlink":
                    target = sidecar_write_root / f"{name}.target"
                    target.write_bytes(raw)
                    (sidecar_write_root / name).symlink_to(target)
                else:
                    assert isinstance(override, bytes)
                    (sidecar_write_root / name).write_bytes(override)
            return census._gate_report(
                all_records,
                manifest,
                True,
                list(excluded_symlinks),
                tracked,
                actors,
                owner_audit,
                expectation_contract,
                source_scope=source_scope,
                logs_root=logs_root,
                root=root or census.ROOT,
                expected_rejection_source_root=effective_root or census.ROOT,
                cargo_authority=actor_cargo_authority,
                expected_rejection_cargo_authority=(
                    census.cargo_test_toolchain_authority(effective_root or census.ROOT)
                ),
            )

    def test_gate_can_pass_only_for_exact_canonical_zero_fallback_roster(self) -> None:
        gate = self._gate(["tests/a.mimi"], [self._record("tests/a.mimi")])
        self.assertTrue(gate["passed"])

    def test_canonical_disposition_without_observed_route_telemetry_fails(self) -> None:
        record = self._record("tests/a.mimi")
        record["route"] = {"disposition": "canonical"}
        gate = self._gate(["tests/a.mimi"], [record])

        self.assertFalse(gate["passed"])
        self.assertFalse(gate["checks"]["all_canonical_expectations_are_proven"])
        self.assertNotIn("tests/a.mimi", gate["canonical_success_inputs"])

    def test_compatibility_route_fallback_and_missing_item_fail_closed(self) -> None:
        records = [
            self._record("tests/a.mimi", route="compatibility"),
            self._record("tests/extra.mimi", fallback=True),
        ]
        gate = self._gate(
            ["tests/a.mimi", "tests/extra.mimi"],
            records,
            missing_manifest_paths=("tests/b.mimi",),
        )
        self.assertFalse(gate["passed"])
        self.assertFalse(gate["checks"]["all_routes_are_canonical_or_authorized_rejections"])
        self.assertFalse(gate["checks"]["no_legacy_or_emission_fallback"])
        self.assertFalse(gate["checks"]["nonempty_exact_input_denominator"])
        self.assertIn("tests/b.mimi", gate["missing_manifest_records"])

    def test_source_diagnostic_classification_still_fails_nonzero_build(self) -> None:
        record = self._record("tests/a.mimi", route="rejected")
        record["exit_code"] = 1
        record["route"] = {
            "disposition": "rejected",
            "classification_status": "inferred_pre_route_rejection",
        }
        record["failure_classification"] = {
            "class": "source_diagnostic",
            "status": "classified",
        }
        record["expectation"] = {"matched": False}

        gate = self._gate(["tests/a.mimi"], [record])

        self.assertFalse(gate["passed"])
        self.assertTrue(gate["checks"]["no_unclassified_failure_causes"])
        self.assertFalse(gate["checks"]["all_builds_succeeded_or_rejected_as_declared"])
        self.assertFalse(gate["checks"]["all_outcomes_match_expectations"])
        self.assertFalse(gate["checks"]["all_routes_are_canonical_or_authorized_rejections"])

    def test_external_dependency_classification_still_fails_nonzero_build(self) -> None:
        record = self._record("tests/fixtures/v02_app/w4_loopback_service.mimi")
        record["exit_code"] = 1
        record["failure_classification"] = census._failure_classification(
            run_status="completed",
            exit_code=1,
            text="ld.lld: error: undefined symbol: v02_checksum\n",
            target_kind="executable",
            route={"disposition": "canonical"},
        )
        record["expectation"] = {"matched": False}

        gate = self._gate([record["path"]], [record])

        self.assertFalse(gate["passed"])
        self.assertTrue(gate["checks"]["no_unclassified_failure_causes"])
        self.assertFalse(gate["checks"]["all_builds_succeeded_or_rejected_as_declared"])
        self.assertFalse(gate["checks"]["all_outcomes_match_expectations"])
        self.assertTrue(gate["checks"]["all_routes_are_canonical_or_authorized_rejections"])

    def test_unknown_failure_class_and_incomplete_function_roster_fail_closed(self) -> None:
        record = self._record("tests/a.mimi")
        record["failure_classification"] = {"class": "unknown", "status": "unclassified"}
        record["per_function"]["named_roster_complete"] = False
        gate = self._gate(["tests/a.mimi"], [record])
        self.assertFalse(gate["passed"])
        self.assertEqual(gate["unclassified_failure_inputs"], ["tests/a.mimi"])
        self.assertEqual(gate["per_function_incomplete_inputs"], ["tests/a.mimi"])

    def test_gate_fails_when_tracked_manifest_identity_is_incomplete(self) -> None:
        gate = self._gate(
            ["tests/a.mimi"],
            [self._record("tests/a.mimi")],
            tracked_inventory={
                "status": "available",
                "path_count": 99,
                "paths_sha256": "f" * 64,
            },
        )
        self.assertFalse(gate["tracked_manifest_path_count_matches"])
        self.assertFalse(gate["tracked_manifest_paths_sha256_matches"])
        self.assertFalse(gate["checks"]["tracked_git_manifest_available"])

    def test_raw_log_hash_mismatch_fails_closed(self) -> None:
        record = self._record("tests/a.mimi")
        record["stdout_raw"]["sha256"] = "f" * 64
        gate = self._gate(["tests/a.mimi"], [record])
        self.assertFalse(gate["checks"]["all_stdout_stderr_logs_are_retained"])

    def test_stdout_stderr_sidecars_must_exist_as_regular_files_and_match_inline_bytes(self) -> None:
        record = self._record("tests/a.mimi")
        stdout_sidecar = record["stdout_raw"]["path"]
        for label, replacement in (
            ("tampered", b"changed"),
            ("missing", None),
            ("symlink", "symlink"),
        ):
            with self.subTest(label=label):
                gate = self._gate(
                    ["tests/a.mimi"],
                    [record],
                    sidecar_overrides={stdout_sidecar: replacement},
                )
                self.assertFalse(gate["checks"]["all_stdout_stderr_logs_are_retained"])
                self.assertIn("tests/a.mimi", gate["missing_raw_log_sidecars"])

    def test_raw_capture_sidecar_traversal_and_symlinked_artifact_root_fail_closed(self) -> None:
        record = self._record("tests/a.mimi")
        record["stdout_raw"]["path"] = "../outside.bin"
        traversal_gate = self._gate(["tests/a.mimi"], [record])
        self.assertFalse(traversal_gate["checks"]["all_stdout_stderr_logs_are_retained"])
        self.assertIn("tests/a.mimi", traversal_gate["missing_raw_log_sidecars"])
        self.assertIn("tests/a.mimi", traversal_gate["missing_raw_log_inputs"])

        root_symlink_gate = self._gate(
            ["tests/a.mimi"],
            [self._record("tests/a.mimi")],
            sidecar_root_symlink=True,
        )
        self.assertFalse(root_symlink_gate["checks"]["all_stdout_stderr_logs_are_retained"])
        self.assertIn("tests/a.mimi", root_symlink_gate["missing_raw_log_sidecars"])

    def test_every_referenced_stdout_stderr_sidecar_is_validated(self) -> None:
        record = self._record("tests/a.mimi")
        sidecars = [
            ("record stdout", record["stdout_raw"]["path"], "tests/a.mimi"),
            ("record stderr", record["stderr_raw"]["path"], "tests/a.mimi"),
            (
                "native stdout",
                record["native_executable_run"]["stdout_raw"]["path"],
                "tests/a.mimi",
            ),
            (
                "native stderr",
                record["native_executable_run"]["stderr_raw"]["path"],
                "tests/a.mimi",
            ),
            ("owner stdout", "legacy-owner-audit.stdout.bin", "legacy-owner-audit"),
            ("owner stderr", "legacy-owner-audit.stderr.bin", "legacy-owner-audit"),
        ]
        for label, path, reported_owner in sidecars:
            with self.subTest(sidecar=label):
                gate = self._gate(
                    ["tests/a.mimi"],
                    [record],
                    sidecar_overrides={path: b"tampered sidecar"},
                )
                self.assertFalse(gate["checks"]["all_stdout_stderr_logs_are_retained"])
                self.assertIn(reported_owner, gate["missing_raw_log_sidecars"])

    def test_raw_output_reference_must_name_a_bin_sidecar(self) -> None:
        record = self._record("tests/a.mimi")
        record["stdout_raw"]["path"] = "stdout.log"
        gate = self._gate(["tests/a.mimi"], [record])

        self.assertFalse(gate["checks"]["all_stdout_stderr_logs_are_retained"])
        self.assertIn("tests/a.mimi", gate["missing_raw_log_sidecars"])

    def test_actor_native_executable_wrong_stdout_blocks_gate(self) -> None:
        path = census.ACTOR_FALLBACK_INPUTS[0]
        record = self._record(path)
        record["native_executable_run"]["stdout_raw"] = census._raw_capture(
            b"wrong\n", record["native_executable_run"]["stdout_raw"]["path"]
        )
        gate = self._gate([path], [record])
        self.assertFalse(gate["checks"]["actor_fallback_cases_are_canonical_without_fallback"])
        self.assertIn(path, gate["actor_fallback_native_output_mismatches"])

    def test_unknown_and_unmapped_candidates_are_counted_and_block_the_gate(self) -> None:
        unknown = self._record("tests/unknown.mimi", route="rejected")
        unknown["expectation"] = {"class": "required_compile", "matched": False}
        unmapped = self._record("tests/unmapped.mimi", route="unobserved")
        unmapped["run_status"] = "not_run"
        unmapped["exit_code"] = None
        unmapped["expectation"] = {"class": "unmapped", "matched": False}

        gate = self._gate(
            ["tests/unknown.mimi", "tests/unmapped.mimi"], [unknown, unmapped]
        )

        self.assertEqual(
            gate["candidate_classification_counts"]["unknown"], 1
        )
        self.assertEqual(
            gate["candidate_classification_counts"]["unmapped"], 1
        )
        self.assertEqual(
            gate["unknown_or_unmapped_candidates"],
            ["tests/unknown.mimi", "tests/unmapped.mimi"],
        )
        self.assertTrue(gate["checks"]["all_candidates_have_one_route_classification"])
        self.assertFalse(gate["checks"]["no_unknown_or_unmapped_route_candidates"])
        self.assertFalse(gate["passed"])

    def test_duplicate_candidate_records_fail_exact_once_accounting(self) -> None:
        path = "tests/duplicate.mimi"
        manifest = [
            {"path": path, "present": True, "tracked_by_git": False},
            {"path": path, "present": True, "tracked_by_git": False},
        ]
        records = [self._record(path), self._record(path)]
        actors = census._actor_fallback_evidence(manifest, records)
        gate = census._gate_report(
            records,
            manifest,
            True,
            [],
            {"status": "available", "path_count": 0, "paths_sha256": census._manifest_paths_hash([])},
            actors,
            self._owner_audit(),
        )

        self.assertEqual(gate["duplicate_record_paths"], [path])
        self.assertEqual(gate["scope_duplicate_paths"], [path])
        self.assertFalse(gate["checks"]["nonempty_exact_input_denominator"])
        self.assertFalse(gate["checks"]["source_scope_is_accounted_exactly_once"])
        self.assertFalse(gate["passed"])

    def test_missing_actor_record_or_owner_row_fails_closed(self) -> None:
        manifest = [
            {"path": path, "present": True, "tracked_by_git": False}
            for path in census.ACTOR_FALLBACK_INPUTS
        ]
        records = [self._record(path) for path in census.ACTOR_FALLBACK_INPUTS]
        actors = census._actor_fallback_evidence(manifest, records[:1])
        owner_audit = self._owner_audit()
        owner_audit["owners"] = owner_audit["owners"][1:]
        gate = census._gate_report(
            records,
            manifest,
            True,
            [],
            {"status": "available", "path_count": 0, "paths_sha256": census._manifest_paths_hash([])},
            actors,
            owner_audit,
        )
        self.assertFalse(gate["checks"]["both_actor_fallback_cases_are_in_denominator"])
        self.assertFalse(
            gate["checks"][
                "two_active_owners_audited_and_two_retired_owners_statically_verified"
            ]
        )


class ExpectationContractTests(unittest.TestCase):
    @staticmethod
    def _entry(
        root: Path,
        source_path: str,
        source_sha256: str,
        *,
        expectation_class: str = "required_compile",
        expected_exit_code: int | None = None,
    ) -> dict:
        is_rejection = expectation_class == "expected_build_rejection"
        assertion_relative_path = (
            "tests/route_contract.rs" if is_rejection else "tests/route_contract.py"
        )
        assertion_path = root / assertion_relative_path
        assertion_path.parent.mkdir(parents=True, exist_ok=True)
        if is_rejection and not (root / "Cargo.toml").exists():
            (root / "Cargo.toml").write_text(
                '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n',
                encoding="utf-8",
            )
        test_id = "test_route_contract"
        if is_rejection:
            fixture_path = "".join(
                f'.join({json.dumps(part)})' for part in Path(source_path).parts
            )
            assertion_path.write_text(
                "#[test]\n"
                f"fn {test_id}() {{\n"
                "    let fixture = ::std::path::Path::new(::std::env!(\"CARGO_MANIFEST_DIR\"))"
                f"{fixture_path};\n"
                "    let result = ::std::process::Command::new(::std::env!(\"CARGO_BIN_EXE_mimi\"))\n"
                "        .current_dir(::std::env!(\"CARGO_MANIFEST_DIR\"))\n"
                '        .arg("build")\n'
                "        .arg(&fixture)\n"
                '        .arg("-o")\n'
                '        .arg("route-contract-output")\n'
                "        .output()\n"
                '        .expect("failed to execute fixture build");\n'
                "    ::std::assert!(!result.status.success());\n"
                "    let stderr = ::std::string::String::from_utf8_lossy(&result.stderr);\n"
                '    ::std::assert!(stderr.contains("E0432"));\n'
                '    ::std::assert!(stderr.contains("expected negative diagnostic") && '
                'stderr.contains("canonical MIR build rejected: expected diagnostic"));\n'
                "}\n",
                encoding="utf-8",
            )
            assertion_line = 14
        else:
            assertion_path.write_text(
                f"def {test_id}():\n    assert compile_fixture()\n", encoding="utf-8"
            )
            assertion_line = 2
        entry = {
            "class": expectation_class,
            "reason": "explicit fixture contract",
            "source_sha256": source_sha256,
            "authority": {
                "test_id": test_id,
                "assertion": {
                    "path": assertion_relative_path,
                    "line": assertion_line,
                    "sha256": census.sha256_file(assertion_path),
                },
            },
        }
        if expected_exit_code is not None:
            entry["expected_exit_code"] = expected_exit_code
        if expectation_class == "expected_build_rejection":
            entry["expected_diagnostic_codes"] = ["E0432"]
            entry["required_output_substrings"] = ["expected negative diagnostic"]
            entry["expected_route_disposition"] = "rejected"
            entry["expected_route_classification_status"] = "observed"
            entry["expected_route_reason"] = "expected diagnostic"
            entry["expected_failure_class"] = "capability"
        return entry

    def _load_and_bind(
        self,
        root: Path,
        inputs: dict[str, dict],
        sources: list[dict],
    ) -> tuple[dict, dict]:
        expectations_path = root / "expectations.json"
        expectations_path.write_text(
            json.dumps(
                {"schema_version": census.EXPECTATION_SCHEMA_VERSION, "inputs": inputs}
            ),
            encoding="utf-8",
        )
        declared, _identity = census.load_expectations(expectations_path, root)
        return census.bind_expectations_to_inventory(declared, sources, root)

    def _execute_authority_tests(
        self,
        root: Path,
        sources: list[dict],
        bound: dict,
        *,
        target_source: Path | None = None,
        target_kind: str = "test",
        run_stdout: bytes | None = None,
    ) -> dict:
        logs = root / "authority-logs"
        logs.mkdir(exist_ok=True)
        first = next(
            item for item in bound.values()
            if item.get("class") == "expected_build_rejection"
        )
        authority_path = root / first["authority"]["assertion"]["path"]
        source_path = target_source or authority_path
        root_manifest = str(root / "Cargo.toml")
        kind = ["test"] if target_kind == "test" else [target_kind]
        target = {
            "name": source_path.stem,
            "src_path": str(source_path.resolve()),
            "kind": kind,
            "crate_types": ["bin"],
            "test": target_kind == "test",
            "doc": False,
        }
        package_id = "fixture 0.1.0 (path+file:///fixture)"
        metadata = {
            "packages": [
                {
                    "id": package_id,
                    "name": "fixture",
                    "manifest_path": root_manifest,
                    "targets": [target],
                }
            ],
            "workspace_members": [package_id],
            "workspace_root": str(root),
            "target_directory": str(root / "target"),
        }

        def fake_run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[bytes]:
            if command[1] == "metadata":
                return subprocess.CompletedProcess(
                    command, 0, json.dumps(metadata).encode("utf-8"), b""
                )
            if "--no-run" in command:
                return subprocess.CompletedProcess(command, 0, b"Finished test target\n", b"")
            test_id = next(
                (str(part) for part in command if str(part).startswith("test_")),
                "test_route_contract",
            )
            stdout = run_stdout
            if stdout is None:
                stdout = (
                    f"running 1 test\ntest {test_id} ... ok\n\n"
                    "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
                    "0 filtered out; finished in 0.00s\n"
                ).encode("utf-8")
            return subprocess.CompletedProcess(command, 0, stdout, b"")

        cargo_authority = census.cargo_test_toolchain_authority(root)
        with patch.object(census.subprocess, "run", side_effect=fake_run):
            result = census._run_expected_rejection_authority_tests(
                root, logs, sources, bound, timeout=5,
                cargo_authority=cargo_authority,
            )
        return result

    def _valid_expected_rejection_record(
        self, root: Path, source_path: str = "case.mimi"
    ) -> tuple[dict, dict, dict, dict]:
        source_file = root / source_path
        source_file.parent.mkdir(parents=True, exist_ok=True)
        source_file.write_text("func main() { }\n", encoding="utf-8")
        source = census.source_record(source_file, root)
        entry = self._entry(
            root,
            source_path,
            source["sha256"],
            expectation_class="expected_build_rejection",
            expected_exit_code=1,
        )
        bound, coverage = self._load_and_bind(root, {source_path: entry}, [source])
        authority_execution = self._execute_authority_tests(root, [source], bound)
        coverage["invalid_authority_paths"] = sorted(
            path for path, expectation in bound.items()
            if expectation.get("class") != "unmapped"
            and expectation.get("authority_valid") is not True
        )
        output = (
            b"error[E0432]: expected negative diagnostic\n"
            b"canonical MIR build rejected: expected diagnostic\n"
        )
        result = census._expectation_result(
            source_path,
            1,
            EXPECTED_REJECTION_ROUTE,
            bound,
            run_status="completed",
            failure_classification={
                "class": "capability",
                "status": "classified",
                "reason": "compiler reported an explicit Canonical/native capability rejection",
            },
            captured_output=output,
        )
        record = StrictGateTests._record(source_path, route="rejected")
        record["route"] = dict(EXPECTED_REJECTION_ROUTE)
        record["exit_code"] = 1
        record["output_artifact"] = None
        record["failure_classification"] = {
            "class": "capability",
            "status": "classified",
            "reason": "compiler reported an explicit Canonical/native capability rejection",
        }
        record["expectation"] = result
        self.assertEqual(authority_execution["status"], "passed")
        return source, record, coverage, bound

    def _run_expected_rejection_with_path_artifact(self, artifact_kind: str) -> dict:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_build_rejection",
                expected_exit_code=1,
            )
            bound, _coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            scratch = root / "scratch"
            scratch.mkdir()
            logs = root / "logs"
            logs.mkdir()

            def fake_run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[bytes]:
                output_path = Path(command[-1])
                if artifact_kind == "directory":
                    output_path.mkdir()
                elif artifact_kind == "dangling_symlink":
                    output_path.symlink_to(output_path.parent / "missing-target")
                else:
                    raise AssertionError(f"unsupported artifact kind: {artifact_kind}")
                return subprocess.CompletedProcess(
                    command,
                    1,
                    stdout=b"",
                    stderr=(
                        b"error[E0432]: expected negative diagnostic\n"
                        b"canonical MIR build rejected: expected diagnostic\n"
                    ),
                )

            with patch.object(census.subprocess, "run", side_effect=fake_run):
                return census._run_one(
                    root=root,
                    source=source,
                    index=1,
                    mimi=root / "mimi",
                    timeout=5,
                    scratch_root=scratch,
                    logs_root=logs,
                    expected=bound,
                )

    def test_missing_expectations_are_unmapped_and_do_not_change_denominator(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_paths = [
                "docs/staging/sample-known-gap.mimi",
                "tests/fixtures/interpreter_only.mimi",
            ]
            sources = []
            for name in source_paths:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("func main() { }\n", encoding="utf-8")
                sources.append(census.source_record(path, root))
            bound, coverage = census.bind_expectations_to_inventory({}, sources, root)
            observed = StrictGateTests._record(source_paths[0], route="canonical")
            observed["expectation"] = census._expectation_result(
                source_paths[0],
                0,
                {"disposition": "canonical"},
                bound,
                run_status="completed",
            )
            gate = StrictGateTests._gate(
                [source_paths[0]],
                [observed],
                missing_manifest_paths=(source_paths[1],),
                expectation_contract=coverage,
            )

        self.assertEqual([row["path"] for row in sources], source_paths)
        self.assertEqual(set(bound), set(source_paths))
        self.assertTrue(all(item["class"] == "unmapped" for item in bound.values()))
        self.assertEqual(coverage["missing_paths"], sorted(source_paths))
        self.assertFalse(coverage["exact_bijection"])
        result = census._expectation_result(
            source_paths[0],
            0,
            {"disposition": "canonical"},
            bound,
            run_status="completed",
        )
        self.assertEqual(result["status"], "unmapped")
        self.assertFalse(result["matched"])
        self.assertFalse(gate["checks"]["expectation_paths_exactly_biject_inventory"])
        self.assertFalse(gate["checks"]["all_outcomes_match_expectations"])
        self.assertNotIn(source_paths[0], gate["canonical_success_inputs"])

    def test_schema_requires_typed_class_test_assertion_and_both_source_hashes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(root, "case.mimi", source["sha256"])
            bound, coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])

            self.assertEqual(bound["case.mimi"]["class"], "required_compile")
            self.assertTrue(bound["case.mimi"]["authority_valid"])
            self.assertEqual(bound["case.mimi"]["source_hash_status"], "matched")
            self.assertTrue(coverage["exact_bijection"])

            entry.pop("authority")
            with self.assertRaisesRegex(ValueError, "authority"):
                self._load_and_bind(root, {"case.mimi": entry}, [source])

            rejection_entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_build_rejection",
                expected_exit_code=1,
            )
            rejection_entry.pop("expected_diagnostic_codes")
            with self.assertRaisesRegex(ValueError, "expected_diagnostic_codes"):
                self._load_and_bind(root, {"case.mimi": rejection_entry}, [source])

    def test_expected_rejection_schema_requires_disposition_reason_and_failure(self) -> None:
        required_fields = (
            "expected_route_disposition",
            "expected_route_classification_status",
            "expected_route_reason",
            "expected_failure_class",
        )
        for field in required_fields:
            with self.subTest(field=field), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source_path = root / "case.mimi"
                source_path.write_text("func main() { }\n", encoding="utf-8")
                source = census.source_record(source_path, root)
                entry = self._entry(
                    root,
                    "case.mimi",
                    source["sha256"],
                    expectation_class="expected_build_rejection",
                    expected_exit_code=1,
                )
                entry.pop(field)
                with self.assertRaisesRegex(ValueError, field):
                    self._load_and_bind(root, {"case.mimi": entry}, [source])

    def test_schema_version_rejects_wrong_json_types(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            expectations_path = root / "expectations.json"
            for invalid_schema_version in (
                2,
                float(census.EXPECTATION_SCHEMA_VERSION),
                True,
                str(census.EXPECTATION_SCHEMA_VERSION),
            ):
                with self.subTest(schema_version=invalid_schema_version):
                    expectations_path.write_text(
                        json.dumps(
                            {"schema_version": invalid_schema_version, "inputs": {}}
                        ),
                        encoding="utf-8",
                    )
                    with self.assertRaisesRegex(ValueError, "schema_version"):
                        census.load_expectations(expectations_path, root)

    def test_all_expectation_classes_are_explicit_and_typed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            classes = [
                "required_compile",
                "expected_build_rejection",
                "interpreter_only",
                "known_gap",
                "unmapped",
                "environment_failure",
            ]
            sources = []
            inputs: dict[str, dict] = {}
            for index, expectation_class in enumerate(classes):
                name = f"case-{index}.mimi"
                source_path = root / name
                source_path.write_text("func main() { }\n", encoding="utf-8")
                source = census.source_record(source_path, root)
                sources.append(source)
                if expectation_class == "unmapped":
                    inputs[name] = {"class": expectation_class, "reason": "awaiting mapping"}
                else:
                    expected_code = 1 if expectation_class in {
                        "expected_build_rejection",
                        "environment_failure",
                    } else None
                    inputs[name] = self._entry(
                        root,
                        name,
                        source["sha256"],
                        expectation_class=expectation_class,
                        expected_exit_code=expected_code,
                    )
            bound, coverage = self._load_and_bind(root, inputs, sources)

        self.assertEqual(
            [bound[f"case-{index}.mimi"]["class"] for index in range(len(classes))],
            classes,
        )
        self.assertTrue(coverage["exact_bijection"])

    def test_expectation_paths_must_biject_inventory_and_duplicate_json_keys_reject(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(root, "case.mimi", source["sha256"])
            _bound, coverage = self._load_and_bind(
                root,
                {"case.mimi": entry, "extra.mimi": dict(entry)},
                [source, {**source, "path": "missing.mimi"}],
            )
            record = StrictGateTests._record("case.mimi")
            gate = StrictGateTests._gate(
                ["case.mimi"],
                [record],
                missing_manifest_paths=("missing.mimi",),
                expectation_contract=coverage,
            )
            expectations_path = root / "duplicate.json"
            expectations_path.write_text(
                f'{{"schema_version":{census.EXPECTATION_SCHEMA_VERSION},"inputs":{{"case.mimi":{{}},"case.mimi":{{}}}}}}',
                encoding="utf-8",
            )

            self.assertFalse(coverage["exact_bijection"])
            self.assertEqual(coverage["missing_paths"], ["missing.mimi"])
            self.assertEqual(coverage["extra_paths"], ["extra.mimi"])
            self.assertFalse(gate["checks"]["expectation_paths_exactly_biject_inventory"])
            with self.assertRaisesRegex(ValueError, "duplicate JSON object key"):
                census.load_expectations(expectations_path, root)

        duplicate_inventory = census.bind_expectations_to_inventory(
            {},
            [
                {"path": "same.mimi", "sha256": "a" * 64},
                {"path": "same.mimi", "sha256": "a" * 64},
            ],
            Path("."),
        )[1]
        self.assertEqual(duplicate_inventory["duplicate_inventory_paths"], ["same.mimi"])
        self.assertFalse(duplicate_inventory["exact_bijection"])

    def test_expectation_source_hash_mismatch_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(root, "case.mimi", "0" * 64)
            bound, coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            result = census._expectation_result(
                "case.mimi",
                0,
                {"disposition": "canonical"},
                bound,
                run_status="completed",
            )

        self.assertEqual(coverage["invalid_source_hash_paths"], ["case.mimi"])
        self.assertEqual(result["status"], "invalid_evidence")
        self.assertFalse(result["matched"])

    def test_expectation_authority_hash_or_location_mismatch_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(root, "case.mimi", source["sha256"])
            (root / "tests/route_contract.py").write_text(
                "assert changed_fixture()\n", encoding="utf-8"
            )
            bound, coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            result = census._expectation_result(
                "case.mimi",
                0,
                {"disposition": "canonical"},
                bound,
                run_status="completed",
            )

        self.assertEqual(coverage["invalid_authority_paths"], ["case.mimi"])
        self.assertEqual(result["authority_status"], "source_location_or_hash_mismatch")
        self.assertEqual(result["status"], "invalid_evidence")
        self.assertFalse(result["matched"])

    def test_expectation_authority_requires_named_test_and_assertion_line(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(root, "case.mimi", source["sha256"])

            entry["authority"]["test_id"] = "test_missing_from_source"
            missing_test_bound, missing_test_coverage = self._load_and_bind(
                root, {"case.mimi": entry}, [source]
            )

            entry["authority"]["test_id"] = "test_route_contract"
            entry["authority"]["assertion"]["line"] = 1
            non_assertion_bound, non_assertion_coverage = self._load_and_bind(
                root, {"case.mimi": entry}, [source]
            )

        self.assertEqual(missing_test_coverage["invalid_authority_paths"], ["case.mimi"])
        self.assertFalse(missing_test_bound["case.mimi"]["authority_valid"])
        self.assertEqual(non_assertion_coverage["invalid_authority_paths"], ["case.mimi"])
        self.assertFalse(non_assertion_bound["case.mimi"]["authority_valid"])

    def test_expected_negative_matches_are_reported_separately_from_canonical_success(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_build_rejection",
                expected_exit_code=1,
            )
            bound, coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            execution = self._execute_authority_tests(root, [source], bound)
            coverage["invalid_authority_paths"] = sorted(
                path for path, expectation in bound.items()
                if expectation.get("class") != "unmapped"
                and expectation.get("authority_valid") is not True
            )
            result = census._expectation_result(
                "case.mimi",
                1,
                EXPECTED_REJECTION_ROUTE,
                bound,
                run_status="completed",
                failure_classification={
                    "class": "capability",
                    "status": "classified",
                },
                captured_output=(
                    b"error[E0432]: expected negative diagnostic\n"
                    b"canonical MIR build rejected: expected diagnostic\n"
                ),
            )
            record = StrictGateTests._record("case.mimi", route="rejected")
            record["exit_code"] = 1
            record["route"] = dict(EXPECTED_REJECTION_ROUTE)
            record["output_artifact"] = None
            record["expectation"] = result
            record["failure_classification"] = {
                "class": "capability",
                "status": "classified",
                "reason": "compiler reported an explicit Canonical/native capability rejection",
            }
            gate = StrictGateTests._gate(
                ["case.mimi"], [record]
            )

        self.assertTrue(result["matched"])
        self.assertTrue(result["authority_valid"])
        self.assertEqual(execution["status"], "passed")
        self.assertTrue(coverage["exact_bijection"])
        self.assertEqual(gate["expected_negative_matches"], ["case.mimi"])
        self.assertEqual(gate["passed_expected_rejections"], ["case.mimi"])
        self.assertEqual(gate["unexpected_failure_inputs"], [])
        self.assertEqual(gate["candidate_classification_counts"]["intentional_reject"], 1)
        self.assertEqual(gate["intentional_reject_candidates"], ["case.mimi"])
        self.assertNotIn("case.mimi", gate["canonical_success_inputs"])
        self.assertTrue(gate["checks"]["all_outcomes_match_expectations"])
        self.assertTrue(gate["checks"]["all_routes_are_canonical_or_authorized_rejections"])
        self.assertTrue(gate["checks"]["all_builds_succeeded_or_rejected_as_declared"])
        self.assertTrue(gate["passed"])

    def test_expected_rejection_requires_exact_observed_route_and_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _source, record, coverage, bound = self._valid_expected_rejection_record(root)
            output = (
                b"error[E0432]: expected negative diagnostic\n"
                b"canonical MIR build rejected: expected diagnostic\n"
            )
            cases = {
                "wrong_disposition": (
                    {**EXPECTED_REJECTION_ROUTE, "disposition": "compatibility"},
                    {"class": "capability", "status": "classified"},
                ),
                "wrong_classification_status": (
                    {**EXPECTED_REJECTION_ROUTE, "classification_status": "inferred_pre_route_rejection"},
                    {"class": "capability", "status": "classified"},
                ),
                "wrong_route_reason": (
                    {**EXPECTED_REJECTION_ROUTE, "reason": "a different rejection"},
                    {"class": "capability", "status": "classified"},
                ),
                "wrong_failure_class": (
                    EXPECTED_REJECTION_ROUTE,
                    {"class": "source_diagnostic", "status": "classified"},
                ),
                "unclassified_failure": (
                    EXPECTED_REJECTION_ROUTE,
                    {"class": "capability", "status": "unclassified"},
                ),
            }
            for name, (route, failure) in cases.items():
                with self.subTest(case=name):
                    result = census._expectation_result(
                        "case.mimi",
                        1,
                        route,
                        bound,
                        run_status="completed",
                        failure_classification=failure,
                        captured_output=output,
                    )
                    self.assertFalse(result["matched"])

    def test_expected_rejection_unexpected_success_and_compatibility_fallback_fail(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _source, base_record, coverage, bound = self._valid_expected_rejection_record(root)
            output = (
                b"error[E0432]: expected negative diagnostic\n"
                b"canonical MIR build rejected: expected diagnostic\n"
            )
            success_route = {"disposition": "canonical"}
            unexpected_success = dict(base_record)
            unexpected_success["route"] = success_route
            unexpected_success["exit_code"] = 0
            unexpected_success["failure_classification"] = {
                "class": "none",
                "status": "not_applicable",
            }
            unexpected_success["output_artifact"] = {"sha256": "b" * 64, "bytes": 6}
            unexpected_success["expectation"] = census._expectation_result(
                "case.mimi",
                0,
                success_route,
                bound,
                run_status="completed",
                failure_classification=unexpected_success["failure_classification"],
                captured_output=output,
                output_artifact=unexpected_success["output_artifact"],
            )
            success_gate = StrictGateTests._gate(
                ["case.mimi"], [unexpected_success], expectation_contract=coverage
            )
            self.assertFalse(success_gate["passed"])
            self.assertEqual(success_gate["passed_expected_rejections"], [])
            self.assertEqual(success_gate["unproven_expected_rejection_inputs"], ["case.mimi"])
            self.assertEqual(success_gate["unexpected_failure_inputs"], [])
            self.assertEqual(success_gate["unexpected_or_mismatched_inputs"], ["case.mimi"])

            compatibility_route = {
                "disposition": "compatibility",
                "classification_status": "observed",
                "reason": "legacy path",
            }
            compatibility = dict(base_record)
            compatibility["route"] = compatibility_route
            compatibility["per_function"] = {
                "named_roster_complete": True,
                "has_fallback": True,
            }
            compatibility["expectation"] = census._expectation_result(
                "case.mimi",
                1,
                compatibility_route,
                bound,
                run_status="completed",
                failure_classification=base_record["failure_classification"],
                captured_output=output,
            )
            compatibility_gate = StrictGateTests._gate(
                ["case.mimi"], [compatibility], expectation_contract=coverage
            )
            self.assertFalse(compatibility_gate["passed"])
            self.assertEqual(compatibility_gate["passed_expected_rejections"], [])
            self.assertEqual(compatibility_gate["fallback_inputs"], ["case.mimi"])
            self.assertEqual(compatibility_gate["unexpected_failure_inputs"], ["case.mimi"])

    def test_missing_authority_cannot_make_expected_rejection_pass(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _source, record, coverage, bound = self._valid_expected_rejection_record(root)
            bound["case.mimi"]["authority_valid"] = False
            output = (
                b"error[E0432]: expected negative diagnostic\n"
                b"canonical MIR build rejected: expected diagnostic\n"
            )
            record["expectation"] = census._expectation_result(
                "case.mimi",
                1,
                EXPECTED_REJECTION_ROUTE,
                bound,
                run_status="completed",
                failure_classification=record["failure_classification"],
                captured_output=output,
            )
            gate = StrictGateTests._gate(
                ["case.mimi"], [record], expectation_contract=coverage
            )

        self.assertFalse(record["expectation"]["matched"])
        self.assertFalse(gate["passed"])
        self.assertFalse(gate["checks"]["all_mapped_expectations_have_test_authority"])
        self.assertEqual(gate["passed_expected_rejections"], [])
        self.assertEqual(gate["unproven_expected_rejection_inputs"], ["case.mimi"])

    def test_strict_gate_rechecks_execution_proof_instead_of_trusting_booleans(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _source, record, coverage, _bound = self._valid_expected_rejection_record(root)
            record["expectation"]["test_execution_valid"] = True
            record["expectation"]["test_execution_proof"] = {"status": "passed"}
            gate = StrictGateTests._gate(
                ["case.mimi"], [record], expectation_contract=coverage
            )

        self.assertEqual(gate["passed_expected_rejections"], [])
        self.assertEqual(gate["unproven_expected_rejection_inputs"], ["case.mimi"])
        self.assertFalse(gate["checks"]["all_expected_rejections_are_proven"])
        self.assertFalse(gate["passed"])

    def test_expected_rejection_authority_requires_same_source_build_and_diagnostic(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "tests/fixtures/case.mimi"
            source_path.parent.mkdir(parents=True)
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(
                root,
                "tests/fixtures/case.mimi",
                source["sha256"],
                expectation_class="expected_build_rejection",
                expected_exit_code=1,
            )
            assertion_path = root / entry["authority"]["assertion"]["path"]
            valid_text = assertion_path.read_text(encoding="utf-8")
            bound, coverage = self._load_and_bind(
                root, {"tests/fixtures/case.mimi": entry}, [source]
            )
            valid_authority = bound["tests/fixtures/case.mimi"]["test_contract_valid"]

            invalid_texts = {
                "different_source": valid_text.replace(
                    '.join("case.mimi")', '.join("other.mimi")'
                ),
                "different_command": valid_text.replace('.arg("build")', '.arg("run")'),
                "explicit_mir_route": valid_text.replace(
                    '.arg("build")\n        .arg(&fixture)',
                    '.arg("build")\n        .arg("--mir")\n        .arg(&fixture)',
                ),
                "unasserted_code": valid_text.replace(
                    'stderr.contains("E0432")', 'stderr.contains("E0423")'
                ),
                "unasserted_message": valid_text.replace(
                    'stderr.contains("expected negative diagnostic")', "true"
                ),
                "unasserted_route_reason": valid_text.replace(
                    'stderr.contains("canonical MIR build rejected: expected diagnostic")',
                    "true",
                ),
                "unexecuted_command": valid_text.replace(".output()", ".status()"),
                "unqualified_command": valid_text.replace(
                    "::std::process::Command", "Command"
                ).replace(
                    '::std::env!("CARGO_BIN_EXE_mimi")', "mimi_bin()"
                ),
                "unqualified_assert": valid_text.replace("::std::assert!", "assert!"),
                "unqualified_utf8_conversion": valid_text.replace(
                    "::std::string::String::from_utf8_lossy",
                    "String::from_utf8_lossy",
                ),
                "ignored_test": valid_text.replace(
                    "#[test]\n", "#[test]\n#[ignore]\n"
                ),
                "should_panic_test": valid_text.replace(
                    "#[test]\n", "#[test]\n#[should_panic]\n"
                ),
            }
            invalid_authorities = {}
            for label, text in invalid_texts.items():
                with self.subTest(authority=label):
                    assertion_path.write_text(text, encoding="utf-8")
                    entry["authority"]["assertion"]["sha256"] = census.sha256_file(
                        assertion_path
                    )
                    rebound, rebound_coverage = self._load_and_bind(
                        root, {"tests/fixtures/case.mimi": entry}, [source]
                    )
                    invalid_authorities[label] = rebound[
                        "tests/fixtures/case.mimi"
                    ]["authority_valid"]
                    self.assertEqual(
                        rebound_coverage["invalid_authority_paths"],
                        ["tests/fixtures/case.mimi"],
                    )

            assertion_path.write_text(valid_text, encoding="utf-8")
            entry["authority"]["assertion"]["sha256"] = census.sha256_file(
                assertion_path
            )
            entry["authority"]["assertion"]["line"] = 12
            rebound, rebound_coverage = self._load_and_bind(
                root, {"tests/fixtures/case.mimi": entry}, [source]
            )
            invalid_authorities["arbitrary_assertion_line"] = rebound[
                "tests/fixtures/case.mimi"
            ]["authority_valid"]
            self.assertEqual(
                rebound_coverage["invalid_authority_paths"],
                ["tests/fixtures/case.mimi"],
            )

        self.assertTrue(coverage["exact_bijection"])
        self.assertTrue(valid_authority)
        self.assertTrue(all(value is False for value in invalid_authorities.values()))

    def test_expected_rejection_qualified_std_contract_survives_shadowing(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_build_rejection",
                expected_exit_code=1,
            )
            authority = root / entry["authority"]["assertion"]["path"]
            valid = authority.read_text(encoding="utf-8")
            shadowing = (
                "macro_rules! assert { ($($arg:tt)*) => {{}} }\n"
                "struct Command;\n"
                "struct String;\n"
            )
            authority.write_text(shadowing + valid, encoding="utf-8")
            entry["authority"]["assertion"]["line"] += shadowing.count("\n")
            entry["authority"]["assertion"]["sha256"] = census.sha256_file(authority)
            bound, _coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])

        self.assertTrue(bound["case.mimi"]["test_contract_valid"])
        self.assertTrue(bound["case.mimi"]["source_authority_valid"])

    def test_expected_rejection_ignores_comment_and_string_decoys_and_rejects_nested_code(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_build_rejection",
                expected_exit_code=1,
            )
            authority = root / entry["authority"]["assertion"]["path"]
            valid = authority.read_text(encoding="utf-8")
            start, end = census._rust_test_function_span(valid, "test_route_contract")
            body_open = valid.find("{", start, end)
            body = valid[body_open + 1 : end - 1]
            escaped_body = json.dumps(body)
            decoys = {
                "block_comment": f"/*{body}*/",
                "normal_string": f"let _decoy = {escaped_body};",
                "raw_string": f'let _decoy = r##"{body}"##;',
                "if_false": f"if false {{{body}}}",
                "closure": f"let _deferred = || {{{body}}};",
                "macro": f"macro_rules! decoy {{ () => {{{body}}} }}\ndecoy!();",
            }
            observed: dict[str, bool] = {}
            for label, decoy in decoys.items():
                with self.subTest(decoy=label):
                    text = (
                        "#[test]\nfn test_route_contract() {\n"
                        + decoy
                        + "\n}\n"
                    )
                    authority.write_text(text, encoding="utf-8")
                    entry["authority"]["assertion"]["sha256"] = census.sha256_file(authority)
                    bound, coverage = self._load_and_bind(
                        root, {"case.mimi": entry}, [source]
                    )
                    observed[label] = bound["case.mimi"]["test_contract_valid"]
                    self.assertEqual(
                        coverage["invalid_authority_paths"], ["case.mimi"]
                    )
            authority.write_text(valid, encoding="utf-8")

        self.assertTrue(all(value is False for value in observed.values()))

    def test_expected_rejection_requires_cargo_target_and_run_proof(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_build_rejection",
                expected_exit_code=1,
            )
            bound, coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            non_target = root / "tests/not_the_authority.rs"
            non_target.write_text("#[test]\nfn elsewhere() {}\n", encoding="utf-8")
            wrong_target_result = self._execute_authority_tests(
                root, [source], bound, target_source=non_target
            )
            self.assertEqual(wrong_target_result["status"], "failed")
            self.assertFalse(bound["case.mimi"]["cargo_target_valid"])
            self.assertFalse(bound["case.mimi"]["test_execution_valid"])

            bound, coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            wrong_kind_result = self._execute_authority_tests(
                root, [source], bound, target_kind="bin"
            )
            self.assertEqual(wrong_kind_result["status"], "failed")
            self.assertFalse(bound["case.mimi"]["cargo_target_valid"])
            self.assertFalse(bound["case.mimi"]["authority_valid"])

            bound, coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            not_run_stdout = (
                b"running 0 tests\n\n"
                b"test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; "
                b"1 filtered out; finished in 0.00s\n"
            )
            execution = self._execute_authority_tests(
                root, [source], bound, run_stdout=not_run_stdout
            )
            result = census._expectation_result(
                "case.mimi",
                1,
                EXPECTED_REJECTION_ROUTE,
                bound,
                run_status="completed",
                failure_classification={"class": "capability", "status": "classified"},
                captured_output=(
                    b"error[E0432]: expected negative diagnostic\n"
                    b"canonical MIR build rejected: expected diagnostic\n"
                ),
            )

        self.assertEqual(execution["status"], "failed")
        self.assertFalse(bound["case.mimi"]["test_execution_valid"])
        self.assertFalse(result["matched"])
        self.assertEqual(coverage["exact_bijection"], True)

    def test_10752_unmapped_rejections_remain_full_denominator_blockers(self) -> None:
        count = 10_752
        paths = list(census.ACTOR_FALLBACK_INPUTS) + [
            f"unknown/rejection-{index:05d}.mimi"
            for index in range(count - len(census.ACTOR_FALLBACK_INPUTS))
        ]
        records = []
        for path in paths:
            record = StrictGateTests._record(path, route="rejected")
            record["exit_code"] = 1
            record["expectation"] = {
                "class": "unmapped",
                "status": "unmapped",
                "matched": False,
                "source_hash_status": "not_applicable",
                "authority_status": "not_applicable",
                "authority_valid": False,
            }
            record["failure_classification"] = {
                "class": "unknown",
                "status": "unclassified",
            }
            record["per_function"] = {
                "named_roster_complete": False,
                "has_fallback": False,
            }
            record["output_artifact"] = None
            records.append(record)

        gate = StrictGateTests._gate(paths, records)

        self.assertEqual(gate["input_count"], count)
        self.assertEqual(gate["outcome_count"], count)
        self.assertTrue(gate["checks"]["nonempty_exact_input_denominator"])
        self.assertFalse(gate["checks"]["all_outcomes_match_expectations"])
        self.assertFalse(gate["checks"]["all_routes_are_canonical_or_authorized_rejections"])
        self.assertFalse(gate["checks"]["all_builds_succeeded_or_rejected_as_declared"])
        self.assertFalse(gate["checks"]["no_unclassified_failure_causes"])
        self.assertEqual(len(gate["noncanonical_route_inputs"]), count)
        self.assertEqual(len(gate["not_successful_inputs"]), count)
        self.assertEqual(gate["passed_expected_rejections"], [])
        self.assertEqual(len(gate["unexpected_failure_inputs"]), count)
        self.assertFalse(gate["passed"])

    def test_expected_rejection_with_wrong_diagnostic_does_not_match(self) -> None:
        expectation = {
            "case.mimi": {
                "class": "expected_build_rejection",
                "reason": "negative fixture",
                "explicit": True,
                "expected_exit_code": 1,
                "expected_diagnostic_codes": ["E0432"],
                "required_output_substrings": ["expected negative diagnostic"],
                "expected_route_disposition": "rejected",
                "expected_route_classification_status": "observed",
                "expected_route_reason": "expected diagnostic",
                "expected_failure_class": "capability",
                "source_hash_status": "matched",
                "authority_valid": True,
                "test_execution_valid": True,
            }
        }
        result = census._expectation_result(
            "case.mimi",
            1,
            EXPECTED_REJECTION_ROUTE,
            expectation,
            run_status="completed",
            failure_classification={"class": "capability", "status": "classified"},
            captured_output=(
                b"error[E0423]: expected negative diagnostic\n"
                b"canonical MIR build rejected: expected diagnostic\n"
            ),
        )

        self.assertFalse(result["matched"])
        self.assertFalse(result["diagnostic_contract"]["diagnostic_codes_match_exactly"])
        self.assertEqual(result["actual_diagnostic_codes"], ["E0423"])

    def test_expected_rejection_ignores_incidental_code_tokens_outside_errors(self) -> None:
        expectation = {
            "case.mimi": {
                "class": "expected_build_rejection",
                "reason": "negative fixture",
                "explicit": True,
                "expected_exit_code": 1,
                "expected_diagnostic_codes": ["E0432"],
                "required_output_substrings": ["expected negative diagnostic"],
                "expected_route_disposition": "rejected",
                "expected_route_classification_status": "observed",
                "expected_route_reason": "expected diagnostic",
                "expected_failure_class": "capability",
                "source_hash_status": "matched",
                "authority_valid": True,
                "test_execution_valid": True,
            }
        }
        result = census._expectation_result(
            "case.mimi",
            1,
            {"disposition": "rejected", "classification_status": "observed", "reason": "generic capability gate"},
            expectation,
            run_status="completed",
            failure_classification={"class": "capability", "status": "classified"},
            captured_output=(
                b"canonical MIR build rejected: generic capability gate\n"
                b"info: E0432 expected negative diagnostic\n"
            ),
        )

        self.assertEqual(result["actual_diagnostic_codes"], [])
        self.assertFalse(result["diagnostic_contract"]["diagnostic_codes_match_exactly"])
        self.assertFalse(result["matched"])

    def test_expected_rejection_that_creates_output_artifact_does_not_match(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_build_rejection",
                expected_exit_code=1,
            )
            bound, _coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            scratch = root / "scratch"
            scratch.mkdir()
            logs = root / "logs"
            logs.mkdir()

            def fake_run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[bytes]:
                Path(command[-1]).write_bytes(b"rejected-build-output")
                return subprocess.CompletedProcess(
                    command,
                    1,
                    stdout=b"",
                    stderr=(
                        b"error[E0432]: expected negative diagnostic\n"
                        b"canonical MIR build rejected: expected diagnostic\n"
                    ),
                )

            with patch.object(census.subprocess, "run", side_effect=fake_run):
                record = census._run_one(
                    root=root,
                    source=source,
                    index=1,
                    mimi=root / "mimi",
                    timeout=5,
                    scratch_root=scratch,
                    logs_root=logs,
                    expected=bound,
                )

        self.assertEqual(record["route"]["disposition"], "rejected")
        self.assertIsNotNone(record["output_artifact"])
        self.assertTrue(record["expectation"]["diagnostic_contract"]["diagnostic_codes_match_exactly"])
        self.assertFalse(record["expectation"]["diagnostic_contract"]["output_artifact_absent"])
        self.assertFalse(record["expectation"]["matched"])

    def test_expected_rejection_directory_at_output_path_does_not_match(self) -> None:
        record = self._run_expected_rejection_with_path_artifact("directory")

        self.assertEqual(record["output_artifact"]["status"], "non_regular_path")
        self.assertEqual(record["output_artifact"]["kind"], "directory")
        self.assertFalse(record["expectation"]["diagnostic_contract"]["output_artifact_absent"])
        self.assertFalse(record["expectation"]["matched"])

    def test_expected_rejection_dangling_symlink_at_output_path_does_not_match(self) -> None:
        record = self._run_expected_rejection_with_path_artifact("dangling_symlink")

        self.assertEqual(record["output_artifact"]["status"], "non_regular_path")
        self.assertEqual(record["output_artifact"]["kind"], "symlink")
        self.assertFalse(record["expectation"]["diagnostic_contract"]["output_artifact_absent"])
        self.assertFalse(record["expectation"]["matched"])

    def test_interpreter_only_valid_mapping_skips_build_and_still_fails_strict_gate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "interpreter.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(
                root,
                "interpreter.mimi",
                source["sha256"],
                expectation_class="interpreter_only",
            )
            bound, coverage = self._load_and_bind(root, {"interpreter.mimi": entry}, [source])
            scratch = root / "scratch"
            scratch.mkdir()
            logs = root / "logs"
            logs.mkdir()

            with patch.object(census.subprocess, "run") as run_mock:
                record = census._run_one(
                    root=root,
                    source=source,
                    index=1,
                    mimi=root / "mimi",
                    timeout=5,
                    scratch_root=scratch,
                    logs_root=logs,
                    expected=bound,
                )
            gate = StrictGateTests._gate(
                ["interpreter.mimi"],
                [record],
                expectation_contract=coverage,
            )

        run_mock.assert_not_called()
        self.assertEqual(record["run_status"], "not_run")
        self.assertIsNone(record["command"])
        self.assertIsNone(record["output_artifact"])
        self.assertIsNone(record["exit_code"])
        self.assertEqual(record["build_not_run_reason"], "explicit fixture contract")
        self.assertTrue(record["expectation"]["matched"])
        self.assertEqual(record["expectation"]["authority_status"], "valid")
        self.assertEqual(record["expectation"]["source_hash_status"], "matched")
        self.assertTrue(record["source_stable_for_input"])
        self.assertEqual(record["route"]["disposition"], "unobserved")
        self.assertFalse(gate["checks"]["all_builds_succeeded_or_rejected_as_declared"])
        self.assertFalse(gate["checks"]["all_routes_are_canonical_or_authorized_rejections"])
        self.assertFalse(gate["passed"])

    def test_known_gap_interpreter_only_and_unmapped_never_count_as_canonical_success(self) -> None:
        records = []
        paths = ["known-gap.mimi", "interpreter.mimi", "unmapped.mimi"]
        classes = ["known_gap", "interpreter_only", "unmapped"]
        for path, expectation_class in zip(paths, classes):
            record = StrictGateTests._record(path, route="canonical")
            record["expectation"] = {
                "class": expectation_class,
                "status": "known_gap_unresolved" if expectation_class == "known_gap" else "build_was_run" if expectation_class == "interpreter_only" else "unmapped",
                "matched": False,
                "source_hash_status": "matched",
                "authority_status": "valid" if expectation_class != "unmapped" else "not_applicable",
                "authority_valid": expectation_class != "unmapped",
            }
            records.append(record)
        gate = StrictGateTests._gate(paths, records)

        self.assertTrue(set(paths).isdisjoint(gate["canonical_success_inputs"]))
        self.assertFalse(gate["checks"]["all_outcomes_match_expectations"])
        self.assertFalse(gate["passed"])


class RawCaptureTests(unittest.TestCase):
    def test_run_one_preserves_complete_text_and_exact_raw_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            scratch = root / "scratch"
            scratch.mkdir()
            logs = root / "logs"
            logs.mkdir()
            stdout = b"complete stdout\n"
            stderr = (
                b"canonical route disposition: canonical (default-route) mir_digest="
                + b"b" * 64
                + b"\ninfo: canonical MIR native emitted owner='function:main'\n"
                + b"info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest="
                + b"b" * 64
                + b"\n"
            )
            source = census.source_record(source_path, root)

            def fake_run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[bytes]:
                env = kwargs["env"]
                assert isinstance(env, dict)
                Path(command[-1]).write_bytes(b"binary")
                return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr=stderr)

            with patch.object(census.subprocess, "run", side_effect=fake_run):
                record = census._run_one(
                    root=root,
                    source=source,
                    index=1,
                    mimi=root / "mimi",
                    timeout=5,
                    scratch_root=scratch,
                    logs_root=logs,
                    expected={},
                )

            self.assertEqual(record["stdout"], "complete stdout\n")
            self.assertIn("canonical MIR native compiled", record["stderr"])
            self.assertEqual((logs / "000001.stdout.bin").read_bytes(), stdout)
            self.assertEqual((logs / "000001.stderr.bin").read_bytes(), stderr)
            self.assertEqual(record["route"]["disposition"], "canonical")
            self.assertEqual(record["expectation"]["class"], "unmapped")
            self.assertFalse(record["expectation"]["matched"])
            self.assertTrue(record["per_function"]["named_roster_complete"])
            self.assertEqual(record["scratch_cleanup"]["status"], "removed")
            self.assertFalse((scratch / "input-000001").exists())

    def test_explicit_expected_rejection_must_match_exit_and_route(self) -> None:
        expectation = {
            "case.mimi": {
                "class": "expected_build_rejection",
                "reason": "negative test",
                "explicit": True,
                "expected_exit_code": 1,
                "source_sha256": "a" * 64,
                "source_hash_status": "matched",
                "source_hash_matches": True,
                "authority_status": "valid",
                "authority_valid": True,
                "test_execution_valid": True,
                "test_execution_proof": {"status": "passed"},
                "expected_diagnostic_codes": ["E0432"],
                "required_output_substrings": ["expected negative diagnostic"],
                "expected_route_disposition": "rejected",
                "expected_route_classification_status": "observed",
                "expected_route_reason": "expected diagnostic",
                "expected_failure_class": "capability",
            }
        }
        route = EXPECTED_REJECTION_ROUTE
        result = census._expectation_result(
            "case.mimi",
            1,
            route,
            expectation,
            run_status="completed",
            failure_classification={"class": "capability", "status": "classified"},
            captured_output=(
                b"error[E0432]: expected negative diagnostic\n"
                b"canonical MIR build rejected: expected diagnostic\n"
            ),
        )
        self.assertTrue(result["matched"])
        self.assertEqual(result["class"], "expected_build_rejection")
        self.assertEqual(result["status"], "matched")
        mismatch = census._expectation_result(
            "case.mimi",
            0,
            route,
            expectation,
            run_status="completed",
            failure_classification={"class": "capability", "status": "classified"},
            captured_output=(
                b"error[E0432]: expected negative diagnostic\n"
                b"canonical MIR build rejected: expected diagnostic\n"
            ),
        )
        self.assertFalse(mismatch["matched"])


class CollectionPathTests(unittest.TestCase):
    def test_inventory_only_never_invokes_cargo_rustc_or_mimi(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "source.mimi").write_text("func main() { }\n", encoding="utf-8")
            mimi = root / "mimi"
            mimi.write_text("#!/bin/sh\necho mimi must not run\n", encoding="utf-8")
            mimi.chmod(0o755)
            output = root / "inventory.json"
            args = census.parse_args(
                [
                    "--root", str(root),
                    "--mimi", str(mimi),
                    "--output", str(output),
                    "--inventory-only",
                ]
            )
            real_run = subprocess.run

            def reject_tool_invocation(command: object, **kwargs: object) -> subprocess.CompletedProcess[bytes]:
                if isinstance(command, (list, tuple)) and command:
                    executable_name = Path(str(command[0])).name.lower()
                    if executable_name in {"cargo", "rustc", "mimi"}:
                        raise AssertionError(
                            f"inventory-only invoked forbidden tool: {command!r}"
                        )
                return real_run(command, **kwargs)

            with patch.object(
                census,
                "_run_actor_authority_tests",
                side_effect=AssertionError("inventory-only must not invoke Cargo tests"),
            ) as run_actor_tests:
                with patch.object(
                    census.subprocess, "run", side_effect=reject_tool_invocation
                ) as run:
                    status = census.collect(args)
                collection = json.loads(output.read_text(encoding="utf-8"))
            collection_mode = collection["collection_mode"]

        self.assertEqual(status, 0)
        self.assertEqual(run_actor_tests.call_count, 0)
        self.assertEqual(collection_mode, "inventory_only")
        self.assertTrue(all(
            Path(str(call.args[0][0])).name.lower() not in {"cargo", "rustc", "mimi"}
            for call in run.call_args_list
        ))
        self.assertEqual(collection["toolchain"]["mimi"]["version_probe"], "not_run_inventory_only")
        self.assertEqual(collection["toolchain"]["cargo"]["version"]["status"], "not_run_inventory_only")
        self.assertEqual(collection["toolchain"]["rustc"]["version"]["status"], "not_run_inventory_only")
        self.assertEqual(
            collection["cargo_test_toolchain_authority"]["rustup_toolchain"]["policy"]["issues"],
            ["rustup_toolchain_not_probed_inventory_only"],
        )

    def test_report_path_under_root_cannot_end_in_mimi(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "source.mimi").write_text("func main() { }\n", encoding="utf-8")
            output = root / "report.mimi"
            args = census.parse_args(
                [
                    "--root",
                    str(root),
                    "--mimi",
                    str(root / "mimi"),
                    "--output",
                    str(output),
                    "--inventory-only",
                ]
            )

            with self.assertRaisesRegex(ValueError, "over an in-scope Mimi path"):
                census.collect(args)

            self.assertFalse(output.exists())
            self.assertEqual(
                (root / "source.mimi").read_text(encoding="utf-8"),
                "func main() { }\n",
            )

    def test_in_root_mimi_report_is_rejected_before_full_census_starts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "new-report.mimi"
            args = census.parse_args(
                [
                    "--root",
                    str(root),
                    "--mimi",
                    str(root / "mimi"),
                    "--output",
                    str(output),
                ]
            )

            with patch.object(
                census, "tracked_mimi_paths", side_effect=AssertionError("inventory started")
            ) as inventory_mock:
                with self.assertRaisesRegex(ValueError, "over an in-scope Mimi path"):
                    census.collect(args)

            inventory_mock.assert_not_called()
            self.assertFalse(output.exists())

    def test_outside_symlink_alias_cannot_write_mimi_report_inside_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as outside:
            root = Path(temporary)
            target = root / "new-report.mimi"
            alias = Path(outside) / "report-alias"
            alias.symlink_to(target)
            args = census.parse_args(
                [
                    "--root",
                    str(root),
                    "--mimi",
                    str(root / "mimi"),
                    "--output",
                    str(alias),
                    "--inventory-only",
                ]
            )

            with self.assertRaisesRegex(ValueError, "over an in-scope Mimi path"):
                census.collect(args)

            self.assertTrue(alias.is_symlink())
            self.assertFalse(target.exists())

    def test_outside_symlinked_parent_cannot_write_mimi_report_inside_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as outside:
            root = Path(temporary)
            outside_root = Path(outside)
            alias_parent = outside_root / "root-alias"
            alias_parent.symlink_to(root, target_is_directory=True)
            target = root / "new-report.mimi"
            args = census.parse_args(
                [
                    "--root",
                    str(root),
                    "--mimi",
                    str(root / "mimi"),
                    "--output",
                    str(alias_parent / target.name),
                    "--inventory-only",
                ]
            )

            with self.assertRaisesRegex(ValueError, "over an in-scope Mimi path"):
                census.collect(args)

            self.assertTrue(alias_parent.is_symlink())
            self.assertFalse(target.exists())

    def test_report_destination_is_revalidated_before_inventory_write(self) -> None:
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as outside:
            root = Path(temporary)
            outside_root = Path(outside)
            original_target = outside_root / "report.json"
            alias = outside_root / "report-alias"
            alias.symlink_to(original_target)
            target = root / "new-report.mimi"
            args = census.parse_args(
                [
                    "--root",
                    str(root),
                    "--mimi",
                    str(root / "mimi"),
                    "--output",
                    str(alias),
                    "--inventory-only",
                ]
            )
            original_snapshot = census.source_snapshot

            def retarget_after_snapshot(
                snapshot_root: Path, sources: list[dict]
            ) -> dict:
                snapshot = original_snapshot(snapshot_root, sources)
                alias.unlink()
                alias.symlink_to(target)
                return snapshot

            with patch.object(census, "source_snapshot", side_effect=retarget_after_snapshot):
                with self.assertRaisesRegex(ValueError, "changed during collection"):
                    census.collect(args)

            self.assertTrue(alias.is_symlink())
            self.assertEqual(alias.resolve(), target)
            self.assertFalse(target.exists())
            self.assertFalse(original_target.exists())


class BoundedParallelExecutionTests(unittest.TestCase):
    def test_out_of_order_workers_commit_journal_and_progress_in_manifest_order(self) -> None:
        sources = [{"path": f"input-{index}.mimi"} for index in range(1, 5)]
        second_finished = threading.Event()
        completion_order: list[int] = []
        committed_order: list[int] = []

        def worker(index: int, source: dict) -> dict:
            if index == 1:
                if not second_finished.wait(timeout=5):
                    raise RuntimeError("second input did not finish")
            completion_order.append(index)
            if index == 2:
                second_finished.set()
            return {"path": source["path"], "input_index": index}

        with tempfile.TemporaryDirectory() as temporary:
            journal = Path(temporary) / "journal.jsonl"
            journal.write_bytes(b"")

            def commit(index: int, source: dict, record: dict) -> None:
                census._write_journal_line(journal, record)
                committed_order.append(index)

            records, had_worker_error = census._run_bounded_parallel_ordered(
                sources,
                jobs=2,
                worker=worker,
                on_worker_error=lambda index, source, error: {
                    "path": source["path"],
                    "input_index": index,
                    "run_status": "worker_error",
                },
                on_commit=commit,
            )
            journal_records = [
                json.loads(line) for line in journal.read_text(encoding="utf-8").splitlines()
            ]

        self.assertFalse(had_worker_error)
        self.assertEqual(completion_order[:2], [2, 1])
        self.assertEqual(committed_order, [1, 2, 3, 4])
        self.assertEqual([row["input_index"] for row in records], [1, 2, 3, 4])
        self.assertEqual([row["input_index"] for row in journal_records], [1, 2, 3, 4])

    def test_worker_exception_is_journaled_as_failure_without_final_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src").mkdir()
            (root / "src/a.mimi").write_text("func main() { }\n", encoding="utf-8")
            (root / "src/b.mimi").write_text("func main() { }\n", encoding="utf-8")
            output = root / "report.json"
            args = census.parse_args(
                [
                    "--root",
                    str(root),
                    "--mimi",
                    str(root / "mimi"),
                    "--output",
                    str(output),
                    "--jobs",
                    "2",
                ]
            )
            with (
                patch.object(census, "toolchain_manifest", return_value={}),
                patch.object(census, "_collect_legacy_owner_audit", return_value={}),
                patch.object(census, "_run_one", side_effect=RuntimeError("simulated worker failure")),
            ):
                status = census.collect(args)

            artifacts = Path(str(output) + ".artifacts")
            journal_records = [
                json.loads(line)
                for line in (artifacts / "journal.jsonl").read_text(encoding="utf-8").splitlines()
            ]

        self.assertEqual(status, 2)
        self.assertFalse(output.exists())
        self.assertEqual([row["path"] for row in journal_records], ["src/a.mimi", "src/b.mimi"])
        self.assertEqual([row["input_index"] for row in journal_records], [1, 2])
        for row in journal_records:
            self.assertEqual(row["run_status"], "worker_error")
            self.assertEqual(row["failure_classification"]["class"], "unknown")
            self.assertEqual(row["failure_classification"]["status"], "unclassified")
            self.assertNotEqual(row["route"]["disposition"], "canonical")
            self.assertFalse(row["per_function"]["named_roster_complete"])
            self.assertEqual(row["worker_exception"]["type"], "RuntimeError")

    def test_execution_contract_records_effective_worker_count(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src").mkdir()
            (root / "src/a.mimi").write_text("func main() { }\n", encoding="utf-8")
            output = root / "report.json"
            args = census.parse_args(
                [
                    "--root",
                    str(root),
                    "--mimi",
                    str(root / "mimi"),
                    "--output",
                    str(output),
                    "--jobs",
                    "4",
                ]
            )

            def fake_run_one(**kwargs: object) -> dict:
                index = kwargs["index"]
                assert isinstance(index, int)
                record = StrictGateTests._record("src/a.mimi")
                record["input_index"] = index
                record["expectation"]["status"] = "matched"
                return record

            with (
                patch.object(census, "toolchain_manifest", return_value={}),
                patch.object(census, "_collect_legacy_owner_audit", return_value={}),
                patch.object(census, "_run_one", side_effect=fake_run_one),
            ):
                status = census.collect(args)
            report = json.loads(output.read_text(encoding="utf-8"))

        self.assertEqual(status, 0)
        self.assertEqual(report["execution_contract"]["requested_worker_count"], 4)
        self.assertEqual(report["execution_contract"]["worker_count"], 1)
        self.assertEqual(report["execution_contract"]["parallelism"], 1)
        self.assertEqual(report["records"][0]["input_index"], 1)

    def test_jobs_argument_is_bounded_and_defaults_to_four(self) -> None:
        base = ["--output", "report.json"]
        self.assertEqual(census.parse_args(base).jobs, 4)
        self.assertEqual(census.parse_args(base + ["--jobs", "8"]).jobs, 8)
        with self.assertRaises(SystemExit):
            census.parse_args(base + ["--jobs", "9"])


if __name__ == "__main__":
    unittest.main()
