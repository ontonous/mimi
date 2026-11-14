from __future__ import annotations

import copy
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
import full_route_census as census


def _hash(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _write(path: Path, data: bytes, *, executable: bool = False) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    if executable:
        path.chmod(0o755)
    return _hash(data)


def _fixture_root(root: Path, source_path: str = "fixtures/tool-input.mimi") -> dict[str, str]:
    _write(root / census.ROUTE_CENSUS_COLLECTOR_PATH, b"collector source for schema-v5 tests\n")
    source_hash = _write(root / source_path, b"func main() { println(1) }\n")
    tool_path = "projects/mimi-lint/src/main.mimi"
    tool_hash = _write(root / tool_path, b"func main() { println(\"lint\") }\n")
    return {"source_path": source_path, "source_sha256": source_hash,
            "tool_path": tool_path, "tool_sha256": tool_hash}


def _direct_authority(
    root: Path,
    expectation_class: str,
    source_path: str,
    source_sha256: str,
    contract: dict,
) -> dict:
    identity = census._source_contract_identity_sha20(source_path, source_sha256)
    slug = expectation_class.removeprefix("expected_")
    assertions = (
        census.TOOL_DIAGNOSTIC_EXECUTOR_ASSERTIONS
        if expectation_class == "expected_tool_diagnostic"
        else census.CANONICAL_MIR_INPUT_REJECTION_EXECUTOR_ASSERTIONS
    )
    return {
        "schema": "mimi-direct-executor-authority-v1",
        "executor_id": f"full_route_census.{expectation_class}_executor.v1",
        "test_id": f"route_census_{slug}_{identity}",
        "test_path": census.ROUTE_CENSUS_COLLECTOR_PATH,
        "test_sha256": census._stable_regular_file_sha256(
            root / census.ROUTE_CENSUS_COLLECTOR_PATH
        ),
        "contract_id": contract["contract_id"],
        "contract_sha256": census._contract_sha256(contract),
        "input": {"path": source_path, "source_sha256": source_sha256},
        "assertions": list(assertions),
        "execution_receipt_path": (
            f"target/route-census-{expectation_class}-{identity}.json"
        ),
        "execution_status": "not_run",
    }


def _pending_tool_contract(identity: dict[str, str]) -> dict:
    return {
        "schema": "mimi-tool-diagnostic-contract-v1",
        "contract_id": "tool-row-pending-v1",
        "status": "pending_exact_execution",
        "execution_status": "not_run",
        "accepted": False,
        "input": {"path": identity["source_path"], "source_sha256": identity["source_sha256"]},
        "command": {
            "argv": [
                "<CURRENT_MIMI_EXECUTABLE>", "run", identity["tool_path"], identity["source_path"]
            ],
            "cwd": ".",
            "env": {"MIMI_VERBOSE": None, "MIMI_ROUTE_CENSUS": None},
        },
        "tool": {
            "source_path": identity["tool_path"],
            "source_sha256": identity["tool_sha256"],
            "executable_path": None,
            "executable_sha256": None,
            "executable_source": {
                "required_source": "frozen_build_receipt",
                "binding_status": "pending",
                "receipt_path": None,
                "receipt_sha256": None,
                "executable_path": None,
                "executable_sha256": None,
            },
        },
        "expected": {
            "exit_code": None,
            "stdout_utf8": None,
            "stdout_sha256": None,
            "stderr_utf8": None,
            "stderr_sha256": None,
            "full_output_captured": False,
        },
        "source_adjudication": {
            "conditional_expected_output": {"evidence_status": "source_derived_candidate_not_observed"}
        },
        "blockers": ["exact output and built executable identity remain unbound"],
    }


def _canonical_contract(identity: dict[str, str]) -> dict:
    return {
        "schema": "mimi-canonical-mir-input-rejection-contract-v1",
        "status": "bound",
        "contract_id": "canonical-mir-rejection-v1",
        "input": {"path": identity["source_path"], "source_sha256": identity["source_sha256"]},
        "command": {
            "argv": [
                "<CURRENT_MIMI_EXECUTABLE>", "verify", identity["source_path"], "--mir"
            ],
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


class RouteCensusSchemaV5Tests(unittest.TestCase):
    def test_report_and_expectation_schemas_are_deliberately_versioned(self) -> None:
        self.assertEqual(census.EXPECTATION_SCHEMA_VERSION, 5)
        self.assertEqual(census.SCHEMA_VERSION, "0.1.11-full-route-census-v9")
        self.assertIn("expected_tool_diagnostic", census.EXPECTATION_CLASSES)
        self.assertIn("canonical_mir_input_rejection", census.EXPECTATION_CLASSES)
        self.assertNotIn("expected_checker_diagnostic", census.EXPECTATION_CLASSES)

    def test_schema_four_expectations_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "expectations.json"
            path.write_text('{"schema_version":4,"inputs":{}}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "schema_version: 5"):
                census.load_expectations(path, Path(temporary))

    def test_pending_tool_contract_stays_pending_and_ignores_candidate_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = _fixture_root(root)
            contract = _pending_tool_contract(identity)
            authority = _direct_authority(
                root, "expected_tool_diagnostic", identity["source_path"],
                identity["source_sha256"], contract
            )
            entry = {
                "class": "expected_tool_diagnostic",
                "reason": "mimi-lint emits its pinned diagnostic stream",
                "semantic_class": "expected_tool_diagnostic",
                "contract_id": contract["contract_id"],
                "source_sha256": identity["source_sha256"],
                "tool_diagnostic_contract": contract,
                "tool_diagnostic_authority": authority,
            }
            manifest = root / "expectations.json"
            manifest.write_text(
                json.dumps({"schema_version": 5, "inputs": {identity["source_path"]: entry}}),
                encoding="utf-8",
            )
            loaded, _ = census.load_expectations(manifest, root)

        expectation = loaded[identity["source_path"]]
        self.assertEqual(expectation["class"], "expected_tool_diagnostic")
        self.assertEqual(expectation["reason"], entry["reason"])
        self.assertEqual(expectation["tool_diagnostic_contract"]["status"], "pending_exact_execution")
        self.assertIsNone(expectation["tool_diagnostic_contract"]["expected"]["stdout_utf8"])
        self.assertIsNone(expectation["tool_diagnostic_contract"]["expected"]["exit_code"])
        self.assertIsNone(expectation["authority"])

    def test_pending_tool_contract_rejects_any_derived_expected_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = _fixture_root(root)
            contract = _pending_tool_contract(identity)
            contract["expected"]["stdout_utf8"] = "unobserved candidate"
            with self.assertRaisesRegex(ValueError, "must not invent expected output"):
                census._validate_tool_diagnostic_contract(
                    root, identity["source_path"], identity["source_sha256"], contract
                )

    def test_tool_contract_rejects_stale_input_and_tool_hashes_or_wrong_command(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = _fixture_root(root)
            for field, value in (
                ("source", "0" * 64),
                ("tool", "f" * 64),
                ("argv", ["<CURRENT_MIMI_EXECUTABLE>", "check", identity["tool_path"], identity["source_path"]]),
            ):
                contract = _pending_tool_contract(identity)
                if field == "source":
                    contract["input"]["source_sha256"] = value
                elif field == "tool":
                    contract["tool"]["source_sha256"] = value
                else:
                    contract["command"]["argv"] = value
                with self.subTest(field=field), self.assertRaises(ValueError):
                    census._validate_tool_diagnostic_contract(
                        root, identity["source_path"], identity["source_sha256"], contract
                    )

    def test_bound_tool_contract_pins_exact_outputs_and_current_executable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = _fixture_root(root)
            executable = root / "target/debug/mimi"
            executable_sha = _write(executable, b"#!/bin/sh\nexit 0\n", executable=True)
            receipt_path = root / "evidence/build-receipt.json"
            receipt_sha = _write(receipt_path, b"pinned source-bound build receipt\n")
            stdout = "exact diagnostic\n"
            stderr = ""
            contract = _pending_tool_contract(identity)
            contract.update({"status": "bound", "blockers": []})
            contract["tool"].update({
                "executable_path": str(executable),
                "executable_sha256": executable_sha,
                "executable_source": {
                    "required_source": "frozen_build_receipt",
                    "binding_status": "bound",
                    "receipt_path": str(receipt_path),
                    "receipt_sha256": receipt_sha,
                    "executable_path": str(executable),
                    "executable_sha256": executable_sha,
                },
            })
            contract["expected"] = {
                "exit_code": 7,
                "stdout_utf8": stdout,
                "stdout_sha256": _hash(stdout.encode()),
                "stderr_utf8": stderr,
                "stderr_sha256": _hash(b""),
                "full_output_captured": True,
            }
            with patch.object(census, "_frozen_mimi_build_receipt_is_valid", return_value=True):
                validated = census._validate_tool_diagnostic_contract(
                    root, identity["source_path"], identity["source_sha256"], contract
                )
            self.assertEqual(validated["expected"]["stdout_utf8"], stdout)
            self.assertEqual(validated["expected"]["exit_code"], 7)

            contract["expected"]["stdout_sha256"] = "0" * 64
            with patch.object(census, "_frozen_mimi_build_receipt_is_valid", return_value=True):
                with self.assertRaises(ValueError):
                    census._validate_tool_diagnostic_contract(
                        root, identity["source_path"], identity["source_sha256"], contract
                    )

    def test_canonical_mir_contract_accepts_only_the_verify_input_rejection_shape(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = _fixture_root(root)
            contract = _canonical_contract(identity)
            self.assertEqual(
                census._canonical_mir_input_rejection_contract_is_valid(
                    root, identity["source_path"], identity["source_sha256"], contract
                ),
                contract,
            )
            for mutation in ("argv", "anchors", "exclude", "hash"):
                changed = copy.deepcopy(contract)
                if mutation == "argv":
                    changed["command"]["argv"][1] = "check"
                elif mutation == "anchors":
                    changed["assertions"]["merged_output_contains"].pop()
                elif mutation == "exclude":
                    changed["assertions"]["merged_output_excludes"] = []
                else:
                    changed["input"]["source_sha256"] = "0" * 64
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    census._canonical_mir_input_rejection_contract_is_valid(
                        root, identity["source_path"], identity["source_sha256"], changed
                    )

    def test_direct_authority_binds_contract_hash_test_hash_class_and_receipt_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = _fixture_root(root)
            contract = _canonical_contract(identity)
            authority = _direct_authority(
                root, "canonical_mir_input_rejection", identity["source_path"],
                identity["source_sha256"], contract
            )
            self.assertTrue(census._diagnostic_executor_authority_is_valid(
                root,
                expectation_class="canonical_mir_input_rejection",
                source_path=identity["source_path"],
                source_sha256=identity["source_sha256"],
                contract=contract,
                authority=authority,
            ))
            for field in ("contract_sha256", "test_sha256", "execution_receipt_path", "test_id"):
                changed = copy.deepcopy(authority)
                changed[field] = "0" * 64 if field.endswith("sha256") else "wrong"
                with self.subTest(field=field):
                    self.assertFalse(census._diagnostic_executor_authority_is_valid(
                        root,
                        expectation_class="canonical_mir_input_rejection",
                        source_path=identity["source_path"],
                        source_sha256=identity["source_sha256"],
                        contract=contract,
                        authority=changed,
                    ))

    def test_special_classes_remain_distinct_route_candidates(self) -> None:
        for special_class in ("expected_tool_diagnostic", "canonical_mir_input_rejection"):
            self.assertEqual(
                census._route_candidate_classification(
                    {"disposition": "unobserved"}, {"class": special_class}
                ),
                special_class,
            )

    def test_direct_executor_lanes_defer_until_strict_replay(self) -> None:
        for special_class in ("expected_tool_diagnostic", "canonical_mir_input_rejection"):
            with self.subTest(expectation_class=special_class), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source_path = f"fixtures/{special_class}.mimi"
                identity = _fixture_root(root, source_path)
                mimi = root / "target/debug/mimi"
                mimi_sha = _write(mimi, b"#!/bin/sh\nexit 0\n", executable=True)
                contract = (
                    _pending_tool_contract(identity)
                    if special_class == "expected_tool_diagnostic"
                    else _canonical_contract(identity)
                )
                contract_key = (
                    "tool_diagnostic_contract"
                    if special_class == "expected_tool_diagnostic"
                    else "canonical_mir_input_rejection_contract"
                )
                authority_key = (
                    "tool_diagnostic_authority"
                    if special_class == "expected_tool_diagnostic"
                    else "canonical_mir_input_rejection_authority"
                )
                expectation = {
                    "class": special_class,
                    "reason": "direct lane is deferred during evidence collection",
                    "explicit": True,
                    "source_hash_status": "matched",
                    "source_sha256": identity["source_sha256"],
                    "authority_valid": True,
                    contract_key: contract,
                    authority_key: _direct_authority(
                        root, special_class, source_path, identity["source_sha256"], contract
                    ),
                }
                logs = root / "logs"
                logs.mkdir()
                source = {
                    "path": source_path,
                    "sha256": identity["source_sha256"],
                    "target_kind": "executable",
                }
                with patch.object(
                    census.subprocess,
                    "run",
                    side_effect=AssertionError("evidence collection must defer direct execution"),
                ) as mock_run:
                    record = census._run_direct_diagnostic_one(
                        root=root,
                        source=source,
                        index=0,
                        mimi=mimi,
                        timeout=2,
                        logs_root=logs,
                        expectation=expectation,
                        expectations={source_path: expectation},
                        mimi_sha256=mimi_sha,
                        execute_expectations=False,
                    )

                mock_run.assert_not_called()
                execution_key = (
                    "tool_diagnostic_execution"
                    if special_class == "expected_tool_diagnostic"
                    else "canonical_mir_input_rejection_execution"
                )
                execution = record[execution_key]
                self.assertEqual(record["run_status"], "not_run")
                self.assertIsNone(record["command"])
                self.assertIsNone(record["output_artifact"])
                self.assertEqual(record["expectation"]["class"], special_class)
                self.assertEqual(record["candidate_classification"], special_class)
                self.assertEqual(execution["receipt"]["status"], "not_run")
                self.assertFalse(execution["receipt"]["accepted"])
                self.assertEqual(
                    execution["status"],
                    "not_run_pending_exact_execution"
                    if special_class == "expected_tool_diagnostic"
                    else "not_run_deferred_until_strict_check",
                )

    def test_mocked_tool_executor_checks_exact_output_and_keeps_mimi_route_unobserved(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = _fixture_root(root)
            mimi = root / "target/debug/mimi"
            mimi_sha = _write(mimi, b"#!/bin/sh\nexit 0\n", executable=True)
            receipt_path = root / "evidence/build-receipt.json"
            receipt_sha = _write(receipt_path, b"pinned source-bound build receipt\n")
            contract = _pending_tool_contract(identity)
            contract.update({"status": "bound", "blockers": []})
            contract["tool"].update({
                "executable_path": str(mimi),
                "executable_sha256": mimi_sha,
                "executable_source": {
                    "required_source": "frozen_build_receipt", "binding_status": "bound",
                    "receipt_path": str(receipt_path), "receipt_sha256": receipt_sha,
                    "executable_path": str(mimi), "executable_sha256": mimi_sha,
                },
            })
            stdout, stderr, exit_code = b"exact tool stream\n", b"", 7
            contract["expected"] = {
                "exit_code": exit_code,
                "stdout_utf8": stdout.decode(),
                "stdout_sha256": _hash(stdout),
                "stderr_utf8": "",
                "stderr_sha256": _hash(stderr),
                "full_output_captured": True,
            }
            source_path = identity["source_path"]
            expectation = {
                "class": "expected_tool_diagnostic", "reason": "exact tool output",
                "explicit": True, "source_hash_status": "matched", "source_sha256": identity["source_sha256"],
                "authority_valid": True, "tool_diagnostic_contract": contract,
                "tool_diagnostic_authority": _direct_authority(
                    root, "expected_tool_diagnostic", source_path, identity["source_sha256"], contract
                ),
            }
            logs = root / "logs"
            logs.mkdir()
            source = {"path": source_path, "sha256": identity["source_sha256"], "target_kind": "executable"}
            with patch.object(census, "_frozen_mimi_build_receipt_is_valid", return_value=True), patch.object(
                census.subprocess, "run",
                return_value=SimpleNamespace(stdout=stdout, stderr=stderr, returncode=exit_code),
            ) as mock_run:
                record = census._run_direct_diagnostic_one(
                    root=root, source=source, index=0, mimi=mimi, timeout=2,
                    logs_root=logs, expectation=expectation,
                    expectations={source_path: expectation}, mimi_sha256=mimi_sha,
                )

        self.assertEqual(mock_run.call_args.args[0], [str(mimi), "run", identity["tool_path"], source_path])
        self.assertEqual(record["run_status"], "not_run")
        self.assertEqual(record["route"]["disposition"], "unobserved")
        self.assertEqual(record["expectation"]["class"], "expected_tool_diagnostic")
        self.assertTrue(record["expectation"]["matched"])
        self.assertFalse(record["expectation"]["accepted"])
        self.assertEqual(record["tool_diagnostic_execution"]["exit_code"], exit_code)

    def test_mocked_canonical_executor_runs_verify_mir_and_requires_only_contract_anchors(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = _fixture_root(root)
            mimi = root / "target/debug/mimi"
            mimi_sha = _write(mimi, b"#!/bin/sh\nexit 0\n", executable=True)
            contract = _canonical_contract(identity)
            source_path = identity["source_path"]
            expectation = {
                "class": "canonical_mir_input_rejection", "reason": "canonical MIR input rejected",
                "explicit": True, "source_hash_status": "matched", "source_sha256": identity["source_sha256"],
                "authority_valid": True, "canonical_mir_input_rejection_contract": contract,
                "canonical_mir_input_rejection_authority": _direct_authority(
                    root, "canonical_mir_input_rejection", source_path, identity["source_sha256"], contract
                ),
            }
            logs = root / "logs"
            logs.mkdir()
            source = {"path": source_path, "sha256": identity["source_sha256"], "target_kind": "executable"}
            stdout = (
                b"canonical MIR verifier input rejected\n"
                b"use after consuming non-Copy value\n"
            )
            with patch.object(
                census.subprocess, "run",
                return_value=SimpleNamespace(stdout=stdout, stderr=b"", returncode=1),
            ) as mock_run:
                record = census._run_direct_diagnostic_one(
                    root=root, source=source, index=1, mimi=mimi, timeout=2,
                    logs_root=logs, expectation=expectation,
                    expectations={source_path: expectation}, mimi_sha256=mimi_sha,
                )

        self.assertEqual(mock_run.call_args.args[0], [str(mimi), "verify", source_path, "--mir"])
        self.assertEqual(record["expectation"]["class"], "canonical_mir_input_rejection")
        self.assertTrue(record["expectation"]["matched"])
        self.assertEqual(record["failure_classification"]["class"], "canonical_mir_input_rejection")
        self.assertEqual(record["route"]["disposition"], "unobserved")
        self.assertIsNone(record["output_artifact"])
        self.assertTrue(record["canonical_mir_input_rejection_execution"]["verifier_proof_not_entered"])


if __name__ == "__main__":
    unittest.main()
