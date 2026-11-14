"""Fast, no-Cargo tests for the 0.2 evidence/fixture tooling."""

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import evidence_manifest as evidence


RUNNER_SPEC = importlib.util.spec_from_file_location(
    "run_fixtures", Path(__file__).with_name("run-fixtures.py")
)
assert RUNNER_SPEC and RUNNER_SPEC.loader
RUNNER = importlib.util.module_from_spec(RUNNER_SPEC)
sys.modules[RUNNER_SPEC.name] = RUNNER
RUNNER_SPEC.loader.exec_module(RUNNER)


class EvidenceManifestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="mimi-v02-evidence-")
        self.root = Path(self.temp.name)
        (self.root / "fixture.json").write_text("{}\n", encoding="utf-8")
        (self.root / "artifact.bin").write_bytes(b"artifact-v1\n")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_source_snapshot_is_explicit_and_untracked_files_are_not_included(self) -> None:
        (self.root / "unlisted.tmp").write_text("do not collect\n", encoding="utf-8")
        snapshot = evidence.source_snapshot(self.root, ["fixture.json"])
        self.assertEqual(sorted(snapshot["inputs"]), ["fixture.json"])
        self.assertEqual(snapshot["inputs"]["fixture.json"]["bytes"], 3)

    def test_manifest_hashes_artifact_and_refuses_overwrite(self) -> None:
        artifact = evidence.artifact_record(self.root / "artifact.bin", root=self.root, kind="native")
        manifest = evidence.create_manifest(
            root=self.root,
            evidence_dir=self.root / "evidence",
            source_paths=[self.root / "fixture.json"],
            records=[{
                "fixture_id": "fixture",
                "accepted": False,
                "provisional": True,
                "checks_passed": True,
                "result_class": "已通过",
            }],
            artifacts=[artifact],
            expected_fixture_ids=["fixture"],
        )
        output = self.root / "evidence" / "manifest.json"
        evidence.write_manifest(output, manifest)
        report = evidence.verify_manifest(output)
        self.assertTrue(report["digest_ok"])
        self.assertFalse(report["accepted"])
        self.assertTrue(report["provisional_accepted"])
        with self.assertRaises(FileExistsError):
            evidence.write_manifest(output, manifest)
        (self.root / "artifact.bin").write_bytes(b"tampered\n")
        with self.assertRaises(ValueError, msg="changed artifact must invalidate evidence"):
            evidence.verify_manifest(output)

    def test_verifier_recomputes_acceptance_instead_of_trusting_rehashed_flags(self) -> None:
        manifest = evidence.create_manifest(
            root=self.root,
            evidence_dir=self.root / "evidence-recompute",
            source_paths=[self.root / "fixture.json"],
            records=[{
                "fixture_id": "fixture",
                "accepted": False,
                "provisional": True,
                "checks_passed": True,
                "result_class": "已通过",
            }],
            artifacts=[evidence.artifact_record(self.root / "artifact.bin", root=self.root)],
            expected_fixture_ids=["fixture"],
        )
        output = self.root / "evidence-recompute" / "manifest.json"
        evidence.write_manifest(output, manifest)

        def rewrite_with_valid_outer_hash(data: dict) -> None:
            record = data["records"][0]
            record.pop("record_hash", None)
            record["record_hash"] = evidence.digest_json(record)
            data.pop("manifest_content_hash", None)
            data["manifest_content_hash"] = evidence.digest_json(data)
            payload = json.dumps(data, indent=2, sort_keys=True, ensure_ascii=False).encode("utf-8") + b"\n"
            output.write_bytes(payload)
            output.with_name(output.name + ".sha256").write_text(
                evidence.sha256_bytes(payload) + "\n", encoding="ascii"
            )

        wrong_counts = json.loads(output.read_text(encoding="utf-8"))
        wrong_counts["result_counts"]["已通过"] = 41
        rewrite_with_valid_outer_hash(wrong_counts)
        with self.assertRaisesRegex(ValueError, "result counts do not match"):
            evidence.verify_manifest(output)

        tampered = json.loads(output.read_text(encoding="utf-8"))
        tampered["records"][0]["checks_passed"] = False
        rewrite_with_valid_outer_hash(tampered)
        with self.assertRaisesRegex(ValueError, "provisional_accepted does not match"):
            evidence.verify_manifest(output)

        accepted_claim = json.loads(output.read_text(encoding="utf-8"))
        accepted_claim["records"][0]["checks_passed"] = True
        accepted_claim["accepted"] = True
        rewrite_with_valid_outer_hash(accepted_claim)
        with self.assertRaisesRegex(ValueError, "cannot be accepted"):
            evidence.verify_manifest(output)

    def test_empty_expected_inventory_cannot_be_provisionally_green(self) -> None:
        manifest = evidence.create_manifest(
            root=self.root,
            evidence_dir=self.root / "empty-inventory",
            source_paths=[self.root / "fixture.json"],
            records=[{
                "fixture_id": "fixture",
                "accepted": False,
                "provisional": True,
                "checks_passed": True,
                "result_class": "已通过",
            }],
            artifacts=[evidence.artifact_record(self.root / "artifact.bin", root=self.root)],
            expected_fixture_ids=[],
        )
        self.assertFalse(manifest["provisional_accepted"])
        self.assertFalse(manifest["accepted"])

    def _write_program(self, name: str, source: str) -> None:
        (self.root / name).write_text(source, encoding="utf-8")

    def _run_manifest(self, fixture: dict, name: str = "fixtures.json") -> tuple[int, dict]:
        return self._run_fixtures([fixture], name=name)

    def _run_fixtures(self, fixtures: list[dict], name: str = "fixtures.json", selected: list[str] | None = None) -> tuple[int, dict]:
        manifest_path = self.root / name
        manifest_path.write_text(
            json.dumps({"schema_version": 1, "fixtures": fixtures}, ensure_ascii=False),
            encoding="utf-8",
        )
        evidence_dir = self.root / ("evidence-" + name.replace(".", "-"))
        args = type(
            "Args",
            (),
            {
                "root": str(self.root),
                "manifest": str(manifest_path),
                "evidence": str(evidence_dir),
                "output": None,
                "plan": None,
                "fixture": selected,
                "timeout": 10.0,
            },
        )()
        code = RUNNER.run_all(args)
        result = json.loads((evidence_dir / "manifest.json").read_text(encoding="utf-8"))
        return code, result

    def test_runner_compares_an_independent_oracle(self) -> None:
        self._write_program("candidate.py", "print('stable')\n")
        self._write_program("oracle.py", "print('stable')\n")
        fixture = {
            "id": "w1-oracle",
            "task_id": "B-02",
            "inputs": ["candidate.py", "oracle.py"],
            "command": [sys.executable, "candidate.py"],
            "oracle": {"command": [sys.executable, "oracle.py"]},
            "expected": {"result_class": "pass", "exit_code": 0},
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 0)
        self.assertFalse(result["accepted"])
        self.assertTrue(result["provisional_accepted"])
        self.assertEqual(result["result_counts"]["已通过"], 1)
        record = result["records"][0]
        self.assertEqual(record["task_id"], "B-02")
        self.assertEqual(record["plan_hash"], None)
        self.assertTrue(record["raw_log_hashes"]["stdout"])

    def test_dotted_fixture_ids_keep_distinct_log_names(self) -> None:
        self._write_program("candidate-dot.py", "print('dot')\n")
        fixture = {
            "id": "w1.json-roundtrip",
            "task_id": "B-02",
            "inputs": ["candidate-dot.py"],
            "command": [sys.executable, "candidate-dot.py"],
            "expected": {"result_class": "pass", "exit_code": 0, "stdout": "dot\n"},
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 0)
        self.assertFalse(result["accepted"])
        self.assertTrue(result["provisional_accepted"])
        self.assertTrue((self.root / "evidence-fixtures-json" / "w1.json-roundtrip.stdout").exists())

    def test_reject_fixture_is_accepted_only_when_rejection_is_declared(self) -> None:
        self._write_program("reject.py", "import sys; print('bad input', file=sys.stderr); sys.exit(3)\n")
        fixture = {
            "id": "w1-negative",
            "task_id": "B-06",
            "inputs": ["reject.py"],
            "command": [sys.executable, "reject.py"],
            "expected": {"result_class": "reject", "exit_code": 3, "stderr_regex": "bad input"},
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 0)
        self.assertFalse(result["accepted"])
        self.assertTrue(result["provisional_accepted"])
        self.assertEqual(result["records"][0]["result_class"], "明确拒绝")

    def test_zero_required_tests_cannot_be_green(self) -> None:
        self._write_program("no_tests.py", "print('done')\n")
        fixture = {
            "id": "zero-tests",
            "task_id": "B-09",
            "inputs": ["no_tests.py"],
            "command": [sys.executable, "no_tests.py"],
            "expected": {"result_class": "pass", "exit_code": 0, "stdout": "done\n"},
            "require_tests": True,
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 2)
        self.assertFalse(result["accepted"])
        self.assertTrue(any("required test fixture" in error for error in result["records"][0]["errors"]))

    def test_unsupported_fixture_is_recorded_but_never_counts_as_pass(self) -> None:
        self._write_program("unsupported.py", "import sys; sys.exit(2)\n")
        fixture = {
            "id": "unsupported-shape",
            "task_id": "B-02",
            "inputs": ["unsupported.py"],
            "command": [sys.executable, "unsupported.py"],
            "expected": {"result_class": "不支持", "exit_code": 2},
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 2)
        self.assertFalse(result["accepted"])
        self.assertEqual(result["records"][0]["result_class"], "不支持")

    def test_source_input_mutation_invalidates_fixture(self) -> None:
        self._write_program(
            "mutate.py",
            "from pathlib import Path\nPath('input.txt').write_text('changed\\n')\nprint('ok')\n",
        )
        (self.root / "input.txt").write_text("before\n", encoding="utf-8")
        fixture = {
            "id": "source-drift",
            "task_id": "B-01",
            "inputs": ["mutate.py", "input.txt"],
            "command": [sys.executable, "mutate.py"],
            "expected": {"result_class": "pass", "exit_code": 0, "stdout": "ok\n"},
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 2)
        self.assertFalse(result["accepted"])
        self.assertFalse(result["records"][0]["source_stable"])

    def test_missing_declared_input_cannot_be_skipped(self) -> None:
        self._write_program("ignores_missing.py", "print('ok')\n")
        fixture = {
            "id": "missing-input",
            "task_id": "B-01",
            "inputs": ["does-not-exist.json", "ignores_missing.py"],
            "command": [sys.executable, "ignores_missing.py"],
            "expected": {"result_class": "pass", "exit_code": 0, "stdout": "ok\n"},
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 2)
        self.assertFalse(result["accepted"])
        self.assertIn("declared input is missing", result["records"][0]["errors"][0])

    def test_timeout_is_an_unexplained_failure_not_a_missing_environment(self) -> None:
        self._write_program("slow.py", "import time; time.sleep(1)\n")
        fixture = {
            "id": "timeout",
            "task_id": "B-04",
            "inputs": ["slow.py"],
            "command": [sys.executable, "slow.py"],
            "timeout_seconds": 0.02,
            "expected": {"result_class": "pass", "exit_code": 0},
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 2)
        self.assertFalse(result["accepted"])
        self.assertEqual(result["records"][0]["result_class"], "未解释失败")

    def test_exit_code_alone_cannot_pass_a_fixture(self) -> None:
        fixture = {
            "id": "exit-only",
            "command": ["true"],
            "expected": {"result_class": "pass", "exit_code": 0},
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 2)
        self.assertFalse(result["accepted"])
        self.assertFalse(result["provisional_accepted"])
        self.assertIn("exit code alone is insufficient", result["records"][0]["errors"][-1])

    def test_arbitrary_nonzero_exit_cannot_be_accepted_as_explicit_rejection(self) -> None:
        fixture = {
            "id": "crash-is-not-rejection",
            "command": [sys.executable, "-c", "raise SystemExit(139)"],
            "expected": {"result_class": "reject"},
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 2)
        self.assertFalse(result["records"][0]["checks_passed"])
        self.assertNotEqual(result["records"][0]["result_class"], "明确拒绝")

    def test_fake_test_summary_from_non_test_process_is_rejected(self) -> None:
        summary = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
        fixture = {
            "id": "fake-test-count",
            "command": [sys.executable, "-c", f"print({summary!r}, end='')"],
            "expected": {"result_class": "pass", "exit_code": 0, "stdout": summary},
            "require_tests": True,
        }
        code, result = self._run_manifest(fixture)
        self.assertEqual(code, 2)
        self.assertFalse(result["records"][0]["checks_passed"])
        self.assertTrue(any("must invoke cargo test or mimi test" in error for error in result["records"][0]["errors"]))

    def test_partial_fixture_selection_cannot_satisfy_full_inventory(self) -> None:
        fixtures = [
            {"id": "one", "command": ["true"], "expected": {"result_class": "pass", "exit_code": 0, "stdout": ""}},
            {"id": "two", "command": ["false"], "expected": {"result_class": "pass", "exit_code": 0, "stdout": ""}},
        ]
        code, result = self._run_fixtures(fixtures, name="partial.json", selected=["one"])
        self.assertEqual(code, 2)
        self.assertFalse(result["accepted"])
        self.assertFalse(result["provisional_accepted"])
        self.assertEqual(result["expected_fixture_ids"], ["one", "two"])
        self.assertIn("fixture selection is incomplete", result["errors"][0])

    def test_fixture_exception_cannot_disappear_from_manifest(self) -> None:
        fixtures = [
            {"id": "good", "command": ["true"], "expected": {"result_class": "pass", "exit_code": 0, "stdout": ""}},
            {"id": "bad-path", "inputs": ["../outside"], "command": ["true"], "expected": {"result_class": "pass", "exit_code": 0, "stdout": ""}},
        ]
        code, result = self._run_fixtures(fixtures, name="exception.json")
        self.assertEqual(code, 2)
        self.assertFalse(result["accepted"])
        self.assertFalse(result["provisional_accepted"])
        bad_path = next(record for record in result["records"] if record["fixture_id"] == "bad-path")
        self.assertFalse(bad_path["checks_passed"])
        self.assertIn("path escapes repository root", bad_path["errors"][0])
        self.assertTrue(any("bad-path" in error for error in result["errors"]))
        self.assertEqual(len(result["records"]), 2)


class PlanSummaryTests(unittest.TestCase):
    def test_check_plan_json_exposes_digest_without_editing_plan(self) -> None:
        tool = Path(__file__).parents[1] / "devdocs" / "V0.2" / "tools" / "check_plan.py"
        plan = tool.parent.parent / "plan.toml"
        before = hashlib.sha256(plan.read_bytes()).hexdigest()
        result = subprocess.run(
            [sys.executable, str(tool), "--json"], capture_output=True, text=True, check=False
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads(result.stdout)
        self.assertTrue(summary["valid"])
        self.assertEqual(summary["task_count"], 71)
        self.assertEqual(summary["decision_count"], 14)
        self.assertEqual(summary["plan_sha256"], before)
        self.assertEqual(hashlib.sha256(plan.read_bytes()).hexdigest(), before)


if __name__ == "__main__":
    unittest.main(verbosity=2)
