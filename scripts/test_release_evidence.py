#!/usr/bin/env python3
"""Isolated tests for the V0.2 release evidence collector.

The tests use a tiny Git repository and synthetic command output.  They test
collector decisions, not Mimi semantics; real compiler logs remain the source
of route/backend/verifier/native observations in a release run.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT_DIR = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("release_evidence", SCRIPT_DIR / "release_evidence.py")
release_evidence = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(release_evidence)


class ReleaseEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="mimi-v02-release-evidence-")
        self.parent = Path(self.temp.name)
        self.root = self.parent / "repo"
        self.root.mkdir()
        (self.root / "src").mkdir()
        (self.root / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
        (self.root / "Cargo.toml").write_text("[package]\nname='fixture'\nversion='0.0.0'\n", encoding="utf-8")
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(
            ["git", "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
             "commit", "-qm", "initial fixture"], cwd=self.root, check=True,
        )
        self.evidence = self.parent / "evidence"
        self.evidence.mkdir()
        self.toolchain = {
            "platform": {"sysname": "Synthetic", "machine": "fixture"},
            "tools": [
                {
                    "name": name,
                    "status": "observed",
                    "executable_sha256": hashlib.sha256(name.encode()).hexdigest(),
                }
                for name in ("rustc", "cargo", "llvm", "z3", "cc")
            ],
            "environment": {},
            "identity": "toolchain-identity",
        }
        self.probe = patch.object(release_evidence, "toolchain_snapshot", return_value=self.toolchain)
        self.probe.start()
        self.addCleanup(self.probe.stop)
        self.baseline = release_evidence.source_snapshot(self.root)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_parsers_require_real_markers_and_capture_route_backend_validator_memory(self) -> None:
        text = """\
