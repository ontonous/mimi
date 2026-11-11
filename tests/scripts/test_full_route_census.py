from __future__ import annotations

import base64
import inspect
import json
import os
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
    identity_span = census._rust_named_function_span(
        source, census.ACTOR_IDENTITY_HELPER_NAME
    )
    if span is None or identity_span is None:
        raise AssertionError("repository Actor helpers are not top-level functions")
    return source[identity_span[0] : identity_span[1]] + "\n" + source[span[0] : span[1]]


EXPECTED_REJECTION_ROUTE = {
    "disposition": "rejected",
    "classification_status": "observed",
    "reason": "expected diagnostic",
}


def _write_test_mimi(root: Path) -> tuple[Path, str]:
    """Create the canonical executable fixture required by _run_one's identity gate."""
    mimi = root / "mimi"
    if not mimi.exists():
        mimi.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        mimi.chmod(0o755)
    mimi = mimi.resolve(strict=True)
    return mimi, census.sha256_file(mimi)


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

    def test_generated_excluded_directory_symlink_is_a_scope_limitation_not_a_blocker(self) -> None:
        with tempfile.TemporaryDirectory() as temporary, tempfile.TemporaryDirectory() as outside:
            root = Path(temporary)
            external = Path(outside)
            (root / "tests").mkdir()
            (root / "tests/active.mimi").write_text(
                "func main() { println(1) }\n", encoding="utf-8"
            )
            (external / "generated.mimi").write_text(
                "func main() { println(2) }\n", encoding="utf-8"
            )
            (root / "target").symlink_to(external, target_is_directory=True)

            sources, _scope_exclusions, scope = census.enumerate_source_scope(root, [])
            manifest_sha256 = census._manifest_digest(sources)
            symlinks = census.source_symlink_exclusions(root)
            gate = StrictGateTests._gate(
                ["tests/active.mimi"],
                [StrictGateTests._record("tests/active.mimi")],
                excluded_symlinks=tuple(symlinks),
                source_scope=scope,
            )
            (root / "target").unlink()
            sources_without_target, _exclusions, scope_without_target = (
                census.enumerate_source_scope(root, [])
            )
            manifest_sha256_without_target = census._manifest_digest(
                sources_without_target
            )

        self.assertEqual([source["path"] for source in sources], ["tests/active.mimi"])
        self.assertEqual(scope["candidate_count"], 1)
        self.assertTrue(scope["exactly_once"])
        self.assertEqual(
            symlinks[0]["kind"], census.SCOPE_LIMITED_DIRECTORY_SYMLINK_KIND
        )
        self.assertTrue(gate["checks"]["no_unfollowed_mimi_symlinks"])
        self.assertEqual(gate["blocking_mimi_symlink_paths"], [])
        self.assertEqual(gate["symlink_scope_limitations"][0]["path"], "target")
        self.assertEqual(
            manifest_sha256_without_target, manifest_sha256
        )
        self.assertEqual(scope_without_target["candidate_count"], scope["candidate_count"])

    def test_active_mimi_file_symlink_still_blocks_the_gate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "tests").mkdir()
            source = root / "tests/active.mimi"
            source.write_text("func main() { println(1) }\n", encoding="utf-8")
            alias = root / "tests/alias.mimi"
            try:
                alias.symlink_to(source.name)
            except OSError as error:
                self.skipTest(f"symlinks unavailable: {error}")

            sources, _scope_exclusions, scope = census.enumerate_source_scope(root, [])
            symlinks = census.source_symlink_exclusions(root)
            gate = StrictGateTests._gate(
                [source["path"] for source in sources],
                [StrictGateTests._record(source["path"]) for source in sources],
                excluded_symlinks=tuple(symlinks),
                source_scope=scope,
            )

        self.assertEqual(
            {source["path"] for source in sources},
            {"tests/active.mimi", "tests/alias.mimi"},
        )
        self.assertFalse(gate["checks"]["no_unfollowed_mimi_symlinks"])
        self.assertEqual(gate["blocking_mimi_symlink_paths"], ["tests/alias.mimi"])

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
            ("target", census.SCOPE_LIMITED_DIRECTORY_SYMLINK_KIND),
            {(item["path"], item["kind"]) for item in symlink_exclusions},
        )
        self.assertEqual(blockers[0]["kind"], "tracked_mimi_path_parent_symlink_not_followed")
        self.assertFalse(
            gate["checks"]["no_tracked_mimi_paths_through_symlinked_parents"]
        )
        self.assertFalse(gate["checks"]["no_unfollowed_mimi_symlinks"])
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
    def test_actor_authority_census_mode_fails_closed_without_identity(self) -> None:
        authority_path = census.ROOT / census.ACTOR_HELPER_TEST_PATH
        source = authority_path.read_text(encoding="utf-8")
        identity_span = census._rust_named_function_span(
            source, census.ACTOR_IDENTITY_HELPER_NAME
        )
        self.assertIsNotNone(identity_span)
        helper = source[identity_span[0] : identity_span[1]]
        self.assertIn('MIMI_ROUTE_CENSUS_MIMI")', helper)
        self.assertIn('MIMI_ROUTE_CENSUS_MIMI_SHA256")', helper)
        self.assertIn('census_mode.as_deref() != Some("1")', helper)
        self.assertIn('Some("1")', helper)
        self.assertIn("canonical and symlink-free", helper)
        self.assertIn("CARGO_BIN_EXE_mimi", helper)
        contracts = census.actor_fixture_test_contracts(census.ROOT)
        self.assertTrue(all(item["helper_valid"] for item in contracts.values()))

    def _temporary_actor_project(self, root: Path) -> tuple[list[dict], dict, tuple[str, str]]:
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
        mimi = root / "bin" / "mimi"
        mimi.parent.mkdir()
        mimi.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        mimi.chmod(0o755)
        sources = [census.source_record(root / path, root) for path in census.ACTOR_FALLBACK_INPUTS]
        return sources, census.actor_fixture_test_contracts(root), (
            str(mimi), census.sha256_file(mimi)
        )

    @staticmethod
    def _successful_test_output(test_id: str, mimi_identity: tuple[str, str]) -> bytes:
        marker = census._authority_mimi_identity_marker(*mimi_identity)
        assert marker is not None
        return (
            f"{marker}\n"
            f"running 1 test\ntest {test_id} ... ok\n\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
            "0 filtered out; finished in 0.00s\n"
        ).encode()

    def test_actor_runner_executes_exact_tests_and_retains_hash_bound_bin_logs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources, contracts, mimi_identity = self._temporary_actor_project(root)
            logs = root / "artifacts/logs"
            logs.mkdir(parents=True)
            completed = [
                subprocess.CompletedProcess(
                    ["cargo", "test"],
                    0,
                    self._successful_test_output(
                        census.ACTOR_AUTHORITY_TEST_IDS[path], mimi_identity
                    ),
                    b"",
                )
                for path in census.ACTOR_FALLBACK_INPUTS
            ]
            cargo_authority = census.cargo_test_toolchain_authority(root)

            with patch.object(census.subprocess, "run", side_effect=completed) as run:
                execution = census._run_actor_authority_tests(
                    root,
                    logs,
                    sources,
                    contracts,
                    30.0,
                    cargo_authority,
                    mimi_path=mimi_identity[0],
                    mimi_sha256=mimi_identity[1],
                )

            self.assertEqual(run.call_count, 2)
            self.assertEqual(execution["status"], "passed")
            self.assertEqual(
                [case["command"] for case in execution["cases"]],
                [
                    [
                        cargo_authority["cargo"]["path"], "test", "--test", "real_world_cli",
                        census.ACTOR_AUTHORITY_TEST_IDS[path], "--", "--exact", "--nocapture"
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

    def test_actor_runner_rejects_successful_test_with_wrong_mimi_identity_marker(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources, contracts, mimi_identity = self._temporary_actor_project(root)
            logs = root / "artifacts/logs"
            logs.mkdir(parents=True)
            first_test_id = census.ACTOR_AUTHORITY_TEST_IDS[
                census.ACTOR_FALLBACK_INPUTS[0]
            ]
            second_test_id = census.ACTOR_AUTHORITY_TEST_IDS[
                census.ACTOR_FALLBACK_INPUTS[1]
            ]
            wrong_sha256 = "0" * 64 if mimi_identity[1] != "0" * 64 else "1" * 64
            mismatched_identity = (mimi_identity[0], wrong_sha256)
            results = [
                subprocess.CompletedProcess(
                    ["cargo", "test"],
                    0,
                    self._successful_test_output(first_test_id, mismatched_identity),
                    b"",
                ),
                subprocess.CompletedProcess(
                    ["cargo", "test"],
                    0,
                    self._successful_test_output(second_test_id, mimi_identity),
                    b"",
                ),
            ]
            cargo_authority = census.cargo_test_toolchain_authority(root)

            with patch.object(census.subprocess, "run", side_effect=results):
                execution = census._run_actor_authority_tests(
                    root,
                    logs,
                    sources,
                    contracts,
                    30.0,
                    cargo_authority,
                    mimi_path=mimi_identity[0],
                    mimi_sha256=mimi_identity[1],
                )

        self.assertEqual(execution["status"], "failed")
        self.assertFalse(execution["cases"][0]["execution_valid"])
        self.assertTrue(execution["cases"][1]["execution_valid"])

    def test_actor_execution_validator_rejects_mismatched_mimi_identity_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources, contracts, mimi_identity = self._temporary_actor_project(root)
            logs = root / "artifacts/logs"
            logs.mkdir(parents=True)
            completed = [
                subprocess.CompletedProcess(
                    ["cargo", "test"],
                    0,
                    self._successful_test_output(
                        census.ACTOR_AUTHORITY_TEST_IDS[path], mimi_identity
                    ),
                    b"",
                )
                for path in census.ACTOR_FALLBACK_INPUTS
            ]
            cargo_authority = census.cargo_test_toolchain_authority(root)

            with patch.object(census.subprocess, "run", side_effect=completed):
                execution = census._run_actor_authority_tests(
                    root,
                    logs,
                    sources,
                    contracts,
                    30.0,
                    cargo_authority,
                    mimi_path=mimi_identity[0],
                    mimi_sha256=mimi_identity[1],
                )

            valid_case = execution["cases"][0]
            source_path = census.ACTOR_FALLBACK_INPUTS[0]
            self.assertTrue(valid_case["execution_valid"])
            mismatched_case = dict(valid_case)
            mismatched_identity = dict(valid_case["mimi_executable_identity"])
            mismatched_identity["sha256_after"] = (
                "0" * 64 if mimi_identity[1] != "0" * 64 else "1" * 64
            )
            mismatched_case["mimi_executable_identity"] = mismatched_identity

            self.assertFalse(
                census._actor_test_execution_valid(
                    mismatched_case,
                    source_path,
                    sources[0],
                    contracts[source_path],
                    root=root,
                    logs_root=logs,
                    cargo_authority=cargo_authority,
                    mimi_path=mimi_identity[0],
                    mimi_sha256=mimi_identity[1],
                )
            )

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
                sources, contracts, mimi_identity = self._temporary_actor_project(root)
                logs = root / "artifacts/logs"
                logs.mkdir(parents=True)
                failed = subprocess.CompletedProcess(["cargo", "test"], returncode, stdout, b"runtime failure\n")
                passed = subprocess.CompletedProcess(
                    ["cargo", "test"],
                    0,
                    self._successful_test_output(
                        census.ACTOR_AUTHORITY_TEST_IDS[census.ACTOR_FALLBACK_INPUTS[1]],
                        mimi_identity,
                    ),
                    b"",
                )
                cargo_authority = census.cargo_test_toolchain_authority(root)
                with patch.object(census.subprocess, "run", side_effect=[failed, passed]):
                    execution = census._run_actor_authority_tests(
                        root,
                        logs,
                        sources,
                        contracts,
                        30.0,
                        cargo_authority,
                        mimi_path=mimi_identity[0],
                        mimi_sha256=mimi_identity[1],
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

    def test_route_authority_requires_matching_mimi_path_and_hash_before_and_after(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._root(root)
            mimi = root / "bin" / "mimi"
            mimi.parent.mkdir()
            mimi.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            mimi.chmod(0o755)
            mimi = mimi.resolve()
            mimi_sha = census.sha256_file(mimi)
            environment = {
                census.ROUTE_CENSUS_MIMI_MODE_ENV: "1",
                census.ROUTE_CENSUS_MIMI_PATH_ENV: str(mimi),
                census.ROUTE_CENSUS_MIMI_SHA256_ENV: mimi_sha,
            }
            toolchain = root / "toolchain"
            toolchain.mkdir()
            identities = {}
            for name in ("cargo", "rustc", "rustup"):
                executable = toolchain / name
                executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                executable.chmod(0o755)
                identities[name] = {
                    "path": str(executable),
                    "resolved_path": str(executable.resolve()),
                    "sha256": census.sha256_file(executable),
                    "native_executable": True,
                }
            authority = {
                **identities,
                "rustup_toolchain": {"policy": {"valid": True}},
                "policy": {"valid": True},
                "identity_sha256": "a" * 64,
            }
            with (
                patch.object(census, "cargo_test_toolchain_authority", return_value=authority),
                patch.object(census, "_cargo_toolchain_identity_valid", return_value=True),
            ):
                with patch.object(census.subprocess, "run") as run:
                    missing = census._run_cargo_authority_command(
                        [authority["cargo"]["path"], "test", "--no-run"],
                        root,
                        1.0,
                        authority,
                        require_mimi_identity=True,
                        expected_mimi_path=str(mimi),
                        expected_mimi_sha256=mimi_sha,
                    )
                self.assertEqual(run.call_count, 0)
                self.assertNotEqual(missing["run_status"], "completed")

                wrong_environment = {
                    **environment,
                    census.ROUTE_CENSUS_MIMI_SHA256_ENV: "f" * 64,
                }
                with patch.object(census.subprocess, "run") as run:
                    mismatched = census._run_cargo_authority_command(
                        [authority["cargo"]["path"], "test", "--no-run"],
                        root,
                        1.0,
                        authority,
                        environment_overrides=wrong_environment,
                        require_mimi_identity=True,
                        expected_mimi_path=str(mimi),
                        expected_mimi_sha256=mimi_sha,
                    )
                self.assertEqual(run.call_count, 0)
                self.assertNotEqual(mismatched["run_status"], "completed")

                alternate = root / "bin" / "alternate-mimi"
                alternate.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                alternate.chmod(0o755)
                alternate = alternate.resolve()
                wrong_path_environment = {
                    census.ROUTE_CENSUS_MIMI_MODE_ENV: "1",
                    census.ROUTE_CENSUS_MIMI_PATH_ENV: str(alternate),
                    census.ROUTE_CENSUS_MIMI_SHA256_ENV: census.sha256_file(alternate),
                }
                with patch.object(census.subprocess, "run") as run:
                    wrong_path = census._run_cargo_authority_command(
                        [authority["cargo"]["path"], "test", "--no-run"],
                        root,
                        1.0,
                        authority,
                        environment_overrides=wrong_path_environment,
                        require_mimi_identity=True,
                        expected_mimi_path=str(mimi),
                        expected_mimi_sha256=mimi_sha,
                    )
                self.assertEqual(run.call_count, 0)
                self.assertNotEqual(wrong_path["run_status"], "completed")

                def mutate_binary(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[bytes]:
                    mimi.write_text("#!/bin/sh\necho mutated\n", encoding="utf-8")
                    return subprocess.CompletedProcess(command, 0, b"", b"")

                with patch.object(census.subprocess, "run", side_effect=mutate_binary) as run:
                    mutated = census._run_cargo_authority_command(
                        [authority["cargo"]["path"], "test", "--no-run"],
                        root,
                        1.0,
                        authority,
                        environment_overrides=environment,
                        require_mimi_identity=True,
                        expected_mimi_path=str(mimi),
                        expected_mimi_sha256=mimi_sha,
                    )
                self.assertEqual(run.call_count, 1)
                self.assertEqual(mutated["run_status"], "completed")
                self.assertNotEqual(
                    mutated["environment_overrides"][census.ROUTE_CENSUS_MIMI_PATH_ENV]["sha256_before"],
                    mutated["environment_overrides"][census.ROUTE_CENSUS_MIMI_PATH_ENV]["sha256_after"],
                )
                self.assertFalse(mutated["cargo_execution"]["stable"])

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


class CargoRouteAuthorityTargetTests(unittest.TestCase):
    def _metadata_fixture(
        self,
        root: Path,
        *,
        required_features: list[str] | None = None,
        route_feature: list[str] | None = None,
        default_features: list[str] | None = None,
        source_relative: str = census.ROUTE_CENSUS_AUTHORITY_SOURCE_PATH,
        target_name: str = census.ROUTE_CENSUS_AUTHORITY_TARGET_NAME,
    ) -> dict[str, object]:
        (root / "Cargo.toml").write_text(
            '[package]\nname = "mimi"\nversion = "0.1.11-dev"\nedition = "2021"\n',
            encoding="utf-8",
        )
        source = root / census.ROUTE_CENSUS_AUTHORITY_SOURCE_PATH
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text(census.ROUTE_CENSUS_AUTHORITY_WRAPPER_SOURCE, encoding="utf-8")
        target_source = root / source_relative
        target_source.parent.mkdir(parents=True, exist_ok=True)
        if not target_source.exists():
            target_source.write_text("#[test] fn generated_authority() {}\n", encoding="utf-8")
        package_id = "path+file:///fixture/mimi#0.1.11-dev"
        features: dict[str, list[str]] = {"default": default_features or []}
        if route_feature is not None:
            features[census.ROUTE_CENSUS_AUTHORITY_FEATURE] = route_feature
        return {
            "workspace_members": [package_id],
            "packages": [
                {
                    "id": package_id,
                    "name": "mimi",
                    "manifest_path": str(root / "Cargo.toml"),
                    "features": features,
                    "targets": [
                        {
                            "name": target_name,
                            "kind": ["test"],
                            "test": True,
                            "src_path": str(target_source),
                            "required-features": required_features,
                        }
                    ],
                }
            ],
        }

    def test_exact_non_default_empty_feature_target_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            metadata = self._metadata_fixture(
                root,
                required_features=[census.ROUTE_CENSUS_AUTHORITY_FEATURE],
                route_feature=[],
                default_features=["llvm18-host-dynamic"],
            )
            target = census._cargo_test_target_for_source(
                metadata, root, census.ROUTE_CENSUS_AUTHORITY_SOURCE_PATH
            )
            commands = census._cargo_test_target_commands(
                root, "/toolchain/bin/cargo", target or {}, "census_fixture_exact"
            )

        self.assertIsNotNone(target)
        assert target is not None
        self.assertEqual(target["target_name"], "route-census-authorities")
        self.assertEqual(target["required_features"], ["route-census-authorities"])
        self.assertEqual(
            commands,
            (
                [
                    "/toolchain/bin/cargo", "test", "--manifest-path",
                    str(root / "Cargo.toml"), "--test", "route-census-authorities",
                    "--features", "route-census-authorities", "--no-run",
                ],
                [
                    "/toolchain/bin/cargo", "test", "--manifest-path",
                    str(root / "Cargo.toml"), "--test", "route-census-authorities",
                    "--features", "route-census-authorities", "census_fixture_exact",
                    "--", "--exact", "--nocapture",
                ],
            ),
        )

    def test_missing_extra_or_dependency_bearing_feature_is_rejected(self) -> None:
        cases = (
            ("missing_required_feature", [], []),
            (
                "extra_required_feature",
                [census.ROUTE_CENSUS_AUTHORITY_FEATURE, "other-feature"],
                [],
            ),
            (
                "dependency_bearing_feature",
                [census.ROUTE_CENSUS_AUTHORITY_FEATURE],
                ["dep:extra-authority-dependency"],
            ),
        )
        for name, required_features, route_feature in cases:
            with self.subTest(case=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                metadata = self._metadata_fixture(
                    root,
                    required_features=required_features,
                    route_feature=route_feature,
                )
                if name == "extra_required_feature":
                    package = metadata["packages"][0]  # type: ignore[index]
                    package["features"]["other-feature"] = []  # type: ignore[index]
                target = census._cargo_test_target_for_source(
                    metadata, root, census.ROUTE_CENSUS_AUTHORITY_SOURCE_PATH
                )
                self.assertIsNone(target)

    def test_missing_package_feature_or_default_activation_is_rejected(self) -> None:
        cases = (
            ("missing_package_feature", None, []),
            (
                "feature_enabled_by_default",
                [],
                [census.ROUTE_CENSUS_AUTHORITY_FEATURE],
            ),
        )
        for name, feature_body, default_features in cases:
            with self.subTest(case=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                metadata = self._metadata_fixture(
                    root,
                    required_features=[census.ROUTE_CENSUS_AUTHORITY_FEATURE],
                    route_feature=feature_body,
                    default_features=default_features,
                )
                target = census._cargo_test_target_for_source(
                    metadata, root, census.ROUTE_CENSUS_AUTHORITY_SOURCE_PATH
                )
                self.assertIsNone(target)

    def test_wrong_target_path_or_name_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            wrong_path = self._metadata_fixture(
                root,
                required_features=[census.ROUTE_CENSUS_AUTHORITY_FEATURE],
                route_feature=[],
                source_relative="tests/route-census/other.rs",
            )
            wrong_name = self._metadata_fixture(
                root,
                required_features=[census.ROUTE_CENSUS_AUTHORITY_FEATURE],
                route_feature=[],
                target_name="ordinary-integration-tests",
            )
            self.assertIsNone(
                census._cargo_test_target_for_source(
                    wrong_path, root, census.ROUTE_CENSUS_AUTHORITY_SOURCE_PATH
                )
            )
            self.assertIsNone(
                census._cargo_test_target_for_source(
                    wrong_name, root, census.ROUTE_CENSUS_AUTHORITY_SOURCE_PATH
                )
            )

    def test_wrong_or_extra_cargo_feature_command_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            metadata = self._metadata_fixture(
                root,
                required_features=[census.ROUTE_CENSUS_AUTHORITY_FEATURE],
                route_feature=[],
            )
            target = census._cargo_test_target_for_source(
                metadata, root, census.ROUTE_CENSUS_AUTHORITY_SOURCE_PATH
            )
            self.assertIsNotNone(target)
            assert target is not None
            commands = census._cargo_test_target_commands(
                root, "/toolchain/bin/cargo", target, "census_fixture_exact"
            )
            self.assertIsNotNone(commands)
            assert commands is not None
            expected_build, expected_run = commands
            missing_feature = [arg for arg in expected_build if arg != "--features"]
            missing_feature.remove(census.ROUTE_CENSUS_AUTHORITY_FEATURE)
            extra_feature = [
                *expected_run[:-5], "--features", "unexpected-feature", *expected_run[-5:]
            ]

        self.assertTrue(census._cargo_command_matches(expected_build, expected_build))
        self.assertTrue(census._cargo_command_matches(expected_run, expected_run))
        self.assertFalse(census._cargo_command_matches(missing_feature, expected_build))
        self.assertFalse(census._cargo_command_matches(extra_feature, expected_run))


def _valid_rejection_artifact_evidence_fixture(
    root: Path,
    *,
    input_index: int = 1,
    target_kind: str = "executable",
) -> tuple[list[str], dict]:
    scratch = root / "scratch" / f"input-{input_index:06d}"
    output_path = scratch / ("out.so" if target_kind == "shared_library_probe" else "out")
    mimi = root / "mimi"
    mimi.parent.mkdir(parents=True, exist_ok=True)
    if not mimi.exists():
        mimi.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        mimi.chmod(0o755)
    mimi = mimi.resolve()
    mimi_sha = census.sha256_file(mimi)
    mimi_observation = {
        "path": str(mimi), "resolved_path": str(mimi), "sha256": mimi_sha
    }
    mimi_identity = {
        "path": str(mimi), "sha256_expected": mimi_sha,
        "before": mimi_observation, "after": mimi_observation,
        "matches_expected": True,
    }
    command = [str(mimi), "build", str(root / "case.mimi"), "-o", str(output_path)]
    observation = {"path": str(output_path), "exists": False, "sha256": None}
    evidence = {
        "schema_version": 2,
        "scratch": {
            "path": str(scratch),
            "input_index": input_index,
            "existed_before": False,
            "created_exclusively": True,
            "output_absent_before_build": True,
        },
        "output_path": str(output_path),
        "command_output_path": str(output_path),
        "mimi_executable_identity": mimi_identity,
        "before": dict(observation),
        "after": dict(observation),
    }
    return command, evidence


class RejectionArtifactEvidenceTests(unittest.TestCase):
    def test_collector_emits_exact_output_path_and_absent_observation_pair(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root / "case.mimi"
            source_path.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = ExpectationContractTests._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_build_rejection",
                expected_exit_code=1,
            )
            expected, _coverage = census.bind_expectations_to_inventory(
                {"case.mimi": entry}, [source], root
            )
            scratch_root = root / "scratch"
            scratch_root.mkdir()
            logs_root = root / "logs"
            logs_root.mkdir()
            mimi = root / "mimi"
            mimi.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            mimi.chmod(0o755)
            completed = subprocess.CompletedProcess(
                ["mimi", "build"],
                1,
                b"",
                (
                    b"error[E0432]: expected negative diagnostic\n"
                    b"canonical MIR build rejected: expected diagnostic\n"
                ),
            )
            with patch.object(census.subprocess, "run", return_value=completed):
                record = census._run_one(
                    root=root,
                    source=source,
                    index=1,
                    mimi=mimi,
                    mimi_sha256=census.sha256_file(mimi),
                    timeout=5,
                    scratch_root=scratch_root,
                    logs_root=logs_root,
                    expected=expected,
                )
            command = record["command"]
            evidence = record["rejection_artifact_evidence"]
            self.assertEqual(evidence["before"], {
                "path": evidence["output_path"], "exists": False, "sha256": None
            })
            self.assertEqual(evidence["after"], evidence["before"])
            self.assertTrue(
                census._rejection_artifact_evidence_valid(
                    evidence,
                    command=command,
                    input_index=record["input_index"],
                    target_kind=record["target_kind"],
                    mimi_identity=record["mimi_executable_identity"],
                )
            )

    def test_path_mismatch_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            command, evidence = _valid_rejection_artifact_evidence_fixture(Path(temporary))
            evidence["command_output_path"] = str(Path(evidence["output_path"]).parent / "other")
            self.assertFalse(
                census._rejection_artifact_evidence_valid(
                    evidence,
                    command=command,
                    input_index=1,
                    target_kind="executable",
                    mimi_identity=evidence["mimi_executable_identity"],
                )
            )

    def test_preexisting_output_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            command, evidence = _valid_rejection_artifact_evidence_fixture(Path(temporary))
            evidence["scratch"]["output_absent_before_build"] = False
            evidence["before"]["exists"] = True
            evidence["before"]["sha256"] = "a" * 64
            self.assertFalse(
                census._rejection_artifact_evidence_valid(
                    evidence,
                    command=command,
                    input_index=1,
                    target_kind="executable",
                    mimi_identity=evidence["mimi_executable_identity"],
                )
            )

    def test_missing_before_or_after_observation_is_rejected(self) -> None:
        for missing_field in ("before", "after"):
            with self.subTest(missing_field=missing_field), tempfile.TemporaryDirectory() as temporary:
                command, evidence = _valid_rejection_artifact_evidence_fixture(Path(temporary))
                del evidence[missing_field]
                self.assertFalse(
                    census._rejection_artifact_evidence_valid(
                        evidence,
                        command=command,
                        input_index=1,
                        target_kind="executable",
                        mimi_identity=evidence["mimi_executable_identity"],
                    )
                )


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
    @staticmethod
    def _canonical_route_line(digest: str) -> str:
        return (
            "canonical route disposition: canonical (scalar-actor-domain-v1) "
            "mir_digest=" + digest
        )

    def test_canonical_mir_named_roster_reconciles_owner_count_and_route_digest(self) -> None:
        digest = "a" * 64
        route = census._route_observation(
            "",
            self._canonical_route_line(digest),
            0,
            "completed",
        )
        output = "\n".join(
            [
                self._canonical_route_line(digest),
                "info: canonical MIR native emitted owner='function:Counter::bump'",
                "info: canonical MIR native emitted owner='function:main'",
                "info: canonical MIR native compiled 2/2 function(s), 0 fell back; mir_digest="
                + digest,
            ]
        )
        details = census._function_observations("", output, None, route)
        self.assertEqual(details["status"], "complete_named")
        self.assertTrue(details["named_roster_complete"])
        self.assertTrue(details["consistency"]["mir_route_phase_boundary_is_unique"])
        self.assertTrue(
            details["consistency"]["mir_canonical_roster_and_summary_are_post_route"]
        )
        self.assertEqual(details["counts"]["total_functions"], 2)
        self.assertEqual(details["route_mir_digest"], digest)
        self.assertEqual(
            [event["owner"] for event in details["functions"]],
            ["function:Counter::bump", "function:main"],
        )
        self.assertTrue(
            all(event["emission_phase"] == "post_route" for event in details["functions"])
        )

    def test_canonical_mir_pre_route_only_or_route_line_telemetry_fails_closed(self) -> None:
        digest = "a" * 64
        route_line = self._canonical_route_line(digest)
        route = census._route_observation("", route_line, 0, "completed")
        owner = "info: canonical MIR native emitted owner='function:main'"
        summary = (
            "info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest="
            + digest
        )

        pre_route_only = census._function_observations(
            "", "\n".join([owner, summary, route_line]), None, route
        )
        self.assertFalse(pre_route_only["named_roster_complete"])
        self.assertFalse(
            pre_route_only["consistency"][
                "mir_canonical_roster_and_summary_are_post_route"
            ]
        )

        owner_on_route_line = census._function_observations(
            "", "\n".join([route_line + " " + owner, summary]), None, route
        )
        self.assertFalse(owner_on_route_line["named_roster_complete"])
        self.assertFalse(
            owner_on_route_line["consistency"]["mir_telemetry_lines_are_well_formed"]
        )

        summary_on_route_line = census._function_observations(
            "", "\n".join([owner, route_line + " " + summary]), None, route
        )
        self.assertFalse(summary_on_route_line["named_roster_complete"])
        self.assertFalse(
            summary_on_route_line["consistency"]["mir_telemetry_lines_are_well_formed"]
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
                details = census._function_observations(
                    "", output + "\n" + self._canonical_route_line(digest), None, route
                )
                self.assertFalse(details["named_roster_complete"])

    def test_canonical_mir_pre_route_telemetry_cannot_be_counted_as_complete(self) -> None:
        digest = "e" * 64
        route = census._route_observation(
            "",
            self._canonical_route_line(digest),
            0,
            "completed",
        )
        owners = ["function:Counter::bump", "function:main"]
        owner_lines = "\n".join(
            f"info: canonical MIR native emitted owner='{owner}'" for owner in owners
        )
        summary = (
            "info: canonical MIR native compiled 2/2 function(s), 0 fell back; mir_digest="
            + digest
        )
        output = "\n".join(
            [owner_lines, summary, self._canonical_route_line(digest), owner_lines, summary]
        )

        details = census._function_observations("", output, None, route)

        self.assertFalse(details["named_roster_complete"])
        self.assertFalse(
            details["consistency"]["mir_canonical_roster_and_summary_are_post_route"]
        )
        self.assertEqual(details["counts"]["named_emission_successes"], 4)
        self.assertEqual(details["counts"]["unique_named_functions"], 2)
        self.assertEqual(
            [event["emission_phase"] for event in details["functions"]],
            ["pre_route", "pre_route", "post_route", "post_route"],
        )
        self.assertEqual(
            [event["source_line_index"] for event in details["functions"]],
            [0, 1, 4, 5],
        )
        self.assertTrue(all(event["source_stream"] == "stderr" for event in details["functions"]))
        self.assertEqual(
            details["emission_phase_rosters"],
            {"pre_route": owners, "post_route": owners},
        )
        self.assertEqual(
            [summary["emission_phase"] for summary in details["emitter_summaries"]],
            ["pre_route", "post_route"],
        )
        self.assertEqual(
            [summary["source_line_index"] for summary in details["emitter_summaries"]],
            [2, 6],
        )
        self.assertTrue(
            all(summary["source_stream"] == "stderr" for summary in details["emitter_summaries"])
        )
        self.assertEqual(details["duplicate_function_names"], [])
        self.assertEqual(details["cross_phase_repeated_owners"], owners)

    def test_canonical_mir_same_phase_duplicate_and_phase_roster_mismatch_fail_closed(self) -> None:
        digest = "f" * 64
        route = census._route_observation(
            "",
            self._canonical_route_line(digest),
            0,
            "completed",
        )
        same_phase_duplicate = "\n".join(
            [
                "info: canonical MIR native emitted owner='function:main'",
                "info: canonical MIR native emitted owner='function:main'",
                "info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest="
                + digest,
                self._canonical_route_line(digest),
            ]
        )
        duplicate_details = census._function_observations(
            "", same_phase_duplicate, None, route
        )
        self.assertFalse(duplicate_details["named_roster_complete"])
        self.assertFalse(duplicate_details["consistency"]["mir_owner_events_are_unique"])
        self.assertEqual(duplicate_details["duplicate_function_names"], ["function:main"])

        count_mismatch = census._function_observations(
            "",
            "\n".join(
                [
                    "info: canonical MIR native emitted owner='function:main'",
                    "info: canonical MIR native compiled 2/2 function(s), 0 fell back; mir_digest="
                    + digest,
                    self._canonical_route_line(digest),
                ]
            ),
            None,
            route,
        )
        self.assertFalse(count_mismatch["named_roster_complete"])
        self.assertFalse(
            count_mismatch["consistency"]["mir_named_owner_count_matches_total"]
        )
        self.assertFalse(count_mismatch["consistency"]["mir_emitter_counts_reconcile"])

        changed_roster = "\n".join(
            [
                "info: canonical MIR native emitted owner='function:main'",
                "info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest="
                + digest,
                self._canonical_route_line(digest),
                "info: canonical MIR native emitted owner='function:helper'",
                "info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest="
                + digest,
            ]
        )
        roster_details = census._function_observations("", changed_roster, None, route)
        self.assertFalse(roster_details["named_roster_complete"])
        self.assertFalse(roster_details["consistency"]["mir_phase_rosters_match"])

    def test_canonical_mir_missing_summary_fails_closed(self) -> None:
        digest = "c" * 64
        route = census._route_observation(
            "", self._canonical_route_line(digest), 0, "completed"
        )
        output = "\n".join(
            [
                "info: canonical MIR native emitted owner='function:main'",
                self._canonical_route_line(digest),
            ]
        )

        details = census._function_observations("", output, None, route)

        self.assertFalse(details["named_roster_complete"])
        self.assertFalse(details["consistency"]["mir_summary_is_unique"])
        self.assertEqual(details["emitter_summaries"], [])

    def test_canonical_mir_extra_route_marker_and_cross_stream_order_fail_closed(self) -> None:
        digest = "7" * 64
        route_line = self._canonical_route_line(digest)
        route = census._route_observation("", route_line, 0, "completed")
        owner = "info: canonical MIR native emitted owner='function:main'"
        summary = (
            "info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest="
            + digest
        )

        extra_route = census._function_observations(
            "",
            "\n".join([owner, summary, route_line, route_line]),
            None,
            route,
        )
        self.assertFalse(extra_route["named_roster_complete"])
        self.assertFalse(extra_route["consistency"]["mir_route_phase_boundary_is_unique"])
        self.assertEqual(extra_route["functions"][0]["emission_phase"], "unclassified")

        cross_stream_owner = census._function_observations(
            owner,
            "\n".join([summary, route_line]),
            None,
            route,
        )
        self.assertFalse(cross_stream_owner["named_roster_complete"])
        self.assertFalse(
            cross_stream_owner["consistency"]["mir_emission_events_have_known_phases"]
        )
        self.assertEqual(
            cross_stream_owner["functions"][0]["emission_phase"], "unclassified"
        )
        self.assertEqual(cross_stream_owner["functions"][0]["source_stream"], "stdout")

        cross_stream_summary = census._function_observations(
            summary,
            "\n".join([owner, route_line]),
            None,
            route,
        )
        self.assertFalse(cross_stream_summary["named_roster_complete"])
        self.assertFalse(
            cross_stream_summary["consistency"]["mir_emission_events_have_known_phases"]
        )
        self.assertEqual(
            cross_stream_summary["emitter_summaries"][0]["emission_phase"], "unclassified"
        )

    def test_canonical_mir_same_line_duplicate_route_owner_and_summary_prefixes_fail_closed(self) -> None:
        digest = "8" * 64
        route_line = self._canonical_route_line(digest)
        owner = "info: canonical MIR native emitted owner='function:main'"
        helper_owner = "info: canonical MIR native emitted owner='function:helper'"
        summary = (
            "info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest="
            + digest
        )

        doubled_route_line = route_line + " " + route_line
        doubled_route = census._route_observation(
            "", doubled_route_line, 0, "completed"
        )
        self.assertEqual(doubled_route["disposition"], "unobserved")
        self.assertEqual(doubled_route["classification_status"], "multiple_route_markers")
        self.assertEqual(doubled_route["route_marker_count"], 2)
        route_details = census._function_observations(
            "", "\n".join([owner, summary, doubled_route_line]), None, doubled_route
        )
        self.assertFalse(route_details["named_roster_complete"])
        self.assertFalse(
            route_details["consistency"]["mir_route_phase_boundary_is_unique"]
        )
        self.assertEqual(len(route_details["functions"]), 1)
        self.assertEqual(len(route_details["emitter_summaries"]), 1)
        self.assertEqual(len(route_details["route_markers"]), 2)

        doubled_owner_line = owner + " " + helper_owner
        owner_details = census._function_observations(
            "",
            "\n".join([doubled_owner_line, owner, summary, route_line]),
            None,
            census._route_observation("", route_line, 0, "completed"),
        )
        self.assertFalse(owner_details["named_roster_complete"])
        self.assertFalse(
            owner_details["consistency"]["mir_telemetry_lines_are_well_formed"]
        )
        self.assertEqual(len(owner_details["functions"]), 1)
        self.assertEqual(len(owner_details["malformed_telemetry_lines"]), 1)
        malformed_owner = owner_details["malformed_telemetry_lines"][0]
        self.assertEqual(malformed_owner["source_stream"], "stderr")
        self.assertEqual(malformed_owner["source_line_index"], 0)
        self.assertEqual(malformed_owner["source_line"], doubled_owner_line)
        self.assertEqual(malformed_owner["event_prefix_count"], 2)

        invalid_owner_line = "info: canonical MIR native emitted owner='function:main extra'"
        invalid_owner_details = census._function_observations(
            "",
            "\n".join([invalid_owner_line, owner, summary, route_line]),
            None,
            census._route_observation("", route_line, 0, "completed"),
        )
        self.assertFalse(invalid_owner_details["named_roster_complete"])
        self.assertFalse(
            invalid_owner_details["consistency"]["mir_telemetry_lines_are_well_formed"]
        )
        self.assertEqual(len(invalid_owner_details["functions"]), 1)
        self.assertEqual(
            invalid_owner_details["malformed_telemetry_lines"][0]["source_line"],
            invalid_owner_line,
        )

        doubled_summary_line = summary + " " + summary
        summary_details = census._function_observations(
            "",
            "\n".join([owner, doubled_summary_line, summary, route_line]),
            None,
            census._route_observation("", route_line, 0, "completed"),
        )
        self.assertFalse(summary_details["named_roster_complete"])
        self.assertFalse(
            summary_details["consistency"]["mir_telemetry_lines_are_well_formed"]
        )
        self.assertEqual(len(summary_details["emitter_summaries"]), 1)
        self.assertEqual(len(summary_details["malformed_telemetry_lines"]), 1)
        malformed_summary = summary_details["malformed_telemetry_lines"][0]
        self.assertEqual(malformed_summary["source_stream"], "stderr")
        self.assertEqual(malformed_summary["source_line_index"], 1)
        self.assertEqual(malformed_summary["source_line"], doubled_summary_line)
        self.assertEqual(malformed_summary["summary_prefix_count"], 2)

        invalid_summary_line = summary + " trailing"
        invalid_summary_details = census._function_observations(
            "",
            "\n".join([owner, invalid_summary_line, summary, route_line]),
            None,
            census._route_observation("", route_line, 0, "completed"),
        )
        self.assertFalse(invalid_summary_details["named_roster_complete"])
        self.assertFalse(
            invalid_summary_details["consistency"]["mir_telemetry_lines_are_well_formed"]
        )
        self.assertEqual(len(invalid_summary_details["emitter_summaries"]), 1)
        self.assertEqual(
            invalid_summary_details["malformed_telemetry_lines"][0]["source_line"],
            invalid_summary_line,
        )

    def test_canonical_mir_each_phase_digest_and_summary_cardinality_fail_closed(self) -> None:
        digest = "9" * 64
        route = census._route_observation(
            "",
            self._canonical_route_line(digest),
            0,
            "completed",
        )
        owner = "info: canonical MIR native emitted owner='function:main'"
        summary = (
            "info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest="
            + digest
        )
        wrong_second_digest = "\n".join(
            [owner, summary, self._canonical_route_line(digest), owner, summary[:-64] + "8" * 64]
        )
        digest_details = census._function_observations(
            "", wrong_second_digest, None, route
        )
        self.assertFalse(digest_details["named_roster_complete"])
        self.assertFalse(digest_details["consistency"]["mir_graph_digest_matches_route"])

        duplicate_summary = "\n".join(
            [owner, summary, summary, self._canonical_route_line(digest)]
        )
        summary_details = census._function_observations(
            "", duplicate_summary, None, route
        )
        self.assertFalse(summary_details["named_roster_complete"])
        self.assertFalse(summary_details["consistency"]["mir_summary_is_unique"])

    def test_canonical_mir_fallback_count_or_resolved_stats_is_not_accepted(self) -> None:
        digest = "d" * 64
        route = census._route_observation(
            "",
            self._canonical_route_line(digest),
            0,
            "completed",
        )
        details = census._function_observations(
            "",
            "info: canonical MIR native emitted owner='function:main'\n"
            "info: canonical MIR native compiled 1/1 function(s), 1 fell back; mir_digest="
            + digest
            + "\n"
            + self._canonical_route_line(digest),
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
        missing_evidence: tuple[str, ...] = (),
        retirement_line: str | None = None,
        dynamic_probe_status: str | None = "not_run",
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
        lines = [f"schema={census.LEGACY_OWNER_AUDIT_SCHEMA}"]
        for name, scope, test, marker in census.LEGACY_OWNER_EVIDENCE_TUPLES:
            if name not in missing_evidence:
                lines.append(
                    f"owner={name} evidence_scope={scope} evidence_test={test} "
                    f"evidence_marker={marker}"
                )
        for name, guard, context in census.LEGACY_OWNER_CLOSED_ROUTE_GUARD_TUPLES:
            lines.append(
                f"owner={name} closed_route_guard={guard} context={context}"
            )
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
        ])
        if dynamic_probe_status is not None:
            lines.append(f"dynamic_probe_status={dynamic_probe_status}")
        lines.extend(["audit_status=ok", *extra_lines])
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
        self.assertTrue(parsed["owner_roster_disjoint"])
        self.assertEqual(parsed["owner_roster_overlap"], [])
        self.assertEqual(
            parsed["retirement_evidence_fields"]["physical_status"], "physical"
        )
        self.assertEqual(
            parsed["retirement_evidence_fields"]["ast_owner_status"], "deleted"
        )
        self.assertEqual(
            parsed["retirement_evidence_fields"]["file_adapter_status"], "deleted"
        )
        self.assertEqual(
            parsed["retirement_evidence_fields"]["transition_vir_adapter_status"],
            "deleted",
        )
        self.assertEqual(parsed["dynamic_probe_status"], "not_run")

    def test_repository_audit_script_output_matches_the_parser_contract(self) -> None:
        script = census.ROOT / "scripts/audit-mir-legacy-owners.sh"
        environment = os.environ.copy()
        environment["MIMI_LEGACY_OWNER_AUDIT_SKIP_DYNAMIC_PROBES"] = "1"
        result = subprocess.run(
            ["bash", str(script)],
            cwd=census.ROOT,
            env=environment,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", errors="replace"))
        parsed = census._parse_legacy_owner_audit(
            result.stdout.decode("utf-8", errors="replace")
        )
        self.assertTrue(parsed["parsed_owner_set_complete"])
        self.assertEqual(parsed["legacy_owner_audit_schema"], census.LEGACY_OWNER_AUDIT_SCHEMA)
        self.assertEqual(
            [owner["name"] for owner in parsed["owners"]],
            list(census.ACTIVE_LEGACY_OWNER_NAMES),
        )
        self.assertEqual(
            [owner["name"] for owner in parsed["retired_owners"]],
            list(census.RETIRED_LEGACY_OWNER_NAMES),
        )
        self.assertEqual(
            {name: len(rows) for name, rows in parsed["owner_evidence_rows"].items()},
            {"CodegenLegacyRemainder": 1, "FfiVerifierCompatibility": 5},
        )
        self.assertEqual(
            {
                owner: {
                    (row["scope"], row["test"], row["marker"])
                    for row in rows
                }
                for owner, rows in parsed["owner_evidence_rows"].items()
            },
            {
                "CodegenLegacyRemainder": {
                    (
                        "generic ignored-bool binding uses canonical MIR without any legacy owner",
                        "src/codegen/tests.rs::compile_checked_routes_generic_ignored_bool_binding_projection_without_legacy_access",
                        "test_legacy_codegen_function_calls().is_empty()",
                    ),
                },
                "FfiVerifierCompatibility": {
                    (
                        "typed String FFI context reports checker-owned coverage without any legacy owner",
                        "src/verifier/tests.rs::typed_ffi_symbolic_string_contexts_defer_without_legacy_access",
                        "VerifStatus::NotInTrustedSubset",
                    ),
                    (
                        "checked verifier-only owner slice proves Ready repr(C) String-field Ensures",
                        "src/verifier/tests.rs::typed_ffi_string_record_ensures_use_checker_receipt_and_fresh_result",
                        'Some("verifier-ffi-owner-slice-v1")',
                    ),
                    (
                        "unsupported Ready Ensures stays call-local without discarding Requires proof",
                        "src/verifier/tests.rs::typed_ffi_ready_unsupported_record_ensures_preserves_requires_proof",
                        "result.artifact.is_none()",
                    ),
                    (
                        "nested repr(C) String fields preserve exact caller identity through MIR",
                        "src/verifier/tests.rs::typed_ffi_aggregate_nested_string_fields_preserve_nul_unicode_and_length",
                        'Some("verifier-ffi-owner-slice-v1")',
                    ),
                    (
                        "default verify reports proven, disproven, and unsupported String-record Ensures",
                        "tests/real_world_cli.rs::canonical_ffi_string_record_default_dual_cli_proven_disproven_nits",
                        '"? unknown"',
                    ),
                },
            },
        )
        self.assertTrue(parsed["owner_closed_route_guards_complete"])
        self.assertEqual(
            {
                owner: [(row["guard"], row["context"]) for row in rows]
                for owner, rows in parsed["owner_closed_route_guards"].items()
            },
            {
                "CodegenLegacyRemainder": [
                    (
                        "try_compile_exact_migrated_mir_island",
                        "src/codegen/compile.rs::pub fn compile_checked(",
                    ),
                ],
                "FfiVerifierCompatibility": [
                    (
                        "scalar_ffi_admission",
                        "src/verifier/mod.rs::pub fn verify_ffi_checked_with_source_hash(",
                    ),
                ],
            },
        )
        self.assertTrue(parsed["retired_owner_evidence_complete"])
        self.assertEqual(
            parsed["retirement_evidence_fields"],
            {
                "physical_status": "physical",
                "ast_owner_status": "deleted",
                "file_adapter_status": "deleted",
                "transition_vir_adapter_status": "deleted",
                "declared_parity_test_names": list(census.RETIRED_FLOW_DUAL_PARITY_TESTS),
            },
        )
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
        for line in (
            "",
            valid.replace("physical;", "present;"),
            valid.replace("ast-owner=deleted", "ast-owner=present"),
            valid.replace("file-adapter=deleted", "file-adapter=present"),
            valid.replace("transition-vir-adapter=deleted", "transition-vir-adapter=present"),
            valid.replace(census.RETIRED_FLOW_DUAL_PARITY_TESTS[1], "unknown_test"),
            valid.replace("+" + census.RETIRED_FLOW_DUAL_PARITY_TESTS[2], ""),
        ):
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
        absent_dynamic_status = census._parse_legacy_owner_audit(
            self._audit_text(dynamic_probe_status=None)
        )
        self.assertFalse(absent_dynamic_status["retired_owner_evidence_complete"])

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

        retirement = next(
            line for line in base.splitlines() if line.startswith("retired_flow_dual_owners=")
        )
        duplicate_summary = census._parse_legacy_owner_audit(
            self._audit_text(extra_lines=(retirement,))
        )
        self.assertFalse(duplicate_summary["retired_owner_evidence_complete"])

        unknown = census._parse_legacy_owner_audit(
            self._audit_text(extra_lines=("owner=UnexpectedOwner production_accessor_call_sites=0",))
        )
        self.assertIn("UnexpectedOwner", unknown["unknown_owner_names"])
        self.assertFalse(unknown["parsed_owner_set_complete"])

        repeated_evidence = census._parse_legacy_owner_audit(
            self._audit_text(
                extra_lines=(
                    next(
                        line for line in base.splitlines()
                        if line.startswith("owner=CodegenLegacyRemainder evidence_scope=")
                    ),
                )
            )
        )
        self.assertTrue(repeated_evidence["duplicate_owner_rows"])
        self.assertFalse(repeated_evidence["parsed_owner_set_complete"])

        duplicate_dynamic = census._parse_legacy_owner_audit(
            self._audit_text(extra_lines=("dynamic_probe_status=not_run",))
        )
        self.assertFalse(duplicate_dynamic["retired_owner_evidence_complete"])

    def test_malformed_or_whitespace_global_rows_fail_closed(self) -> None:
        base = self._audit_text()
        retirement = next(
            line for line in base.splitlines()
            if line.startswith("retired_flow_dual_owners=")
        )
        condition_digest = next(
            line for line in base.splitlines()
            if line.startswith("owner_deletion_condition_set_digest=")
        )
        malformed_rows = (
            "owner_count=not-a-number",
            " schema=" + census.LEGACY_OWNER_AUDIT_SCHEMA,
            " " + retirement,
            "dynamic_probe_status =not_run",
            " audit_status=ok",
            " " + condition_digest,
        )
        for line in malformed_rows:
            with self.subTest(line=line):
                parsed = census._parse_legacy_owner_audit(
                    self._audit_text(extra_lines=(line,))
                )
                self.assertIn(line, parsed["malformed_global_rows"])
                self.assertFalse(parsed["parsed_owner_set_complete"])
                if line.lstrip().startswith(
                    ("retired_flow_dual_owners=", "dynamic_probe_status")
                ):
                    self.assertFalse(parsed["retired_owner_evidence_complete"])

    def test_ansi_and_prefixed_global_or_owner_rows_fail_closed(self) -> None:
        cases = (
            (
                "ansi duplicate global key",
                "\x1b[31mowner_count=999\x1b[0m",
                "global",
            ),
            (
                "ansi duplicate schema",
                "\x1b[31mschema=attacker\x1b[0m",
                "global",
            ),
            (
                "prefixed audit status",
                "unexpected-prefix audit_status=failed",
                "global",
            ),
            (
                "bom retired owner row",
                "\ufeffowner=FlowVerifierCompatibility production_accessor_call_sites=0",
                "owner",
            ),
            (
                "ansi active owner row",
                "\x1b[31mowner=FfiVerifierCompatibility production_accessor_call_sites=0\x1b[0m",
                "owner",
            ),
        )
        for label, line, kind in cases:
            with self.subTest(row=label):
                parsed = census._parse_legacy_owner_audit(
                    self._audit_text(extra_lines=(line,))
                )
                self.assertFalse(parsed["parsed_owner_set_complete"])
                if kind == "global":
                    self.assertTrue(parsed["malformed_global_rows"])
                else:
                    self.assertTrue(parsed["malformed_owner_rows"])
                if label == "bom retired owner row":
                    self.assertIn(
                        "FlowVerifierCompatibility", parsed["owner_roster_overlap"]
                    )

    def test_owner_count_requires_ascii_digits(self) -> None:
        malformed = self._audit_text().replace("owner_count=2\n", "owner_count=٢\n")
        parsed = census._parse_legacy_owner_audit(malformed)
        self.assertIsNone(parsed["owner_count_reported"])
        self.assertIn("owner_count=٢", parsed["malformed_global_rows"])
        self.assertFalse(parsed["parsed_owner_set_complete"])

    def test_production_accessor_call_sites_must_be_zero(self) -> None:
        owner = census.ACTIVE_LEGACY_OWNER_NAMES[0]
        positive_count = self._audit_text().replace(
            f"owner={owner} production_accessor_call_sites=0",
            f"owner={owner} production_accessor_call_sites=7",
            1,
        )
        parsed = census._parse_legacy_owner_audit(positive_count)
        self.assertEqual(
            next(row["production_accessor_call_sites"] for row in parsed["owners"] if row["name"] == owner),
            7,
        )
        self.assertFalse(parsed["parsed_owner_set_complete"])

    def test_missing_active_owner_evidence_and_merged_rosters_fail_closed(self) -> None:
        for missing in census.ACTIVE_LEGACY_OWNER_NAMES:
            with self.subTest(missing_evidence=missing):
                parsed = census._parse_legacy_owner_audit(
                    self._audit_text(missing_evidence=(missing,))
                )
                self.assertFalse(parsed["owner_evidence_complete"])
                self.assertFalse(parsed["parsed_owner_set_complete"])

        reintroduced = census._parse_legacy_owner_audit(
            self._audit_text(
                extra_lines=(
                    f"owner={census.RETIRED_LEGACY_OWNER_NAMES[0]} "
                    "production_accessor_call_sites=0",
                )
            )
        )
        self.assertIn(census.RETIRED_LEGACY_OWNER_NAMES[0], reintroduced["owner_roster_overlap"])
        self.assertFalse(reintroduced["owner_roster_disjoint"])
        self.assertFalse(reintroduced["parsed_owner_set_complete"])

    def test_owner_evidence_and_guards_require_exact_expected_tuples(self) -> None:
        base_lines = self._audit_text().splitlines()
        ffi_evidence = [
            line for line in base_lines
            if line.startswith("owner=FfiVerifierCompatibility evidence_scope=")
        ]
        self.assertEqual(len(ffi_evidence), 5)
        codegen_evidence = next(
            line for line in base_lines
            if line.startswith("owner=CodegenLegacyRemainder evidence_scope=")
        )
        forged_ffi = (
            "owner=FfiVerifierCompatibility evidence_scope=forged scope "
            "evidence_test=missing::test evidence_marker=no assertion"
        )
        shortened = [
            line for line in base_lines
            if not line.startswith("owner=FfiVerifierCompatibility evidence_scope=")
        ]
        shortened.insert(shortened.index(codegen_evidence) + 1, forged_ffi)
        parsed_shortened = census._parse_legacy_owner_audit("\n".join(shortened) + "\n")
        self.assertFalse(parsed_shortened["owner_evidence_complete"])
        self.assertFalse(parsed_shortened["parsed_owner_set_complete"])

        forged_guard_lines = [
            line.replace(
                "closed_route_guard=try_compile_exact_migrated_mir_island "
                "context=src/codegen/compile.rs::pub fn compile_checked(",
                "closed_route_guard=bogus_guard context=missing::entry",
            )
            for line in base_lines
        ]
        parsed_guard = census._parse_legacy_owner_audit(
            "\n".join(forged_guard_lines) + "\n"
        )
        self.assertFalse(parsed_guard["owner_closed_route_guards_complete"])
        self.assertFalse(parsed_guard["parsed_owner_set_complete"])

        forged_ffi_row = census._parse_legacy_owner_audit(
            "\n".join(
                line.replace(ffi_evidence[0], forged_ffi) if line == ffi_evidence[0] else line
                for line in base_lines
            ) + "\n"
        )
        self.assertFalse(forged_ffi_row["owner_evidence_complete"])
        self.assertFalse(forged_ffi_row["parsed_owner_set_complete"])

    def test_malformed_active_and_reintroduced_retired_owner_rows_fail_closed(self) -> None:
        malformed_rows = (
            f"owner={census.ACTIVE_LEGACY_OWNER_NAMES[0]} "
            "production_accessor_call_sites=not-a-number",
            f"owner={census.ACTIVE_LEGACY_OWNER_NAMES[0]} garbage",
            f" owner={census.ACTIVE_LEGACY_OWNER_NAMES[0]} production_accessor_call_sites=0",
            f"owner = {census.ACTIVE_LEGACY_OWNER_NAMES[0]} production_accessor_call_sites=0",
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
                "status": "active",
                "production_accessor_call_sites": 0,
                "production_access_status": "sealed",
                "dependency_class": "fixture-dependency",
                "owner_deletion_ready": False,
                "owner_deletion_blocker": "retained compatibility evidence",
                "deletion_condition": condition,
                "deletion_condition_sha256": census.sha256_bytes(condition.encode("utf-8")),
            })
        retirement_evidence = {
            "physical_status": "physical",
            "ast_owner_status": "deleted",
            "file_adapter_status": "deleted",
            "transition_vir_adapter_status": "deleted",
            "declared_parity_test_names": list(census.RETIRED_FLOW_DUAL_PARITY_TESTS),
        }
        retired_owners = [
            {
                "name": name,
                "status": "physically_retired",
                "retirement_evidence_kind": "audit_script_source_checks",
                "retirement_evidence_complete": True,
                "retirement_evidence": dict(retirement_evidence),
                "declared_parity_test_names": list(census.RETIRED_FLOW_DUAL_PARITY_TESTS),
                "dynamic_probe_status": "not_run",
            }
            for name in census.RETIRED_LEGACY_OWNER_NAMES
        ]
        return {
            "accepted_as_complete_inventory": True,
            "owner_count_reported": len(owners),
            "active_owner_names": list(census.ACTIVE_LEGACY_OWNER_NAMES),
            "retired_owner_names": list(census.RETIRED_LEGACY_OWNER_NAMES),
            "owner_deletion_condition_set_sha256": "e" * 64,
            "deletion_condition_digests_match_text": True,
            "parsed_owner_set_complete": True,
            "retired_owner_evidence_complete": True,
            "retirement_evidence_fields": retirement_evidence,
            "legacy_owner_audit_schema": census.LEGACY_OWNER_AUDIT_SCHEMA,
            "owner_evidence_complete": True,
            "owner_evidence_rows": {
                name: [
                    {"scope": scope, "test": test, "marker": marker}
                    for owner_name, scope, test, marker
                    in census.LEGACY_OWNER_EVIDENCE_TUPLES
                    if owner_name == name
                ]
                for name in census.ACTIVE_LEGACY_OWNER_NAMES
            },
            "owner_closed_route_guards_complete": True,
            "owner_closed_route_guards": {
                name: [
                    {"guard": guard, "context": context}
                    for owner_name, guard, context
                    in census.LEGACY_OWNER_CLOSED_ROUTE_GUARD_TUPLES
                    if owner_name == name
                ]
                for name in census.ACTIVE_LEGACY_OWNER_NAMES
            },
            "owner_roster_disjoint": True,
            "owner_roster_overlap": [],
            "dynamic_probe_status": "not_run",
            "unknown_owner_names": [],
            "duplicate_owner_fields": [],
            "duplicate_owner_rows": [],
            "malformed_condition_rows": [],
            "malformed_owner_rows": [],
            "malformed_retirement_rows": [],
            "malformed_global_rows": [],
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
                "test_contract_valid": True,
                "actor_specific_authority": path in census.ACTOR_FALLBACK_INPUTS,
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
        owner_audit: dict | None = None,
        root: Path | None = None,
        strict_check: bool = False,
        omit_mimi_identity: bool = False,
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
        mimi_fixture_dir: tempfile.TemporaryDirectory[str] | None = None
        command_records = [record for record in records if record.get("command") is not None]
        if command_records:
            record_identity = command_records[0].get("mimi_executable_identity")
            if isinstance(record_identity, dict):
                gate_mimi_identity = {
                    "path": record_identity.get("path"),
                    "sha256": record_identity.get("sha256_expected"),
                    "before_authority_tests": record_identity.get("before"),
                    "after_collection": record_identity.get("after"),
                }
            else:
                gate_mimi_identity = {}
        else:
            mimi_fixture_dir = tempfile.TemporaryDirectory(
                prefix="route-census-gate-mimi-"
            )
            mimi_fixture_path = Path(mimi_fixture_dir.name) / "mimi"
            mimi_fixture_path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            mimi_fixture_path.chmod(0o755)
            mimi_fixture_path = mimi_fixture_path.resolve(strict=True)
            mimi_fixture_observation = census._executable_observation(
                str(mimi_fixture_path)
            )
            mimi_fixture_sha256 = mimi_fixture_observation.get("sha256")
            gate_mimi_identity = {
                "path": str(mimi_fixture_path),
                "sha256": mimi_fixture_sha256,
                "before_authority_tests": mimi_fixture_observation,
                "after_collection": mimi_fixture_observation,
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
            mimi_path = gate_mimi_identity.get("path")
            mimi_sha256 = gate_mimi_identity.get("sha256")
            marker = census._authority_mimi_identity_marker(
                mimi_path if isinstance(mimi_path, str) else None,
                mimi_sha256 if isinstance(mimi_sha256, str) else None,
            )
            stdout = (
                (marker + "\n" if marker is not None else "")
                + f"running 1 test\ntest {test_id} ... ok\n\n"
                "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
                "0 filtered out; finished in 0.00s\n"
            ).encode()
            actor_execution_cases.append({
                "path": path,
                "test_id": test_id,
                "authority_path": "tests/real_world_cli.rs",
                "command": [
                    actor_cargo_authority["cargo"]["path"], "test", "--test",
                    "real_world_cli", test_id, "--", "--exact", "--nocapture"
                ],
                "mimi_executable_identity": {
                    "path": mimi_path,
                    "sha256": mimi_sha256,
                    "sha256_before": mimi_sha256,
                    "sha256_after": mimi_sha256,
                },
                "mimi_environment_overrides": census._expected_mimi_environment_observation(
                    mimi_path if isinstance(mimi_path, str) else None,
                    mimi_sha256 if isinstance(mimi_sha256, str) else None,
                ),
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
            mimi_identity=gate_mimi_identity,
        )
        owner_stdout = census._raw_capture(
            b"owner audit output\n", "legacy-owner-audit.stdout.bin"
        )
        owner_stderr = census._raw_capture(
            b"", "legacy-owner-audit.stderr.bin"
        )
        owner_audit = owner_audit if owner_audit is not None else cls._owner_audit()
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
            try:
                gate_report = census._gate_report(
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
                        census.cargo_test_toolchain_authority(
                            effective_root or census.ROOT
                        )
                    ),
                    mimi_identity=(
                        None if omit_mimi_identity else gate_mimi_identity
                    ),
                    strict_check=strict_check,
                )
            finally:
                if mimi_fixture_dir is not None:
                    mimi_fixture_dir.cleanup()
            return gate_report

    def test_gate_can_pass_only_for_exact_canonical_zero_fallback_roster(self) -> None:
        gate = self._gate(
            [],
            [],
            source_scope={"exactly_once": True},
            expectation_contract={
                "exact_bijection": True,
                "duplicate_inventory_paths": [],
                "invalid_source_hash_paths": [],
                "invalid_authority_paths": [],
                "authority_bundle_stable": True,
            },
        )
        self.assertTrue(gate["passed"])
        self.assertEqual(
            gate["canonical_success_inputs"], sorted(census.ACTOR_FALLBACK_INPUTS)
        )

    @staticmethod
    def _complete_source_bound_contract() -> dict:
        return {
            "exact_bijection": True,
            "duplicate_inventory_paths": [],
            "invalid_source_hash_paths": [],
            "invalid_authority_paths": [],
            "authority_bundle_stable": True,
        }

    def test_strict_gate_requires_explicit_source_bound_inputs(self) -> None:
        positive = self._gate(
            [],
            [],
            source_scope={"exactly_once": True},
            expectation_contract=self._complete_source_bound_contract(),
            strict_check=True,
        )
        self.assertTrue(positive["passed"])
        self.assertTrue(
            positive["checks"]["all_required_source_bound_gate_inputs_are_present"]
        )

        missing_source_scope = self._gate(
            [],
            [],
            expectation_contract=self._complete_source_bound_contract(),
            strict_check=True,
        )
        self.assertFalse(
            missing_source_scope["checks"]["source_scope_is_accounted_exactly_once"]
        )
        self.assertFalse(
            missing_source_scope["checks"]["all_required_source_bound_gate_inputs_are_present"]
        )

        missing_identity = self._gate(
            [],
            [],
            source_scope={"exactly_once": True},
            expectation_contract=self._complete_source_bound_contract(),
            strict_check=True,
            omit_mimi_identity=True,
        )
        self.assertFalse(
            missing_identity["checks"][
                "mimi_binary_identity_is_stable_for_census_and_authorities"
            ]
        )
        self.assertFalse(
            missing_identity["checks"]["all_required_source_bound_gate_inputs_are_present"]
        )

        missing_contract = self._gate(
            [], [], source_scope={"exactly_once": True}, strict_check=True
        )
        self.assertFalse(
            missing_contract["checks"]["expectation_paths_exactly_biject_inventory"]
        )
        self.assertFalse(
            missing_contract["checks"]["generated_authority_bundle_is_stable"]
        )
        self.assertFalse(
            missing_contract["checks"]["all_required_source_bound_gate_inputs_are_present"]
        )

        missing_bundle_stability = self._complete_source_bound_contract()
        del missing_bundle_stability["authority_bundle_stable"]
        unstable_bundle_contract = self._gate(
            [],
            [],
            source_scope={"exactly_once": True},
            expectation_contract=missing_bundle_stability,
            strict_check=True,
        )
        self.assertFalse(
            unstable_bundle_contract["checks"]["generated_authority_bundle_is_stable"]
        )
        self.assertFalse(
            unstable_bundle_contract["checks"][
                "all_required_source_bound_gate_inputs_are_present"
            ]
        )

    def test_gate_revalidates_owner_tuples_and_zero_accessor_count(self) -> None:
        forged_evidence = self._owner_audit()
        forged_evidence["owner_evidence_rows"]["FfiVerifierCompatibility"] = [
            {
                "scope": "forged scope",
                "test": "missing::test",
                "marker": "no assertion",
            }
        ]
        forged_guard = self._owner_audit()
        forged_guard["owner_closed_route_guards"]["CodegenLegacyRemainder"] = [
            {"guard": "bogus_guard", "context": "missing::entry"}
        ]
        positive_accessor_count = self._owner_audit()
        positive_accessor_count["owners"][0]["production_accessor_call_sites"] = 7

        for label, owner_audit in (
            ("forged evidence", forged_evidence),
            ("forged guard", forged_guard),
            ("positive accessor count", positive_accessor_count),
        ):
            with self.subTest(owner_audit=label):
                gate = self._gate([], [], owner_audit=owner_audit)
                self.assertFalse(
                    gate["checks"][
                        "two_active_owners_audited_and_two_retired_owners_statically_verified"
                    ]
                )

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

    def test_proven_source_diagnostic_stays_separate_and_route_markers_fail_closed(self) -> None:
        path = "tests/a.mimi"

        def source_diagnostic_record() -> dict:
            record = self._record(path, route="rejected")
            absolute_source = str((census.ROOT / path).resolve())
            diagnostic = (
                f"{absolute_source} has 1 type error(s):\n"
                f"error[E0432] {absolute_source}:2:1-2 unknown name\n"
                "error: type checking failed\n"
            )
            record.update({
                "run_status": "completed",
                "exit_code": 1,
                "route": {
                    "disposition": "rejected",
                    "classification_status": "inferred_pre_route_rejection",
                    "observations": [],
                    "route_marker_count": 0,
                },
                "failure_classification": {
                    "class": "source_diagnostic",
                    "status": "classified",
                },
                "per_function": {
                    "named_roster_complete": False,
                    "has_fallback": False,
                },
                "output_artifact": None,
                "source_stable_for_input": True,
                "source_diagnostic_artifact_evidence": {"before": {}, "after": {}},
                "stdout_raw": census._raw_capture(b"", "source-diagnostic.stdout.bin"),
                "stderr_raw": census._raw_capture(
                    diagnostic.encode("utf-8"), "source-diagnostic.stderr.bin"
                ),
            })
            record["expectation"] = {
                "class": "expected_source_diagnostic",
                "status": "matched",
                "matched": True,
                "explicit_expectation": True,
                "source_hash_status": "matched",
                "authority_valid": True,
                "test_contract_valid": True,
                "source_authority_valid": True,
                "cargo_target_valid": True,
                "test_execution_valid": True,
                "expected": {"exit_code": 1},
                "diagnostic_contract": {
                    "valid": True,
                    "diagnostic_codes_match_exactly": True,
                    "diagnostic_lines_match_exactly": True,
                    "diagnostic_text_matches_exactly": True,
                    "stdout_empty": True,
                    "unknown_or_extra_diagnostics_absent": True,
                    "route_telemetry_absent": True,
                    "fallback_markers_absent": True,
                    "output_artifact_absent": True,
                },
            }
            return record

        with (
            patch.object(
                census,
                "_expected_source_diagnostic_execution_proof_is_valid",
                return_value=True,
            ),
            patch.object(
                census,
                "_rejection_artifact_evidence_valid",
                return_value=True,
            ),
        ):
            accepted = self._gate([path], [source_diagnostic_record()])
            self.assertTrue(
                accepted["checks"]["all_expected_source_diagnostics_are_proven_separately"]
            )
            self.assertTrue(
                accepted["checks"]["all_expected_source_diagnostics_bind_exact_absent_output_paths"]
            )
            self.assertTrue(
                accepted["checks"][
                    "source_diagnostics_never_become_observed_route_rejections_or_canonical_rosters"
                ]
            )
            self.assertEqual(accepted["passed_expected_source_diagnostics"], [path])
            self.assertEqual(accepted["passed_expected_rejections"], [])
            self.assertEqual(accepted["expected_negative_matches"], [])

            forged = source_diagnostic_record()
            forged["route"].update({
                "classification_status": "observed",
                "observations": [{"kind": "route_rejection"}],
                "route_marker_count": 1,
            })
            forged["per_function"]["named_roster_complete"] = True
            rejected = self._gate([path], [forged])
            self.assertFalse(
                rejected["checks"]["all_expected_source_diagnostics_are_proven_separately"]
            )
            self.assertFalse(
                rejected["checks"][
                    "source_diagnostics_never_become_observed_route_rejections_or_canonical_rosters"
                ]
            )

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

        malformed_owner_audit = self._owner_audit()
        malformed_owner_audit["malformed_global_rows"] = ["owner_count=invalid"]
        malformed_gate = census._gate_report(
            records,
            manifest,
            True,
            [],
            {"status": "available", "path_count": 0, "paths_sha256": census._manifest_paths_hash([])},
            actors,
            malformed_owner_audit,
        )
        self.assertFalse(
            malformed_gate["checks"][
                "two_active_owners_audited_and_two_retired_owners_statically_verified"
            ]
        )


class ExpectationContractTests(unittest.TestCase):
    def test_preflight_accepts_source_contract_before_runtime_authority_exists(self) -> None:
        pending_classes = (
            "required_compile",
            "expected_build_rejection",
            "expected_source_diagnostic",
        )
        for expectation_class in pending_classes:
            with self.subTest(expectation_class=expectation_class):
                expectation = {
                    "class": expectation_class,
                    "source_hash_matches": True,
                    "test_contract_valid": True,
                    "source_authority_valid": True,
                    "authority_valid": False,
                }
                self.assertTrue(
                    census.expectation_source_contract_valid_for_preflight(expectation)
                )
                self.assertIs(
                    expectation["authority_valid"],
                    False,
                    "preflight must not manufacture runtime authority",
                )

    def test_preflight_rejects_stale_or_invalid_source_contracts(self) -> None:
        pending = {
            "class": "expected_source_diagnostic",
            "source_hash_matches": True,
            "test_contract_valid": True,
            "source_authority_valid": True,
            "authority_valid": False,
        }
        for field in ("source_hash_matches", "test_contract_valid", "source_authority_valid"):
            with self.subTest(field=field):
                invalid = dict(pending)
                invalid[field] = False
                self.assertFalse(
                    census.expectation_source_contract_valid_for_preflight(invalid)
                )

        self.assertFalse(
            census.expectation_source_contract_valid_for_preflight(
                {"class": "unmapped", "source_hash_matches": True, "authority_valid": False}
            )
        )

    def test_preflight_keeps_other_classes_on_final_authority_validity(self) -> None:
        accepted = {
            "class": "canonical",
            "source_hash_matches": True,
            "authority_valid": True,
        }
        rejected = {**accepted, "authority_valid": False}
        self.assertTrue(census.expectation_source_contract_valid_for_preflight(accepted))
        self.assertFalse(census.expectation_source_contract_valid_for_preflight(rejected))

    def test_preflight_reports_all_runtime_tests_even_with_static_actor_authority(self) -> None:
        bound = {
            "actor.mimi": {
                "class": "required_compile",
                "authority_valid": True,
                "actor_specific_authority": True,
            },
            "canonical.mimi": {"class": "canonical", "authority_valid": True},
            "diagnostic.mimi": {"class": "expected_source_diagnostic", "authority_valid": False},
            "rejected.mimi": {"class": "expected_build_rejection", "authority_valid": False},
            "generic.mimi": {"class": "required_compile", "authority_valid": False},
        }
        self.assertEqual(
            census.expectation_paths_requiring_runtime_authority(bound),
            ["actor.mimi", "diagnostic.mimi", "generic.mimi", "rejected.mimi"],
        )
        self.assertIs(bound["actor.mimi"]["authority_valid"], True)

    @staticmethod
    def _mimi_identity(root: Path) -> tuple[str, str]:
        mimi = root / "target" / "debug" / "mimi"
        mimi.parent.mkdir(parents=True, exist_ok=True)
        mimi.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        mimi.chmod(0o755)
        mimi = mimi.resolve()
        return str(mimi), census.sha256_file(mimi)

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
        is_source_diagnostic = expectation_class == "expected_source_diagnostic"
        assertion_relative_path = (
            "tests/route_contract.rs"
            if expectation_class in {
                "required_compile", "expected_build_rejection", "expected_source_diagnostic"
            }
            else "tests/route_contract.py"
        )
        assertion_path = root / assertion_relative_path
        assertion_path.parent.mkdir(parents=True, exist_ok=True)
        if expectation_class in {
            "required_compile", "expected_build_rejection", "expected_source_diagnostic"
        } and not (root / "Cargo.toml").exists():
            (root / "Cargo.toml").write_text(
                '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n',
                encoding="utf-8",
            )
        test_id = "test_route_contract"
        if is_source_diagnostic:
            diagnostic_text = (
                f"{(root / source_path).resolve()} has 1 type error(s):\n"
                f"error[E0432] {(root / source_path).resolve()}:2:3-8 unknown name 'absent'\n"
                "error: type checking failed\n"
            )
            output_path = census._source_diagnostic_authority_output_path(
                source_path, source_sha256
            )
            source, assertion_line = census._render_expected_source_diagnostic_test(
                test_id=test_id,
                source_path=source_path,
                source_sha256=source_sha256,
                expected_exit_code=(
                    expected_exit_code if expected_exit_code is not None else 1
                ),
                target_kind="executable",
                output_path=output_path,
                expected_diagnostic_text=diagnostic_text,
                root=root,
            )
            assertion_path.write_text(source, encoding="utf-8")
        elif is_rejection:
            fixture_path = "".join(
                f'.join({json.dumps(part)})' for part in Path(source_path).parts
            )
            assertion_path.write_text(
                "#[test]\n"
                f"fn {test_id}() {{\n"
                "    let fixture = ::std::path::Path::new(::std::env!(\"CARGO_MANIFEST_DIR\"))"
                f"{fixture_path};\n"
                "    let result = ::std::process::Command::new(route_census_mimi_path())\n"
                "        .current_dir(::std::env!(\"CARGO_MANIFEST_DIR\"))\n"
                '        .arg("build")\n'
                "        .arg(&fixture)\n"
                '        .arg("-o")\n'
                '        .arg("route-contract-output")\n'
                '        .env("MIMI_ROUTE_CENSUS", "1")\n'
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
            assertion_line = 15
        elif expectation_class == "required_compile":
            fixture_path = "".join(
                f'.join({json.dumps(part)})' for part in Path(source_path).parts
            )
            source = (
                "#[test]\n"
                f"fn {test_id}() {{\n"
                "    let fixture = ::std::path::Path::new(::std::env!(\"CARGO_MANIFEST_DIR\"))"
                f"{fixture_path};\n"
                "    let result = ::std::process::Command::new(route_census_mimi_path())\n"
                "        .current_dir(::std::env!(\"CARGO_MANIFEST_DIR\"))\n"
                '        .arg("build")\n'
                "        .arg(&fixture)\n"
                '        .arg("-o")\n'
                '        .arg("target/required-compile-route-contract.out")\n'
                '        .env("MIMI_ROUTE_CENSUS", "1")\n'
                "        .output()\n"
                '        .expect("failed to execute exact fixture build");\n'
                "    ::std::assert!(result.status.success());\n"
                "    let stderr = ::std::string::String::from_utf8_lossy(&result.stderr);\n"
                '    ::std::assert!(stderr.contains("canonical route disposition: canonical ("));\n'
                '    ::std::assert!(!stderr.contains("canonical route disposition: legacy"));\n'
                '    ::std::assert!(!stderr.contains("canonical route disposition: compatibility"));\n'
                '    ::std::assert!(!stderr.contains("fallback"));\n'
                '    ::std::assert!(!stderr.contains("compatibility"));\n'
                '    ::std::assert!(stderr.contains("MIMI_ROUTE_CENSUS route-admission consumer=build mode=default "));\n'
                '    ::std::assert!(stderr.contains("MIMI_ROUTE_CENSUS canonical-native-build consumer=build "));\n'
                '    ::std::assert!(stderr.contains("mir_digest="));\n'
                '    ::std::assert!(stderr.contains("receipt_identity_digest="));\n'
                "}\n"
            )
            assertion_path.write_text(source, encoding="utf-8")
            assertion_line = 15
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
        if is_source_diagnostic:
            entry.update({
                "expected_exit_code": (
                    expected_exit_code if expected_exit_code is not None else 1
                ),
                "expected_diagnostic_codes": ["E0432"],
                "expected_diagnostic_lines": census._source_diagnostic_lines(
                    diagnostic_text
                ),
                "expected_diagnostic_text": diagnostic_text,
                "expected_output_path": output_path,
            })
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
        expectation_class: str = "expected_build_rejection",
    ) -> dict:
        logs = root / "authority-logs"
        logs.mkdir(exist_ok=True)
        mimi_identity = self._mimi_identity(root)
        first = next(
            item for item in bound.values()
            if item.get("class") == expectation_class
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
            "required-features": None,
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
            marker = census._authority_mimi_identity_marker(*mimi_identity)
            assert marker is not None
            stdout = marker.encode("utf-8") + b"\n" + stdout
            return subprocess.CompletedProcess(command, 0, stdout, b"")

        cargo_authority = census.cargo_test_toolchain_authority(root)
        runner = (
            census._run_source_diagnostic_authority_tests
            if expectation_class == "expected_source_diagnostic"
            else census._run_expected_rejection_authority_tests
        )
        with patch.object(census.subprocess, "run", side_effect=fake_run):
            result = runner(
                root, logs, sources, bound, timeout=5,
                cargo_authority=cargo_authority,
                mimi_path=mimi_identity[0],
                mimi_sha256=mimi_identity[1],
            )
        return result

    def _execute_required_compile_authority_tests(
        self,
        root: Path,
        sources: list[dict],
        bound: dict,
    ) -> dict:
        logs = root / "required-compile-authority-logs"
        logs.mkdir(exist_ok=True)
        mimi_identity = self._mimi_identity(root)
        first = next(
            item for item in bound.values()
            if item.get("class") == "required_compile"
        )
        authority_path = root / first["authority"]["assertion"]["path"]
        target = {
            "name": authority_path.stem,
            "src_path": str(authority_path.resolve()),
            "kind": ["test"],
            "crate_types": ["bin"],
            "test": True,
            "required-features": None,
            "doc": False,
        }
        package_id = "fixture 0.1.0 (path+file:///fixture)"
        metadata = {
            "packages": [{
                "id": package_id,
                "name": "fixture",
                "manifest_path": str(root / "Cargo.toml"),
                "targets": [target],
            }],
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
            stdout = (
                f"running 1 test\ntest {test_id} ... ok\n\n"
                "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
                "0 filtered out; finished in 0.00s\n"
            ).encode("utf-8")
            marker = census._authority_mimi_identity_marker(*mimi_identity)
            assert marker is not None
            stdout = marker.encode("utf-8") + b"\n" + stdout
            return subprocess.CompletedProcess(command, 0, stdout, b"")

        cargo_authority = census.cargo_test_toolchain_authority(root)
        with patch.object(census.subprocess, "run", side_effect=fake_run):
            return census._run_required_compile_authority_tests(
                root, logs, sources, bound, timeout=5,
                cargo_authority=cargo_authority,
                mimi_path=mimi_identity[0],
                mimi_sha256=mimi_identity[1],
            )

    def _attach_valid_rejection_artifact_evidence(
        self, record: dict, root: Path, source_path: str, *, input_index: int = 1
    ) -> None:
        target_kind = "executable"
        command, evidence = _valid_rejection_artifact_evidence_fixture(
            root, input_index=input_index, target_kind=target_kind
        )
        command[2] = str(root / source_path)
        record["input_index"] = input_index
        record["target_kind"] = target_kind
        record["command"] = command
        record["rejection_artifact_evidence"] = evidence
        record["mimi_executable_identity"] = evidence["mimi_executable_identity"]

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
        self._attach_valid_rejection_artifact_evidence(record, root, source_path)
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
            mimi, mimi_sha256 = _write_test_mimi(root)

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
                    mimi=mimi,
                    mimi_sha256=mimi_sha256,
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
            self.assertTrue(bound["case.mimi"]["test_contract_valid"])
            self.assertTrue(bound["case.mimi"]["source_authority_valid"])
            self.assertFalse(bound["case.mimi"]["authority_valid"])
            self.assertFalse(bound["case.mimi"]["test_execution_valid"])
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

    def test_required_compile_authority_rejects_unrelated_test_path_or_assertion(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            relative = "tests/fixtures/case.mimi"
            source_path = root / relative
            source_path.parent.mkdir(parents=True)
            source_path.write_text("func main() { println(1) }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(root, relative, source["sha256"])
            authority_path = root / entry["authority"]["assertion"]["path"]
            original = authority_path.read_text(encoding="utf-8")
            cases = {
                "unrelated test id": original.replace(
                    "fn test_route_contract()", "fn test_other_contract()"
                ),
                "unrelated source path": original.replace(
                    '.join("case.mimi")', '.join("different.mimi")'
                ),
                "explicit MIR build": original.replace(
                    '.arg("build")', '.arg("build")\n        .arg("--mir")'
                ),
                "unrelated assertion only": original.split(
                    '    ::std::assert!(stderr.contains("canonical route disposition: canonical ("));'
                )[0]
                + "}\n",
            }
            for label, altered in cases.items():
                with self.subTest(authority=label):
                    authority_path.write_text(altered, encoding="utf-8")
                    entry["authority"]["assertion"]["sha256"] = census.sha256_file(
                        authority_path
                    )
                    bound, coverage = self._load_and_bind(root, {relative: entry}, [source])
                    self.assertFalse(bound[relative]["test_contract_valid"])
                    self.assertFalse(bound[relative]["source_authority_valid"])
                    self.assertFalse(bound[relative]["authority_valid"])
                    self.assertEqual(coverage["invalid_authority_paths"], [relative])

            authority_path.write_text(original, encoding="utf-8")
            entry["authority"]["assertion"]["sha256"] = census.sha256_file(authority_path)
            entry["authority"]["assertion"]["line"] = 13
            cited_unrelated_assertion, _coverage = self._load_and_bind(
                root, {relative: entry}, [source]
            )

        self.assertFalse(cited_unrelated_assertion[relative]["test_contract_valid"])

    def test_required_compile_authority_runs_the_pinned_exact_source_test(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            relative = "tests/fixtures/case.mimi"
            source_path = root / relative
            source_path.parent.mkdir(parents=True)
            source_path.write_text("func main() { println(1) }\n", encoding="utf-8")
            source = census.source_record(source_path, root)
            entry = self._entry(root, relative, source["sha256"])
            bound, coverage = self._load_and_bind(root, {relative: entry}, [source])

            self.assertTrue(bound[relative]["test_contract_valid"])
            execution = self._execute_required_compile_authority_tests(
                root, [source], bound
            )
            coverage["invalid_authority_paths"] = sorted(
                path for path, item in bound.items()
                if item.get("class") != "unmapped" and item.get("authority_valid") is not True
            )
            result = census._expectation_result(
                relative,
                0,
                {"disposition": "canonical", "classification_status": "observed"},
                bound,
                run_status="completed",
                captured_output=b"canonical route disposition: canonical (default-route)\n",
                output_artifact={"sha256": "a" * 64, "bytes": 8},
            )

        self.assertEqual(execution["status"], "passed")
        self.assertEqual(bound[relative]["authority_status"], "valid")
        self.assertTrue(bound[relative]["test_execution_valid"])
        self.assertEqual(bound[relative]["test_execution_proof"]["test_id"], "test_route_contract")
        self.assertEqual(bound[relative]["test_execution_proof"]["fixture_path"], relative)
        self.assertTrue(result["matched"])
        self.assertTrue(coverage["exact_bijection"])

    def test_source_diagnostic_authority_and_collector_use_plain_text_mode(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "case.mimi"
            fixture.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(fixture, root)
            output_path = census._source_diagnostic_authority_output_path(
                "case.mimi", source["sha256"]
            )
            diagnostic_text = (
                f"{fixture} has 1 type error(s):\n"
                f"error[E0432] {fixture}:2:3-8 unknown name 'absent'\n"
                "error: type checking failed\n"
            )
            authority, _line = census._render_expected_source_diagnostic_test(
                test_id="test_plain_source_diagnostic",
                source_path="case.mimi",
                source_sha256=source["sha256"],
                expected_exit_code=1,
                target_kind="executable",
                output_path=output_path,
                expected_diagnostic_text=diagnostic_text,
                root=root,
            )

        self.assertIn('.env("NO_COLOR", "1")', authority)
        self.assertIn(
            'env["NO_COLOR"] = "1"', inspect.getsource(census._run_one)
        )
        self.assertIn('"NO_COLOR": "1"', inspect.getsource(census.toolchain_manifest))

    def test_source_diagnostic_build_layouts_bind_counts_and_exact_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            fixture = root / "case.mimi"
            fixture.write_text("func main() { }\n", encoding="utf-8")
            absolute_source = str(fixture)

            type_errors = (
                f"{absolute_source} has 2 type error(s):\n"
                f"error[E0432] {absolute_source}:1:1-2 first error\n"
                f"error[E0432] {absolute_source}:1:3-4 second error\n"
                "error: type checking failed\n"
            )
            parse_errors = (
                f"error {absolute_source}:1:1-2 unexpected token\n"
                "error: 1 parse error(s) found\n"
            )

            self.assertTrue(
                census._source_diagnostic_text_is_well_formed(
                    type_errors, source_path="case.mimi", root=root
                )
            )
            self.assertTrue(
                census._source_diagnostic_text_is_well_formed(
                    parse_errors, source_path="case.mimi", root=root
                )
            )
            self.assertFalse(
                census._source_diagnostic_text_is_well_formed(
                    type_errors.replace("has 2 type error(s)", "has 3 type error(s)"),
                    source_path="case.mimi",
                    root=root,
                )
            )
            self.assertFalse(
                census._source_diagnostic_text_is_well_formed(
                    type_errors.replace(absolute_source, "/tmp/other.mimi", 1),
                    source_path="case.mimi",
                    root=root,
                )
            )
            self.assertFalse(
                census._source_diagnostic_text_is_well_formed(
                    parse_errors.replace("parse error(s) found", "parse error(s)"),
                    source_path="case.mimi",
                    root=root,
                )
            )

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
                "expected_source_diagnostic",
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
                        "expected_source_diagnostic",
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
            (root / "tests/route_contract.rs").write_text(
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
        self.assertEqual(result["authority_status"], "required_compile_test_contract_mismatch")
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
            self._attach_valid_rejection_artifact_evidence(record, root, "case.mimi")
            gate = StrictGateTests._gate(
                ["case.mimi"],
                [record],
                source_scope={"exactly_once": True},
                expectation_contract={
                    "exact_bijection": True,
                    "duplicate_inventory_paths": [],
                    "invalid_source_hash_paths": [],
                    "invalid_authority_paths": [],
                    "authority_bundle_stable": True,
                },
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

    def test_source_diagnostic_is_source_bound_and_runs_its_exact_cargo_test(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "case.mimi"
            fixture.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(fixture, root)
            entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_source_diagnostic",
                expected_exit_code=1,
            )
            bound, coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            execution = self._execute_authority_tests(
                root,
                [source],
                bound,
                expectation_class="expected_source_diagnostic",
            )
            item = bound["case.mimi"]
            stderr = item["expected_diagnostic_text"].encode("utf-8")
            route = census._route_observation("", stderr.decode(), 1, "completed")
            failure = census._failure_classification(
                run_status="completed",
                exit_code=1,
                text=stderr.decode(),
                target_kind="executable",
                route=route,
            )
            result = census._expectation_result(
                "case.mimi",
                1,
                route,
                bound,
                run_status="completed",
                failure_classification=failure,
                captured_output=stderr,
                captured_stdout=b"",
                captured_stderr=stderr,
                output_artifact=None,
            )
            source_test = (
                root / entry["authority"]["assertion"]["path"]
            ).read_text(encoding="utf-8")

        self.assertEqual(item["source_hash_status"], "matched")
        self.assertTrue(item["test_contract_valid"])
        self.assertTrue(item["source_authority_valid"])
        self.assertTrue(item["test_execution_valid"])
        self.assertEqual(execution["authority_class"], "expected_source_diagnostic")
        self.assertEqual(execution["status"], "passed")
        self.assertEqual(
            item["test_execution_proof"]["exact_test_run"]["command"][-4:],
            ["test_route_contract", "--", "--exact", "--nocapture"],
        )
        self.assertEqual(
            item["test_execution_proof"]["source_diagnostic_output_path"],
            entry["expected_output_path"],
        )
        self.assertIn("::std::assert_eq!(output.status.code(), Some(1));", source_test)
        self.assertIn(
            "::std::assert_eq!(output.stderr.as_slice(), expected_stderr.as_slice());",
            source_test,
        )
        self.assertNotIn("String::from_utf8_lossy(&output.stderr)", source_test)
        self.assertTrue(result["matched"])
        self.assertEqual(result["class"], "expected_source_diagnostic")
        self.assertEqual(result["actual_route_classification_status"], "inferred_pre_route_rejection")
        self.assertEqual(result["actual_failure_class"], "source_diagnostic")
        self.assertTrue(coverage["exact_bijection"])

    def test_source_diagnostic_authority_compares_utf8_bytes_without_replacement_collision(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "case.mimi"
            fixture.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(fixture, root)
            diagnostic_text = (
                f"{fixture} has 1 type error(s):\n"
                f"error[E0432] {fixture}:2:3-8 unknown name '\uFFFD'\n"
                "error: type checking failed\n"
            )
            output_path = census._source_diagnostic_authority_output_path(
                "case.mimi", source["sha256"]
            )
            authority_source, _assertion_line = (
                census._render_expected_source_diagnostic_test(
                    test_id="test_exact_utf8_diagnostic",
                    source_path="case.mimi",
                    source_sha256=source["sha256"],
                    expected_exit_code=7,
                    target_kind="executable",
                    output_path=output_path,
                    expected_diagnostic_text=diagnostic_text,
                    root=root,
                )
            )
            expected_bytes = diagnostic_text.encode("utf-8")
            invalid_bytes = expected_bytes.replace("\uFFFD".encode("utf-8"), b"\xff", 1)

        self.assertEqual(invalid_bytes.decode("utf-8", errors="replace"), diagnostic_text)
        self.assertNotEqual(invalid_bytes, expected_bytes)
        self.assertIn("::std::assert_eq!(output.status.code(), Some(7));", authority_source)
        self.assertIn(".into_bytes();", authority_source)
        self.assertIn(
            "::std::assert_eq!(output.stderr.as_slice(), expected_stderr.as_slice());",
            authority_source,
        )
        self.assertNotIn("String::from_utf8_lossy(&output.stderr)", authority_source)
        self.assertTrue(expected_bytes == diagnostic_text.encode("utf-8"))

    def test_source_diagnostic_rejects_extra_diagnostics_route_markers_fallback_and_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "case.mimi"
            fixture.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(fixture, root)
            entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_source_diagnostic",
                expected_exit_code=1,
            )
            bound, _coverage = self._load_and_bind(root, {"case.mimi": entry}, [source])
            self._execute_authority_tests(
                root,
                [source],
                bound,
                expectation_class="expected_source_diagnostic",
            )
            item = bound["case.mimi"]
            expected_text = item["expected_diagnostic_text"].encode("utf-8")
            cases = {
                "extra_diagnostic": (
                    expected_text
                    + f"error[E9999] {fixture}:8:1-2 unexpected extra error\n".encode()
                , b"", None),
                "route_marker": (
                    expected_text + b"default Canonical MIR route rejected: wrong\n",
                    b"", None),
                "fallback_marker": (
                    expected_text + b"warning: legacy fallback used\n",
                    b"", None),
                "stdout_diagnostic": (expected_text, b"unexpected stdout\n", None),
                "output_artifact": (expected_text, b"", {"sha256": "f" * 64, "bytes": 1}),
            }
            for label, (stderr_bytes, stdout_bytes, output_artifact) in cases.items():
                with self.subTest(label=label):
                    route = census._route_observation(
                        stdout_bytes.decode(), stderr_bytes.decode(), 1, "completed"
                    )
                    failure = census._failure_classification(
                        run_status="completed",
                        exit_code=1,
                        text=stderr_bytes.decode() + stdout_bytes.decode(),
                        target_kind="executable",
                        route=route,
                    )
                    result = census._expectation_result(
                        "case.mimi",
                        1,
                        route,
                        bound,
                        run_status="completed",
                        failure_classification=failure,
                        captured_output=stdout_bytes + b"\n" + stderr_bytes,
                        captured_stdout=stdout_bytes,
                        captured_stderr=stderr_bytes,
                        output_artifact=output_artifact,
                    )
                    self.assertFalse(result["matched"])

    def test_source_diagnostic_schema_rejects_route_rejection_semantics_and_stale_hashes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "case.mimi"
            fixture.write_text("func main() { }\n", encoding="utf-8")
            source = census.source_record(fixture, root)
            entry = self._entry(
                root,
                "case.mimi",
                source["sha256"],
                expectation_class="expected_source_diagnostic",
                expected_exit_code=1,
            )
            expectation_path = root / "expectations.json"
            expectation_path.write_text(
                json.dumps({"schema_version": census.EXPECTATION_SCHEMA_VERSION,
                            "inputs": {"case.mimi": {**entry, "expected_failure_class": "capability"}}}),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "cannot claim observed route-rejection"):
                census.load_expectations(expectation_path, root)

            wrong_diagnostic_source = json.loads(json.dumps(entry))
            wrong_diagnostic_source["expected_diagnostic_text"] = (
                entry["expected_diagnostic_text"].replace(
                    str(fixture), str(root / "elsewhere.mimi")
                )
            )
            wrong_diagnostic_source["expected_diagnostic_lines"] = (
                census._source_diagnostic_lines(
                    wrong_diagnostic_source["expected_diagnostic_text"]
                )
            )
            expectation_path.write_text(
                json.dumps({"schema_version": census.EXPECTATION_SCHEMA_VERSION,
                            "inputs": {"case.mimi": wrong_diagnostic_source}}),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "bound to its exact input"):
                census.load_expectations(expectation_path, root)

            forged_output_path = json.loads(json.dumps(entry))
            forged_output_path["expected_output_path"] = (
                "target/route-census-source-diagnostic-forged.out"
            )
            expectation_path.write_text(
                json.dumps({"schema_version": census.EXPECTATION_SCHEMA_VERSION,
                            "inputs": {"case.mimi": forged_output_path}}),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "deterministic isolated source-diagnostic path"):
                census.load_expectations(expectation_path, root)

            forged_fixture_path = json.loads(json.dumps(entry))
            assertion_file = root / forged_fixture_path["authority"]["assertion"]["path"]
            assertion_text = assertion_file.read_text(encoding="utf-8")
            self.assertIn('.join("case.mimi")', assertion_text)
            assertion_file.write_text(
                assertion_text.replace(
                    '.join("case.mimi")', '.join("elsewhere.mimi")', 1
                ),
                encoding="utf-8",
            )
            forged_fixture_path["authority"]["assertion"]["sha256"] = (
                census.sha256_file(assertion_file)
            )
            expectation_path.write_text(
                json.dumps({"schema_version": census.EXPECTATION_SCHEMA_VERSION,
                            "inputs": {"case.mimi": forged_fixture_path}}),
                encoding="utf-8",
            )
            declared_fixture_path, _identity = census.load_expectations(
                expectation_path, root
            )
            bound_fixture_path, coverage_fixture_path = (
                census.bind_expectations_to_inventory(
                    declared_fixture_path, [source], root
                )
            )
            self.assertFalse(bound_fixture_path["case.mimi"]["test_contract_valid"])
            self.assertFalse(bound_fixture_path["case.mimi"]["source_authority_valid"])
            self.assertEqual(
                coverage_fixture_path["invalid_authority_paths"], ["case.mimi"]
            )

            stale = dict(entry)
            stale_sha = "0" * 64
            stale["source_sha256"] = stale_sha
            stale_output_path = census._source_diagnostic_authority_output_path(
                "case.mimi", stale_sha
            )
            stale_source, stale_assertion_line = (
                census._render_expected_source_diagnostic_test(
                    test_id=stale["authority"]["test_id"],
                    source_path="case.mimi",
                    source_sha256=stale_sha,
                    expected_exit_code=stale["expected_exit_code"],
                    target_kind="executable",
                    output_path=stale_output_path,
                    expected_diagnostic_text=stale["expected_diagnostic_text"],
                    root=root,
                )
            )
            assertion_file = root / stale["authority"]["assertion"]["path"]
            assertion_file.write_text(stale_source, encoding="utf-8")
            stale["authority"]["assertion"]["line"] = stale_assertion_line
            stale["authority"]["assertion"]["sha256"] = census.sha256_file(
                assertion_file
            )
            stale["expected_output_path"] = stale_output_path
            expectation_path.write_text(
                json.dumps({"schema_version": census.EXPECTATION_SCHEMA_VERSION,
                            "inputs": {"case.mimi": stale}}),
                encoding="utf-8",
            )
            declared, _identity = census.load_expectations(expectation_path, root)
            bound, coverage = census.bind_expectations_to_inventory(declared, [source], root)
            self.assertEqual(bound["case.mimi"]["source_hash_status"], "mismatch")
            self.assertTrue(bound["case.mimi"]["source_authority_valid"])
            self.assertFalse(bound["case.mimi"]["authority_valid"])
            self.assertEqual(coverage["invalid_source_hash_paths"], ["case.mimi"])

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
                "missing_route_telemetry": valid_text.replace(
                    '        .env("MIMI_ROUTE_CENSUS", "1")\n', ""
                ),
                "wrong_route_telemetry_value": valid_text.replace(
                    '.env("MIMI_ROUTE_CENSUS", "1")',
                    '.env("MIMI_ROUTE_CENSUS", "true")',
                ),
                "duplicate_route_telemetry": valid_text.replace(
                    '        .env("MIMI_ROUTE_CENSUS", "1")\n',
                    '        .env("MIMI_ROUTE_CENSUS", "1")\n'
                    '        .env("MIMI_ROUTE_CENSUS", "1")\n',
                ),
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
            mimi, mimi_sha256 = _write_test_mimi(root)

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
                    mimi=mimi,
                    mimi_sha256=mimi_sha256,
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
            mimi, mimi_sha256 = _write_test_mimi(root)
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
                    mimi=mimi,
                    mimi_sha256=mimi_sha256,
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
            mimi, mimi_sha256 = _write_test_mimi(root)

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
                    mimi=mimi,
                    mimi_sha256=mimi_sha256,
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
    def test_collect_forwards_its_authority_mimi_identity_to_the_production_gate_call(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "case.mimi"
            fixture.write_text("func main() { }\n", encoding="utf-8")
            mimi = root / "mimi"
            mimi.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            mimi.chmod(0o755)
            mimi = mimi.resolve(strict=True)
            mimi_sha256 = census.sha256_file(mimi)
            source = census.source_record(fixture, root)
            tracked_inventory = {
                "status": "available",
                "path_count": 0,
                "paths_sha256": census._manifest_paths_hash([]),
            }
            cargo_authority = {"identity_sha256": "d" * 64, "cargo": {}, "rustc": {}}
            expectation_contract = {
                "exact_bijection": True,
                "missing_paths": [],
                "extra_paths": [],
                "duplicate_inventory_paths": [],
                "invalid_source_hash_paths": [],
                "invalid_authority_paths": [],
            }
            expectations = {source["path"]: {"class": "unmapped"}}
            record = {
                **source,
                "input_index": 1,
                "candidate_classification": "unmapped",
                "run_status": "completed",
                "failure_classification": {"class": "none"},
                "route": {"disposition": "unobserved", "classification_status": "not_run"},
                "expectation": {"class": "unmapped", "matched": False},
                "per_function": {"named_roster_complete": False},
                "source_stable_for_input": True,
            }
            args = census.parse_args([
                "--root", str(root),
                "--mimi", str(mimi),
                "--output", str(root / "report.json"),
                "--artifacts", str(root / "artifacts"),
            ])

            with (
                patch.object(census, "tracked_mimi_paths", return_value=([], tracked_inventory)),
                patch.object(
                    census,
                    "enumerate_source_scope",
                    return_value=([source], [], {"exactly_once": True}),
                ),
                patch.object(census, "enumerate_sources", return_value=[source]),
                patch.object(census, "source_symlink_exclusions", return_value=[]),
                patch.object(census, "tracked_symlink_parent_blockers", return_value=[]),
                patch.object(census, "source_snapshot", return_value={"snapshot_id": "fixed"}),
                patch.object(census, "actor_fixture_test_contracts", return_value={}),
                patch.object(census, "load_expectations", return_value=({}, {"path": None, "sha256": None})),
                patch.object(
                    census,
                    "bind_expectations_to_inventory",
                    return_value=(expectations, expectation_contract),
                ),
                patch.object(census, "cargo_test_toolchain_authority", return_value=cargo_authority),
                patch.object(
                    census,
                    "toolchain_manifest",
                    return_value={
                        "mimi": {"path": str(mimi), "sha256": mimi_sha256},
                        "cargo": {},
                        "rustc": {},
                    },
                ),
                patch.object(census, "repository_identity", return_value={}),
                patch.object(census, "_collect_legacy_owner_audit", return_value={}),
                patch.object(
                    census,
                    "_run_actor_authority_tests",
                    return_value={"status": "stubbed", "cases": []},
                ),
                patch.object(
                    census,
                    "_run_expected_rejection_authority_tests",
                    return_value={"status": "stubbed", "cases": []},
                ),
                patch.object(
                    census,
                    "_run_source_diagnostic_authority_tests",
                    return_value={"status": "stubbed", "cases": []},
                ),
                patch.object(
                    census,
                    "_run_required_compile_authority_tests",
                    return_value={"status": "stubbed", "cases": []},
                ),
                patch.object(
                    census,
                    "_actor_fallback_evidence",
                    return_value={"missing_cases": []},
                ),
                patch.object(
                    census,
                    "_run_bounded_parallel_ordered",
                    return_value=([record], False),
                ),
                patch.object(
                    census,
                    "_gate_report",
                    return_value={"passed": False, "checks": {}},
                ) as gate_report,
            ):
                status = census.collect(args)
                report = json.loads((root / "report.json").read_text(encoding="utf-8"))

        self.assertEqual(status, 0)
        self.assertEqual(gate_report.call_count, 1)
        self.assertEqual(
            gate_report.call_args.kwargs["mimi_identity"],
            report["authority_mimi_identity"],
        )
        self.assertEqual(report["authority_mimi_identity"]["path"], str(mimi))
        self.assertEqual(report["authority_mimi_identity"]["sha256"], mimi_sha256)

    def test_authority_bundle_can_be_paired_with_independent_expectations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            args = census.parse_args([
                "--root", str(root),
                "--mimi", str(root / "mimi"),
                "--output", str(root / "report.json"),
                "--authority-bundle", str(root / "bundle"),
                "--expectations", str(root / "expectations-v4.json"),
                "--check",
            ])

        self.assertTrue(args.check)
        self.assertTrue(args.authority_bundle)
        self.assertTrue(args.expectations)

    def test_strict_check_rejects_zero_blocker_bootstrap_authority_bundle(self) -> None:
        with self.assertRaisesRegex(ValueError, "bootstrap_observation bundles are observation-only"):
            census._require_strict_authority_bundle_for_check(
                {
                    "generation_mode": "bootstrap_observation",
                    "bootstrap_blocker_count": 0,
                },
                check=True,
            )

    def test_collect_check_rejects_bootstrap_bundle_before_authority_tests(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "case.mimi"
            fixture.write_text("func main() { }\n", encoding="utf-8")
            mimi = root / "mimi"
            mimi.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            mimi.chmod(0o755)
            source = census.source_record(fixture, root)
            args = census.parse_args([
                "--root", str(root),
                "--mimi", str(mimi),
                "--output", str(root / "check.json"),
                "--authority-bundle", str(root / "bundle"),
                "--check",
            ])
            with (
                patch.object(census, "tracked_mimi_paths", return_value=([], {"status": "available"})),
                patch.object(
                    census,
                    "enumerate_source_scope",
                    return_value=([source], [], {"exactly_once": True}),
                ),
                patch.object(
                    census,
                    "_validate_route_authority_bundle",
                    return_value={
                        "generation_mode": "bootstrap_observation",
                        "bootstrap_blocker_count": 0,
                    },
                ),
                patch.object(
                    census,
                    "_run_actor_authority_tests",
                    side_effect=AssertionError("bootstrap bundle must be rejected before Cargo"),
                ) as authority_tests,
            ):
                with self.assertRaisesRegex(ValueError, "bootstrap_observation bundles are observation-only"):
                    census.collect(args)

        authority_tests.assert_not_called()

    def test_non_check_observation_bundle_remains_observation_only(self) -> None:
        census._require_strict_authority_bundle_for_check(
            {"generation_mode": "bootstrap_observation", "bootstrap_blocker_count": 0},
            check=False,
        )

    def test_external_expectations_must_match_bundle_class_and_source_hash(self) -> None:
        sources = [{"path": "case.mimi", "sha256": "a" * 64}]
        caller = {"case.mimi": {"class": "required_compile", "source_sha256": "a" * 64}}
        generated = {"case.mimi": {"class": "required_compile", "source_sha256": "a" * 64}}
        census._validate_external_expectations_against_bundle(caller, generated, sources)

        generated["case.mimi"]["class"] = "expected_build_rejection"
        with self.assertRaisesRegex(ValueError, "not grounded in independent expectations"):
            census._validate_external_expectations_against_bundle(caller, generated, sources)

    def test_report_expectations_identity_matches_exact_path_and_digest(self) -> None:
        sha256 = "b" * 64
        report = {
            "expectations": {
                "identity": {
                    "provided_manifest": {"path": "/tmp/expectations-v4.json", "sha256": sha256}
                }
            }
        }
        self.assertTrue(census._provided_expectations_identity_matches(
            report,
            expected_path="/tmp/expectations-v4.json",
            expected_sha256=sha256,
        ))
        self.assertFalse(census._provided_expectations_identity_matches(
            report,
            expected_path="/tmp/other-expectations-v4.json",
            expected_sha256=sha256,
        ))
        self.assertFalse(census._provided_expectations_identity_matches(
            report,
            expected_path="/tmp/expectations-v4.json",
            expected_sha256="c" * 64,
        ))
        self.assertFalse(census._provided_expectations_identity_matches(
            {"expectations": {"identity": {"provided_manifest": None}}},
            expected_path="/tmp/expectations-v4.json",
            expected_sha256=sha256,
        ))

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
        self.assertEqual(
            report["build_provenance"]["status"], "caller_supplied_unverified"
        )
        self.assertFalse(report["build_provenance"]["current_source_attested"])
        self.assertFalse(report["gate"]["current_source_attested"])

    def test_jobs_argument_is_bounded_and_defaults_to_four(self) -> None:
        base = ["--output", "report.json"]
        self.assertEqual(census.parse_args(base).jobs, 4)
        self.assertEqual(census.parse_args(base + ["--jobs", "8"]).jobs, 8)
        with self.assertRaises(SystemExit):
            census.parse_args(base + ["--jobs", "9"])


if __name__ == "__main__":
    unittest.main()
