"""Contract tests for schema-v5 special direct-executor authorities."""

from __future__ import annotations

import hashlib
import importlib.util
from pathlib import Path, PurePosixPath
import sys
import tempfile
import unittest
from typing import Any


sys.dont_write_bytecode = True
REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_DIR = REPOSITORY_ROOT / "scripts"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))
RENDERER_PATH = SCRIPTS_DIR / "render_independent_route_authorities.py"
SPEC = importlib.util.spec_from_file_location("mimi_special_route_renderer_test", RENDERER_PATH)
assert SPEC is not None and SPEC.loader is not None
renderer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(renderer)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class SpecialRouteAuthorityContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="mimi-special-route-contract-")
        self.root = Path(self.temp.name)
        self.input_path = "fixtures/linear_case.mimi"
        self.tool_path = "tools/mimi_lint.mimi"
        self.executable_path = str((self.root / "bin" / "mimi").resolve())
        self.input_raw = b"func main() -> i32 { 0 }\n"
        self.tool_raw = b"func main() { println(1) }\n"
        (self.root / "fixtures").mkdir()
        (self.root / "tools").mkdir()
        (self.root / self.input_path).write_bytes(self.input_raw)
        (self.root / self.tool_path).write_bytes(self.tool_raw)
        self.input_sha = sha256(self.input_raw)
        self.tool_sha = sha256(self.tool_raw)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def tool_contract(self) -> dict[str, Any]:
        stdout = ""
        stderr = "tool reported an independent diagnostic\n"
        return {
            "schema": "mimi-tool-diagnostic-contract-v1",
            "contract_id": "tool-contract-linear-case",
            "status": "bound",
            "input": {"path": self.input_path, "source_sha256": self.input_sha},
            "command": {
                "argv": ["<CURRENT_MIMI_EXECUTABLE>", "run", self.tool_path, self.input_path],
                "cwd": ".",
            },
            "tool": {
                "source_path": self.tool_path,
                "source_sha256": self.tool_sha,
                "executable_path": self.executable_path,
                "executable_sha256": "a" * 64,
            },
            "expected": {
                "exit_code": 1,
                "stdout_utf8": stdout,
                "stdout_sha256": sha256(stdout.encode("utf-8")),
                "stderr_utf8": stderr,
                "stderr_sha256": sha256(stderr.encode("utf-8")),
                "full_output_captured": True,
            },
            "blockers": [],
        }

    def tool_expectation(self, contract: dict[str, Any] | None = None) -> dict[str, Any]:
        contract = self.tool_contract() if contract is None else contract
        return {
            "class": "expected_tool_diagnostic",
            "reason": "independent tool diagnostics are pinned to exact bytes",
            "source_sha256": self.input_sha,
            "contract_id": contract["contract_id"],
            "tool_diagnostic_contract": contract,
        }

    def canonical_mir_contract(self) -> dict[str, Any]:
        return {
            "schema": "mimi-canonical-mir-input-rejection-contract-v1",
            "status": "bound",
            "contract_id": "canonical-mir-rejection-linear-case",
            "input": {"path": self.input_path, "source_sha256": self.input_sha},
            "command": {
                "argv": ["<CURRENT_MIMI_EXECUTABLE>", "verify", self.input_path, "--mir"],
                "cwd": ".",
            },
            "assertions": {
                "exit_nonzero": True,
                "merged_output_contains": [
                    "canonical MIR verifier input rejected",
                    "use after consuming non-Copy value",
                ],
                "merged_output_excludes": ["flow_ast"],
            },
        }

    def canonical_mir_expectation(
        self, contract: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        contract = self.canonical_mir_contract() if contract is None else contract
        return {
            "class": "canonical_mir_input_rejection",
            "reason": "the canonical MIR verifier rejects the independently pinned input",
            "source_sha256": self.input_sha,
            "contract_id": contract["contract_id"],
            "canonical_mir_input_rejection_contract": contract,
        }

    def assert_tool_contract_valid(self, contract: dict[str, Any]) -> None:
        expectation = self.tool_expectation(contract)
        renderer._validate_tool_diagnostic_contract(
            root=self.root,
            path=self.input_path,
            source_sha256=self.input_sha,
            expectation=expectation,
        )
        renderer._validate_expected_tool_diagnostic_contract(
            self.input_path, self.input_sha, expectation
        )

    def assert_canonical_mir_contract_valid(self, contract: dict[str, Any]) -> None:
        renderer._validate_canonical_mir_input_rejection_contract(
            root=self.root,
            path=self.input_path,
            source_sha256=self.input_sha,
            expectation=self.canonical_mir_expectation(contract),
        )

    def test_tool_contract_binds_input_tool_source_executable_and_full_output(self) -> None:
        contract = self.tool_contract()
        self.assert_tool_contract_valid(contract)
        contract["input"]["source_sha256"] = "b" * 64
        with self.assertRaises(renderer.RendererError):
            self.assert_tool_contract_valid(contract)

    def test_tool_contract_rejects_stale_source_hash_and_incomplete_stream_hash(self) -> None:
        contract = self.tool_contract()
        contract["tool"]["source_sha256"] = "b" * 64
        with self.assertRaises(renderer.RendererError):
            self.assert_tool_contract_valid(contract)

        contract = self.tool_contract()
        contract["expected"]["stderr_sha256"] = "c" * 64
        with self.assertRaises(renderer.RendererError):
            self.assert_tool_contract_valid(contract)

    def test_tool_diagnostic_requires_bound_nonzero_exact_tool_command(self) -> None:
        contract = self.tool_contract()
        contract["command"]["argv"][-1] = "fixtures/other.mimi"
        with self.assertRaises(renderer.RendererError):
            self.assert_tool_contract_valid(contract)

        contract = self.tool_contract()
        contract["status"] = "pending_exact_execution"
        with self.assertRaises(renderer.RendererError):
            self.assert_tool_contract_valid(contract)

        contract = self.tool_contract()
        contract["blockers"] = ["executable is not confirmed"]
        with self.assertRaises(renderer.RendererError):
            self.assert_tool_contract_valid(contract)

        contract = self.tool_contract()
        contract["expected"]["exit_code"] = 0
        with self.assertRaises(renderer.RendererError):
            self.assert_tool_contract_valid(contract)

        contract = self.tool_contract()
        contract["tool"]["executable_path"] = "/tmp/../tmp/mimi"
        with self.assertRaises(renderer.RendererError):
            self.assert_tool_contract_valid(contract)

    def test_canonical_mir_contract_pins_bound_verify_command_and_anchors(self) -> None:
        contract = self.canonical_mir_contract()
        self.assert_canonical_mir_contract_valid(contract)
        self.assertEqual(
            set(contract),
            {"schema", "status", "contract_id", "input", "command", "assertions"},
        )
        self.assertEqual(
            contract["command"]["argv"],
            ["<CURRENT_MIMI_EXECUTABLE>", "verify", self.input_path, "--mir"],
        )
        self.assertEqual(
            contract["assertions"]["merged_output_contains"],
            [
                "canonical MIR verifier input rejected",
                "use after consuming non-Copy value",
            ],
        )

    def test_canonical_mir_contract_rejects_stale_input_command_and_unbound_status(self) -> None:
        contract = self.canonical_mir_contract()
        contract["input"]["source_sha256"] = "b" * 64
        with self.assertRaises(renderer.RendererError):
            self.assert_canonical_mir_contract_valid(contract)

        contract = self.canonical_mir_contract()
        contract["command"]["argv"][-1] = "--build"
        with self.assertRaises(renderer.RendererError):
            self.assert_canonical_mir_contract_valid(contract)

        contract = self.canonical_mir_contract()
        contract["command"]["cwd"] = "target"
        with self.assertRaises(renderer.RendererError):
            self.assert_canonical_mir_contract_valid(contract)

        contract = self.canonical_mir_contract()
        contract["status"] = "pending"
        with self.assertRaises(renderer.RendererError):
            self.assert_canonical_mir_contract_valid(contract)

        contract = self.canonical_mir_contract()
        contract["contract_id"] = " "
        with self.assertRaises(renderer.RendererError):
            self.assert_canonical_mir_contract_valid(contract)

    def test_canonical_mir_contract_requires_only_the_pinned_nonzero_output_claims(self) -> None:
        mutations = (
            lambda contract: contract["assertions"].update(exit_nonzero=False),
            lambda contract: contract["assertions"]["merged_output_contains"].pop(),
            lambda contract: contract["assertions"]["merged_output_excludes"].append("other"),
            lambda contract: contract["assertions"].update(extra=True),
            lambda contract: contract.update(stages={"check": {}, "build": {}}),
        )
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                contract = self.canonical_mir_contract()
                mutate(contract)
                with self.assertRaises(renderer.RendererError):
                    self.assert_canonical_mir_contract_valid(contract)

        contract = self.canonical_mir_contract()
        contract["assertions"]["merged_output_excludes"] = ["FLOW_AST"]
        with self.assertRaises(renderer.RendererError):
            self.assert_canonical_mir_contract_valid(contract)

    def test_canonical_mir_contract_rejects_outer_contract_id_mismatch(self) -> None:
        expectation = self.canonical_mir_expectation()
        expectation["contract_id"] = "another-contract"
        with self.assertRaises(renderer.RendererError):
            renderer._validate_canonical_mir_input_rejection_contract(
                root=self.root,
                path=self.input_path,
                source_sha256=self.input_sha,
                expectation=expectation,
            )

    def test_special_executor_authority_binds_contract_and_test_source_without_execution(self) -> None:
        expected_identity = sha256(f"{self.input_path}\0{self.input_sha}".encode("utf-8"))[:20]
        cases = (
            ("expected_tool_diagnostic", self.tool_contract(), "tool_diagnostic"),
            (
                "canonical_mir_input_rejection",
                self.canonical_mir_contract(),
                "canonical_mir_input_rejection",
            ),
        )
        for semantic_class, contract, suffix in cases:
            with self.subTest(semantic_class=semantic_class):
                authority = renderer._special_executor_authority(
                    semantic_class=semantic_class,
                    path=self.input_path,
                    source_sha256=self.input_sha,
                    contract=contract,
                    collector_sha256="d" * 64,
                )
                self.assertEqual(
                    authority["executor_id"],
                    f"full_route_census.{semantic_class}_executor.v1",
                )
                self.assertEqual(
                    authority["test_id"],
                    f"route_census_{suffix}_{expected_identity}",
                )
                self.assertEqual(authority["test_path"], "scripts/full_route_census.py")
                self.assertEqual(authority["test_sha256"], "d" * 64)
                self.assertEqual(authority["execution_status"], "not_run")
                self.assertEqual(authority["contract_id"], contract["contract_id"])
                self.assertEqual(
                    authority["contract_sha256"],
                    renderer._canonical_json_sha256(contract),
                )
                self.assertEqual(
                    authority["input"],
                    {"path": self.input_path, "source_sha256": self.input_sha},
                )
                self.assertEqual(
                    authority["execution_receipt_path"],
                    f"target/route-census-{semantic_class}-{expected_identity}.json",
                )
                expected_assertions = (
                    [
                        "tool_source_sha256",
                        "input_sha256",
                        "current_mimi_executable_sha256",
                        "argv",
                        "exit_code",
                        "stdout_bytes",
                        "stderr_bytes",
                    ]
                    if semantic_class == "expected_tool_diagnostic"
                    else [
                        "verify_exit_nonzero",
                        "merged_output_contains_anchors",
                        "merged_output_excludes_flow_ast",
                    ]
                )
                self.assertEqual(authority["assertions"], expected_assertions)
        self.assertEqual(renderer.OUTPUT_EXPECTATIONS_SCHEMA_VERSION, 5)

    def test_exact_coverage_recognizes_special_classes_with_hash_bound_contracts(self) -> None:
        test_root = self.root

        class Census:
            ROOT = test_root
            EXPECTATION_CLASSES = {"required_compile"}

            @staticmethod
            def _repository_relative_path(value: Any, *, label: str) -> str:
                if not isinstance(value, str):
                    raise ValueError(label)
                candidate = PurePosixPath(value)
                if candidate.is_absolute() or candidate.as_posix() != value:
                    raise ValueError(label)
                return value

        sources = [{"path": self.input_path, "sha256": self.input_sha}]
        for expectation_class, expectation in (
            ("canonical_mir_input_rejection", self.canonical_mir_expectation()),
            ("expected_tool_diagnostic", self.tool_expectation()),
        ):
            with self.subTest(expectation_class=expectation_class):
                input_record = {
                    "schema_version": 1,
                    "inputs": {self.input_path: expectation},
                }
                validated = renderer._validate_exact_input_coverage(input_record, sources, Census)
                self.assertEqual(validated[self.input_path]["class"], expectation_class)

        obsolete_checker_expectation = {
            "class": "expected_checker_diagnostic",
            "reason": "legacy checker contract must not map into schema v5",
            "source_sha256": self.input_sha,
        }
        with self.assertRaises(renderer.RendererError):
            renderer._validate_exact_input_coverage(
                {
                    "schema_version": 1,
                    "inputs": {self.input_path: obsolete_checker_expectation},
                },
                sources,
                Census,
            )


if __name__ == "__main__":
    unittest.main()