canonical route disposition: canonical (ordinary-map-v1) mir_digest=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
backend=native
verifier status: Proven
Maximum resident set size (kbytes): 1280
ERROR SUMMARY: 0 errors
ABI digest: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
"""
        observations = release_evidence.parse_observations(text)
        self.assertEqual(observations["route"]["disposition"], "canonical")
        self.assertEqual(observations["route"]["mir_digest"], "a" * 64)
        self.assertEqual(observations["backend"]["backend"], "native")
        self.assertEqual(observations["validator"]["status"], "Proven")
        self.assertEqual(observations["native_memory"]["max_rss_kb"], 1280)
        self.assertFalse(observations["native_memory"]["memory_errors"])
        contradictory_memory = release_evidence.parse_native_memory(
            "ERROR SUMMARY: 4 errors\nERROR SUMMARY: 0 errors"
        )
        self.assertTrue(contradictory_memory["memory_errors"])
        conflicting_abi = release_evidence.parse_abi(
            "ABI digest: " + "a" * 64 + "\nABI digest: " + "b" * 64
        )
        self.assertFalse(conflicting_abi["observed"])
        self.assertTrue(conflicting_abi["ambiguous"])
        self.assertEqual(observations["abi"]["digest"], "b" * 64)
        self.assertFalse(release_evidence.parse_route("build completed")['observed'])
        ambiguous_route = release_evidence.parse_route(
            "route disposition: canonical mir_digest=" + "a" * 64
            + "\nroute disposition: legacy mir_digest=" + "a" * 64
        )
        self.assertFalse(ambiguous_route["observed"])
        self.assertTrue(ambiguous_route["ambiguous"])
        ambiguous_backend = release_evidence.parse_backend("backend=native\nbackend=bytecode")
        self.assertFalse(ambiguous_backend["observed"])
        self.assertTrue(ambiguous_backend["ambiguous"])
        ambiguous_validator = release_evidence.parse_validator(
            "verifier status: Proven\nverifier status: Disproven"
        )
        self.assertFalse(ambiguous_validator["observed"])
        self.assertTrue(ambiguous_validator["ambiguous"])
        receipt = release_evidence.parse_observations(json.dumps({
            "route": {"disposition": "canonical", "mir_digest": "c" * 64},
            "backend": {"observed": "native"},
            "verifier": {"observed": "Proven"},
        }))
        self.assertEqual(receipt["route"]["mir_digest"], "c" * 64)
        self.assertEqual(receipt["backend"]["backend"], "native")
        self.assertEqual(receipt["validator"]["status"], "Proven")

    def test_command_requires_observations_and_hashes_artifacts(self) -> None:
        artifact = self.root / "native.bin"
        digest = hashlib.sha256(b"native artifact\n").hexdigest()
        code = (
            "from pathlib import Path; "
            f"Path({str(artifact)!r}).write_bytes(b'native artifact\\n'); "
            "print('canonical route disposition: canonical (demo) mir_digest=' + 'a'*64); "
            "print('backend=native'); print('verifier status: Proven'); "
            "print('Maximum resident set size (kbytes): 7'); print('ERROR SUMMARY: 0 errors')"
        )
        record = release_evidence.run_evidence(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="native-proof", command=[sys.executable, "-c", code], kind="native_memory",
            required_observations=["route", "backend", "validator", "native_memory"],
            expected_artifacts={"native.bin": digest},
        )
        self.assertFalse(record["accepted"])
        self.assertTrue(record["provisional_pass"])
        self.assertEqual(record["status"], "passed")
        self.assertEqual(record["artifacts"][0]["status"], "matched")
        self.assertTrue(Path(record["raw_log"]["path"]).is_file())

    def test_zero_exit_without_marker_is_environment_unverified(self) -> None:
        record = release_evidence.run_evidence(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="path-only", command=[sys.executable, "-c", "print('done')"],
            required_observations=["route", "backend", "validator"],
        )
        self.assertFalse(record["accepted"])
        self.assertEqual(record["status"], "environment_unverified")
        self.assertIn("required observations missing", record["reason"])
        self.assertTrue(record.get("failure_record"))
        self.assertTrue(Path(record["failure_record"]).is_file())

    def test_legacy_or_ambiguous_receipts_cannot_pass_release_observations(self) -> None:
        cases = [
            (
                "legacy-route",
                "print('route disposition: legacy mir_digest=' + 'a'*64)",
                ["route"],
                "canonical",
                None,
                None,
            ),
            (
                "ambiguous-backend",
                "print('canonical route disposition: canonical (x) mir_digest=' + 'a'*64); "
                "print('backend=native'); print('backend=bytecode')",
                ["route", "backend"],
                "canonical",
                "native",
                None,
            ),
            (
                "ambiguous-verifier",
                "print('canonical route disposition: canonical (x) mir_digest=' + 'a'*64); "
                "print('verifier status: Proven'); print('verifier status: Disproven')",
                ["route", "validator"],
                "canonical",
                None,
                "Proven",
            ),
        ]
        for evidence_id, code, required, route, backend, validator in cases:
            with self.subTest(evidence_id=evidence_id):
                record = release_evidence.run_evidence(
                    root=self.root,
                    evidence=self.evidence,
                    baseline=self.baseline,
                    evidence_id=evidence_id,
                    command=[sys.executable, "-c", code],
                    required_observations=required,
                    expected_route=route,
                    expected_backend=backend,
                    expected_validator=validator,
                )
                self.assertFalse(record["accepted"])
                self.assertFalse(record["provisional_pass"])

    def test_declared_rejection_is_distinct_from_an_unexplained_failure(self) -> None:
        record = release_evidence.run_evidence(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="expected-rejection", command=[sys.executable, "-c", "print('route disposition: rejected'); raise SystemExit(2)"],
            required_observations=["route"], expected_exit=2, expected_result="explicit_reject",
        )
        self.assertFalse(record["accepted"])
        self.assertTrue(record["provisional_pass"])
        self.assertEqual(record["status"], "explicit_reject")

    def test_source_drift_rejects_command_and_retains_failure(self) -> None:
        code = f"from pathlib import Path; Path({str(self.root / 'src' / 'main.rs')!r}).write_text('changed\\n')"
        record = release_evidence.run_evidence(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="drift", command=[sys.executable, "-c", code],
        )
        self.assertFalse(record["accepted"])
        self.assertEqual(record["status"], "unexplained_failure")
        self.assertFalse(record["source_stable"])
        failure = Path(record["failure_record"])
        self.assertTrue(failure.is_file())
        payload = json.loads(failure.read_text(encoding="utf-8"))
        self.assertFalse(payload["accepted"])

    def test_duplicate_attempt_does_not_overwrite_first_log_or_record(self) -> None:
        first = release_evidence.run_evidence(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="retry", command=[sys.executable, "-c", "raise SystemExit(3)"],
        )
        second = release_evidence.run_evidence(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="retry", command=[sys.executable, "-c", "raise SystemExit(4)"],
        )
        self.assertNotEqual(first["raw_log"]["path"], second["raw_log"]["path"])
        self.assertTrue(Path(first["record_path"]).is_file())
        self.assertTrue(Path(second["record_path"]).is_file())
        failures = list((self.evidence / "failures").glob("retry-*.json"))
        self.assertGreaterEqual(len(failures), 2)

    def test_offline_network_marker_is_not_a_pass(self) -> None:
        record = release_evidence.run_evidence(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="offline", command=[sys.executable, "-c", "print('Updating crates.io index')"],
            offline=True,
        )
        self.assertFalse(record["accepted"])
        self.assertEqual(record["status"], "environment_unverified")
        self.assertIn("network access", record["reason"])
        self.assertEqual(record["environment"]["CARGO_NET_OFFLINE"], "true")

    def test_abi_missing_and_mismatch_are_fail_closed(self) -> None:
        missing = release_evidence.check_abi(self.parent / "missing.expected", self.parent / "missing.actual")
        self.assertFalse(missing["accepted"])
        self.assertEqual(missing["status"], "environment_unverified")
        expected = self.parent / "expected.abi"
        actual = self.parent / "actual.abi"
        expected.write_text("abi-v1\n", encoding="utf-8")
        actual.write_text("abi-v2\n", encoding="utf-8")
        mismatch = release_evidence.check_abi(expected, actual)
        self.assertFalse(mismatch["accepted"])
        self.assertEqual(mismatch["status"], "unexplained_failure")
        actual.write_text("abi-v1\n", encoding="utf-8")
        matched = release_evidence.check_abi(expected, actual)
        self.assertFalse(matched["accepted"])
        self.assertTrue(matched["provisional_pass"])

    def test_detached_log_cannot_self_attest_its_source_or_exit_status(self) -> None:
        log = self._write_log(
            "route.log",
            f"source_identity: {self.baseline['source_identity']}\n"
            + "canonical route disposition: canonical (x) mir_digest=" + "a" * 64,
        )
        record = release_evidence.ingest_log(
            evidence=self.evidence,
            baseline=self.baseline,
            evidence_id="detached-route",
            log_path=log,
            required_observations=["route"],
            source_identity=self.baseline["source_identity"],
            exit_code=0,
        )
        self.assertFalse(record["accepted"])
        self.assertFalse(record["provisional_pass"])
        self.assertFalse(record["provenance_bound"])
        self.assertTrue(record["provenance_marker_match"])
        self.assertEqual(record["observed_status"], "passed")
        self.assertEqual(record["status"], "environment_unverified")
        nonzero = release_evidence.ingest_log(
            evidence=self.evidence,
            baseline=self.baseline,
            evidence_id="detached-crash",
            log_path=log,
            required_observations=["route"],
            source_identity=self.baseline["source_identity"],
            exit_code=139,
        )
        self.assertEqual(nonzero["observed_status"], "unexplained_failure")
        self.assertFalse(nonzero["accepted"])

    def test_empty_gate_and_single_backend_record_cannot_satisfy_matrix(self) -> None:
        empty = release_evidence.aggregate_gate(self.evidence, baseline=self.baseline)
        self.assertFalse(empty["accepted"])
        self.assertFalse(empty["provisional_accepted"])
        self.assertTrue(any("at least one required evidence id" in error for error in empty["integrity_failures"]))
        self.assertTrue(any("fixed fixture/backend/route matrix" in error for error in empty["integrity_failures"]))

        record = self._run_matrix_record("only-bytecode", "bytecode")
        matrix = self._matrix("only-bytecode", "missing-native")
        result = release_evidence.aggregate_gate(
            self.evidence, required_ids=["only-bytecode"], required_matrix=matrix, baseline=self.baseline
        )
        self.assertFalse(result["accepted"])
        self.assertFalse(result["provisional_accepted"])
        self.assertEqual(result["missing_ids"], ["missing-native"])
        self.assertTrue(record["provisional_pass"])

    def test_fixed_matrix_requires_each_fixture_backend_route_cell(self) -> None:
        bytecode = self._run_matrix_record("fixture-bytecode", "bytecode")
        native = self._run_matrix_record("fixture-native", "native")
        self.assertTrue(bytecode["provisional_pass"])
        self.assertTrue(native["provisional_pass"])
        result = release_evidence.aggregate_gate(
            self.evidence,
            required_ids=["fixture-bytecode", "fixture-native"],
            required_matrix=self._matrix("fixture-bytecode", "fixture-native"),
            baseline=self.baseline,
        )
        self.assertTrue(result["provisional_accepted"])
        self.assertFalse(result["accepted"])
        self.assertTrue(result["same_source_identity"])
        self.assertTrue(result["same_toolchain_identity"])

    def test_forged_or_accepted_record_fails_record_integrity_gate(self) -> None:
        forged = {
            "schema_version": release_evidence.SCHEMA_VERSION,
            "evidence_id": "forged",
            "kind": "command",
            "source_identity": self.baseline["source_identity"],
            "toolchain_identity": self.baseline["toolchain"]["identity"],
            "provisional_pass": True,
            "provisional": True,
            "accepted": True,
        }
        release_evidence._immutable_write(self.evidence / "forged.json", forged)
        result = release_evidence.aggregate_gate(
            self.evidence,
            required_ids=["forged"],
            required_matrix=self._matrix("forged", "missing-native"),
            baseline=self.baseline,
        )
        self.assertFalse(result["accepted"])
        self.assertTrue(any("must remain provisional and unaccepted" in error for error in result["integrity_failures"]))
        self.assertTrue(any("raw log is required" in error for error in result["integrity_failures"]))

    def test_hash_manifest_is_content_bound_and_does_not_mark_missing_as_green(self) -> None:
        empty = release_evidence.hash_manifest(self.evidence, baseline=self.baseline)
        self.assertFalse(empty["provisional_accepted"])
        self.assertFalse(empty["accepted"])
        (self.evidence / "notes.log").write_text("a log\n", encoding="utf-8")
        missing_baseline = dict(self.baseline)
        missing_baseline["source_files"] = [{"path": "does-not-exist", "kind": "missing", "status": "missing"}]
        manifest = release_evidence.hash_manifest(self.evidence, baseline=missing_baseline)
        self.assertFalse(manifest["accepted"])
        self.assertEqual(manifest["status"], "environment_unverified")
        self.assertTrue(manifest["manifest_sha256"])

    def test_missing_explicit_source_input_is_retained_and_unverified(self) -> None:
        snapshot = release_evidence.source_snapshot(self.root, ["fixtures/does-not-exist.mimi"])
        missing = [row for row in snapshot["source_files"] if row["path"] == "fixtures/does-not-exist.mimi"]
        self.assertEqual(len(missing), 1)
        self.assertEqual(missing[0]["kind"], "missing")
        manifest = release_evidence.hash_manifest(self.evidence, baseline=snapshot)
        self.assertFalse(manifest["accepted"])

    def test_no_obligations_is_not_proven_and_offline_rebuild_requires_hashes(self) -> None:
        verifier = release_evidence.run_evidence(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="no-obligations", command=[sys.executable, "-c", "print('verifier status: NoObligations')"],
            required_observations=["validator"],
        )
        self.assertFalse(verifier["accepted"])
        self.assertEqual(verifier["status"], "environment_unverified")
        rebuild = release_evidence.offline_rebuild(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="offline-no-hash", command=[sys.executable, "-c", "print('done')"],
            expected_artifacts=None,
        )
        self.assertFalse(rebuild["accepted"])
        self.assertIn("artifact hashes", rebuild["reason"])

    def test_offline_rebuild_requires_cargo_completion_and_matching_artifact(self) -> None:
        artifact = self.root / "offline.bin"
        digest = hashlib.sha256(b"offline\n").hexdigest()
        code = (
            "from pathlib import Path; "
            f"Path({str(artifact)!r}).write_bytes(b'offline\\n'); "
            "print('{\"reason\":\"build-finished\",\"success\":true}')"
        )
        record = release_evidence.offline_rebuild(
            root=self.root, evidence=self.evidence, baseline=self.baseline,
            evidence_id="offline-good", command=[sys.executable, "-c", code],
            expected_artifacts={"offline.bin": digest},
        )
        self.assertFalse(record["accepted"])
        self.assertFalse(record["provisional_pass"])
        self.assertEqual(record["status"], "environment_unverified")
        self.assertIn("resolved Cargo build command", record["reason"])

        fake_cargo = self.root / "fake-bin" / "cargo"
        fake_cargo.parent.mkdir()
        fake_cargo.write_text(
            "#!/usr/bin/env python3\n"
            "import json\n"
            "print(json.dumps({'reason':'build-finished','success':True}))\n",
            encoding="utf-8",
        )
        fake_cargo.chmod(0o755)
        missing_probe = release_evidence.offline_rebuild(
            root=self.root,
            evidence=self.evidence,
            baseline=self.baseline,
            evidence_id="offline-without-cargo-probe",
            command=[str(fake_cargo), "build", "--offline", "--message-format=json"],
            expected_artifacts={"src/main.rs": hashlib.sha256(b"fn main() {}\n").hexdigest()},
        )
        self.assertFalse(missing_probe["accepted"])
        self.assertFalse(missing_probe["provisional_pass"])
        self.assertIn("captured Cargo tool", missing_probe["reason"])

    def _run_matrix_record(self, evidence_id: str, backend: str) -> dict:
        code = (
            "print('canonical route disposition: canonical (matrix) mir_digest=' + 'a'*64); "
            f"print('backend={backend}')"
        )
        return release_evidence.run_evidence(
            root=self.root,
            evidence=self.evidence,
            baseline=self.baseline,
            evidence_id=evidence_id,
            command=[sys.executable, "-c", code],
            required_observations=["route", "backend"],
            expected_route="canonical",
            expected_backend=backend,
            fixture_id="same-fixture",
            route_mode="default-run",
            comparison_hash="c" * 64,
        )

    @staticmethod
    def _matrix(bytecode_id: str, native_id: str) -> list[dict]:
        return [
            {
                "evidence_id": bytecode_id,
                "fixture_id": "same-fixture",
                "route_mode": "default-run",
                "backend": "bytecode",
                "route": "canonical",
                "comparison_hash": "c" * 64,
            },
            {
                "evidence_id": native_id,
                "fixture_id": "same-fixture",
                "route_mode": "default-run",
                "backend": "native",
                "route": "canonical",
                "comparison_hash": "c" * 64,
            },
        ]

    def _write_log(self, name: str, text: str) -> Path:
        path = self.parent / name
        path.write_text(text + "\n", encoding="utf-8")
        return path


if __name__ == "__main__":
    unittest.main(verbosity=2)
