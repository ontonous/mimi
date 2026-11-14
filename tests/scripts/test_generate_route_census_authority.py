from __future__ import annotations

import base64
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_DIR = ROOT / "scripts"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))
GENERATOR_PATH = SCRIPTS_DIR / "generate_route_census_authority.py"
COLLECTOR_PATH = SCRIPTS_DIR / "full_route_census.py"


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


generator = load_module(GENERATOR_PATH, "route_authority_generator_test")


class RouteAuthorityGeneratorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="route-authority-generator-test-")
        self.base = Path(self.temporary.name).resolve()
        self.root = self.base / "repo"
        self.root.mkdir()
        (self.root / "scripts").mkdir()
        fixture_collector = self.root / "scripts" / "full_route_census.py"
        shutil.copy2(COLLECTOR_PATH, fixture_collector)
        shutil.copy2(GENERATOR_PATH, self.root / "scripts" / "generate_route_census_authority.py")
        self.census = load_module(
            fixture_collector, "route_authority_census_fixture"
        )
        (self.root / "tests" / "route-census").mkdir(parents=True)
        (self.root / "tests" / "route-census" / "authorities.rs").write_text(
            generator.EXPECTED_WRAPPER_SOURCE, encoding="utf-8"
        )
        (self.root / "Cargo.toml").write_text(
            """[package]
name = "mimi"
version = "0.1.11-dev"
edition = "2021"

[features]
default = []
route-census-authorities = []

[[test]]
name = "route-census-authorities"
path = "tests/route-census/authorities.rs"
harness = true
required-features = ["route-census-authorities"]
""",
            encoding="utf-8",
        )
        self.binary = self.root / "target" / "debug" / "mimi"
        self.binary.parent.mkdir(parents=True)
        self.binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.binary.chmod(0o755)
        self.report_path = self.base / "full-route-census.json"
        self.artifacts_root = self.base / "full-route-census.artifacts"
        (self.artifacts_root / "logs").mkdir(parents=True)
        self.output_dir = self.base / "generated"
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)

        (self.root / "fixtures").mkdir()
        (self.root / "fixtures" / "canonical.mimi").write_text(
            "func main() -> i32 {\n    return 0\n}\n", encoding="utf-8"
        )
        (self.root / "fixtures" / "rejected.mimi").write_text(
            "func main() -> i32 {\n    return unsupported()\n}\n", encoding="utf-8"
        )
        subprocess.run(["git", "add", "fixtures/*.mimi"], cwd=self.root, check=True)
        self.report = self.make_report()
        self.write_report()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_mimi_identity_and_route_code_registry_contracts(self) -> None:
        self.assertEqual(
            generator.AUTHORITY_MIMI_HELPER_SOURCE,
            self.census.ROUTE_CENSUS_MIMI_HELPER_SOURCE,
        )
        helper = generator.AUTHORITY_MIMI_HELPER_SOURCE
        self.assertIn('env::var("MIMI_ROUTE_CENSUS")', helper)
        self.assertIn('assert_eq!(census_mode, "1"', helper)
        self.assertIn("canonicalize()", helper)
        self.assertIn("MIMI_ROUTE_CENSUS_MIMI_SHA256", helper)
        self.assertIn("MIMI_ROUTE_CENSUS_AUTHORITY_BINARY", helper)
        source = (ROOT / "src" / "diagnostic" / "codes.rs").read_text(encoding="utf-8")
        declarations = re.findall(
            r"(?ms)^const MIR_ROUTE_CODES: \[&str; (\d+)\] = \[(.*?)^\];",
            source,
        )
        self.assertEqual(len(declarations), 1)
        declared_count, body = declarations[0]
        names = re.findall(r"\b(MIR_[A-Z0-9_]+)\b", body)
        constants = dict(re.findall(
            r'(?m)^pub const (MIR_[A-Z0-9_]+): &str = "([A-Z0-9-]+)";', source
        ))
        self.assertEqual(len(names), int(declared_count))
        self.assertEqual(len(names), len(set(names)))
        self.assertTrue(all(name in constants for name in names))
        rust_registry = frozenset(constants[name] for name in names)
        self.assertEqual(generator.CANONICAL_MIR_ROUTE_CODES, rust_registry)
        self.assertNotIn("MIR-FAKE-999", rust_registry)

    @staticmethod
    def digest(data: bytes) -> str:
        return hashlib.sha256(data).hexdigest()

    def raw_capture(self, data: bytes, name: str) -> dict:
        sidecar = self.artifacts_root / "logs" / name
        sidecar.write_bytes(data)
        return {
            "base64": base64.b64encode(data).decode("ascii"),
            "bytes": len(data),
            "path": name,
            "sha256": self.digest(data),
        }

    def make_record(self, source: dict, index: int, *, outcome: str) -> dict:
        path = str(source["path"])
        source_path = self.root / path
        scratch_path = self.base / "scratch" / f"input-{index:06d}"
        scratch_path.mkdir(parents=True)
        output_name = "out.so" if source["target_kind"] == "shared_library_probe" else "out"
        output_path = str(scratch_path / output_name)
        command = [str(self.binary), "build"]
        if source["target_kind"] == "shared_library_probe":
            command.append("--shared")
        command.extend([str(source_path), "-o", output_path])
        mimi_sha256 = self.digest(self.binary.read_bytes())
        mimi_observation = {
            "path": str(self.binary.resolve()),
            "resolved_path": str(self.binary.resolve()),
            "sha256": mimi_sha256,
        }
        mimi_identity = {
            "path": str(self.binary.resolve()),
            "sha256_expected": mimi_sha256,
            "before": mimi_observation,
            "after": mimi_observation,
            "matches_expected": True,
        }
        if outcome == "canonical":
            digest = "a" * 64
            route_line = (
                "canonical route disposition: canonical (default-route) mir_digest=" + digest
            )
            stderr_text = "\n".join([
                "MIMI_ROUTE_CENSUS route-admission consumer=build mode=default outcome=canonical",
                "MIMI_ROUTE_CENSUS canonical-native-build consumer=build mode=default receipt_identity_digest=" + "b" * 64,
                route_line,
                "info: canonical MIR native emitted owner='function:main'",
                "info: canonical MIR native compiled 1/1 function(s), 0 fell back; mir_digest=" + digest,
            ]) + "\n"
            stdout_text = ""
            exit_code = 0
            output_artifact = {"sha256": "c" * 64, "bytes": 42}
            candidate_classification = "canonical"
            expectation_class = "required_compile"
            failure = {"class": "none", "reason": "process exited successfully", "status": "not_applicable", "evidence": None}
            output_artifact_evidence = None
        else:
            code = "MIR-COVERAGE-001"
            route_reason = "closed canonical capability gap"
            route_line = f"default Canonical MIR route rejected: {route_reason}"
            diagnostic_line = (
                f"error[{code}] {source_path}:1:1-2:1 canonical route capability unavailable"
            )
            stderr_text = f"{route_line}\n{diagnostic_line}\n"
            stdout_text = ""
            exit_code = 1
            output_artifact = None
            candidate_classification = "intentional_reject"
            expectation_class = "expected_build_rejection"
            failure = self.census._failure_classification(
                run_status="completed",
                exit_code=exit_code,
                text=stderr_text,
                target_kind=source["target_kind"],
                route=self.census._route_observation(stdout_text, stderr_text, exit_code, "completed"),
            )
            output_artifact_evidence = {
                "schema_version": 2,
                "scratch": {
                    "path": str(scratch_path),
                    "input_index": index,
                    "existed_before": False,
                    "created_exclusively": True,
                    "output_absent_before_build": True,
                },
                "output_path": output_path,
                "command_output_path": output_path,
                "mimi_executable_identity": mimi_identity,
                "before": {"path": output_path, "exists": False, "sha256": None},
                "after": {"path": output_path, "exists": False, "sha256": None},
            }
        route = self.census._route_observation(stdout_text, stderr_text, exit_code, "completed")
        per_function = self.census._function_observations(stdout_text, stderr_text, None, route)
        stdout_raw = self.raw_capture(stdout_text.encode(), f"{index:06d}.stdout.bin")
        stderr_raw = self.raw_capture(stderr_text.encode(), f"{index:06d}.stderr.bin")
        diagnostics = self.census._diagnostic_codes(stderr_text + "\n" + stdout_text)
        expectation = {
            "class": expectation_class,
            "reason": "explicit source-bound fixture contract",
            "source_sha256": source["sha256"],
            "source_hash_status": "matched",
            "source_hash_matches": True,
            "source_authority_valid": True,
            "authority_valid": True,
            "explicit_expectation": True,
            "matched": True,
            "test_contract_valid": True,
            "cargo_target_valid": True,
            "test_execution_valid": True,
            "actual_exit_code": exit_code,
            "actual_route": route["disposition"],
            "actual_route_classification_status": route["classification_status"],
            "actual_route_reason": route["reason"],
            "actual_failure_class": failure["class"],
            "actual_diagnostic_codes": diagnostics,
            "actual_output_artifact_present": output_artifact is not None,
        }
        if outcome == "rejected":
            expectation.update({
                "expected_exit_code": exit_code,
                "expected_diagnostic_codes": diagnostics,
                "required_output_substrings": [route_line, diagnostic_line],
                "expected_route_disposition": "rejected",
                "expected_route_classification_status": "observed",
                "expected_route_reason": route["reason"],
                "expected_failure_class": failure["class"],
                "route_contract": {"reason_matches_exactly": True},
                "diagnostic_contract": {
                    "output_artifact_absent": True,
                    "diagnostic_codes_match_exactly": True,
                    "required_output_substrings_match": True,
                },
            })
        return {
            **source,
            "input_index": index,
            "path": path,
            "run_status": "completed",
            "command": command,
            "mimi_executable_identity": mimi_identity,
            "exit_code": exit_code,
            "stdout": stdout_text,
            "stderr": stderr_text,
            "stdout_raw": stdout_raw,
            "stderr_raw": stderr_raw,
            "sha256": source["sha256"],
            "source_hash_before_execution": source["sha256"],
            "source_hash_after_execution": source["sha256"],
            "source_stable_for_input": True,
            "route": route,
            "failure_classification": failure,
            "per_function": per_function,
            "dispatch_stats": None,
            "output_artifact": output_artifact,
            "rejection_artifact_evidence": output_artifact_evidence,
            "expectation": expectation,
            "candidate_classification": candidate_classification,
        }

    def make_source_diagnostic_record(self, source: dict, index: int) -> dict:
        shutil.rmtree(self.base / "scratch" / f"input-{index:06d}", ignore_errors=True)
        record = self.make_record(source, index, outcome="rejected")
        path = str(source["path"])
        fixture = self.root / path
        stderr_text = (
            f"{fixture} has 1 type error(s):\n"
            f"error[E0432] {fixture}:2:3-8 unknown name 'absent'\n"
            "error: type checking failed\n"
        )
        stderr_bytes = stderr_text.encode("utf-8")
        record["stdout"] = ""
        record["stderr"] = stderr_text
        record["stdout_raw"] = self.raw_capture(b"", f"{index:06d}.stdout.bin")
        record["stderr_raw"] = self.raw_capture(stderr_bytes, f"{index:06d}.stderr.bin")
        route = self.census._route_observation("", stderr_text, 1, "completed")
        failure = self.census._failure_classification(
            run_status="completed",
            exit_code=1,
            text=stderr_text,
            target_kind=source["target_kind"],
            route=route,
        )
        expectation_item = {
            "class": "expected_source_diagnostic",
            "reason": "source diagnostic fixture contract",
            "source_sha256": source["sha256"],
            "authority": {
                "test_id": "test_source_diagnostic_current",
                "assertion": {
                    "path": generator.EXPECTED_TEST_PATH,
                    "line": 1,
                    "sha256": "d" * 64,
                },
            },
            "explicit": True,
            "source_hash_status": "matched",
            "source_hash_matches": True,
            "authority_valid": True,
            "test_contract_valid": True,
            "source_authority_valid": True,
            "cargo_target_valid": True,
            "test_execution_valid": True,
            "test_execution_proof": {
                "status": "passed",
                "test_id": "test_source_diagnostic_current",
                "source_diagnostic_output_path": self.census._source_diagnostic_authority_output_path(
                    path, source["sha256"]
                ),
            },
            "expected_exit_code": 1,
            "expected_diagnostic_text": stderr_text,
            "expected_diagnostic_codes": ["E0432"],
            "expected_diagnostic_lines": self.census._source_diagnostic_lines(stderr_text),
            "expected_output_path": self.census._source_diagnostic_authority_output_path(
                path, source["sha256"]
            ),
        }
        record["exit_code"] = 1
        record["route"] = route
        record["failure_classification"] = failure
        record["per_function"] = self.census._function_observations(
            "", stderr_text, None, route
        )
        record["output_artifact"] = None
        record["source_diagnostic_artifact_evidence"] = dict(
            record["rejection_artifact_evidence"]
        )
        record["rejection_artifact_evidence"] = None
        record["candidate_classification"] = "source_diagnostic"
        record["expectation"] = self.census._expectation_result(
            path,
            1,
            route,
            {path: expectation_item},
            run_status="completed",
            failure_classification=failure,
            captured_output=stderr_bytes,
            captured_stdout=b"",
            captured_stderr=stderr_bytes,
            output_artifact=None,
        )
        return record

    def make_report(self) -> dict:
        shutil.rmtree(self.base / "scratch", ignore_errors=True)
        tracked_paths, tracked = self.census.tracked_mimi_paths(self.root)
        sources, exclusions, scope = self.census.enumerate_source_scope(self.root, tracked_paths)
        snapshot = self.census.source_snapshot(self.root, sources)
        records = []
        for index, source in enumerate(sources, 1):
            records.append(self.make_record(source, index, outcome="canonical" if index == 1 else "rejected"))
        manifest_digest = self.census._manifest_digest(sources)
        return {
            "schema_version": self.census.SCHEMA_VERSION,
            "evidence_kind": "route-census",
            "collection_mode": "full_execution",
            "collector": {
                "path": str(self.root / "scripts" / "full_route_census.py"),
                "sha256": self.digest((self.root / "scripts" / "full_route_census.py").read_bytes()),
            },
            "input_inventory": {
                "root": str(self.root),
                "count": len(sources),
                "manifest_sha256": manifest_digest,
                "inputs": sources,
                "scope_exclusions": exclusions,
                "source_scope": scope,
            },
            "manifest": {
                "denominator": len(sources),
                "sha256": manifest_digest,
                "source_scope_exactly_once": True,
            },
            "source_scope": scope,
            "source_snapshot": snapshot,
            "source_snapshot_end": snapshot,
            "source_snapshot_stable": True,
            "gate": {"mode": "evidence_collection_only", "requested": False, "passed": False},
            "artifacts": {"root": str(self.artifacts_root), "logs": "logs/"},
            "toolchain": {
                "mimi": {
                    "path": str(self.binary),
                    "sha256": self.digest(self.binary.read_bytes()),
                    "bytes": self.binary.stat().st_size,
                    "exists": True,
                }
            },
            "records": records,
        }

    def write_report(self) -> None:
        self.report_path.write_text(
            json.dumps(self.report, ensure_ascii=False, sort_keys=True), encoding="utf-8"
        )

    def mutate_and_write(self, mutate) -> None:
        self.report = self.make_report()
        mutate(self.report)
        self.write_report()

    def run_generator(self) -> dict:
        return generator.generate(self.report_path, self.root, self.output_dir)

    def run_bootstrap_generator(self) -> dict:
        return generator.generate(
            self.report_path,
            self.root,
            self.output_dir,
            bootstrap_report=True,
        )

    def run_generator_with_source_diagnostic_proof_fixture(self) -> dict:
        with (
            patch.object(generator, "_load_census_module", return_value=self.census),
            patch.object(
                self.census,
                "_expected_source_diagnostic_execution_proof_is_valid",
                return_value=True,
            ),
        ):
            return self.run_generator()

    def validate_generated_bundle(self) -> dict:
        self.run_generator()
        tracked_paths, _tracked = self.census.tracked_mimi_paths(self.root)
        sources, _exclusions, _scope = self.census.enumerate_source_scope(
            self.root, tracked_paths
        )
        return self.census._validate_route_authority_bundle(
            self.output_dir,
            root=self.root,
            sources=sources,
            mimi_path=self.binary,
        )

    def validate_generated_bootstrap_bundle(self) -> dict:
        self.run_bootstrap_generator()
        tracked_paths, _tracked = self.census.tracked_mimi_paths(self.root)
        sources, _exclusions, _scope = self.census.enumerate_source_scope(
            self.root, tracked_paths
        )
        return self.census._validate_route_authority_bundle(
            self.output_dir,
            root=self.root,
            sources=sources,
            mimi_path=self.binary,
        )

    def _refresh_generated_source_artifact_identity(self, source: bytes) -> None:
        source_path = self.output_dir / generator.EXPECTED_TEST_PATH
        source_path.write_bytes(source)
        manifest_path = self.output_dir / generator.GENERATION_MANIFEST_FILENAME
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["artifacts"][generator.EXPECTED_TEST_PATH] = {
            "sha256": self.digest(source),
            "bytes": len(source),
        }
        manifest_path.write_text(
            json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
            encoding="utf-8",
        )

    def test_cli_has_no_candidate_denominator_override(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            generator.parse_args(
                [
                    "--report",
                    "report.json",
                    "--root",
                    ".",
                    "--output-dir",
                    "generated",
                    "--expected-count",
                    "2",
                ]
            )

    def test_candidate_denominator_is_derived_from_current_inventory_and_frozen_report(self) -> None:
        self.assertFalse(hasattr(generator, "EXPECTED_CANDIDATE_COUNT"))
        self.report["input_inventory"]["count"] += 1
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "exact current inventory"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_bootstrap_cli_mode_is_explicit_and_mutually_exclusive(self) -> None:
        parsed = generator.parse_args(
            ["--bootstrap-report", "observations.json", "--root", ".", "--output-dir", "out"]
        )
        self.assertEqual(parsed.bootstrap_report, Path("observations.json"))
        self.assertIsNone(parsed.report)
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            generator.parse_args(
                [
                    "--report", "strict.json", "--bootstrap-report", "observations.json",
                    "--root", ".", "--output-dir", "out",
                ]
            )

    def test_bootstrap_generates_authorities_from_raw_rows_without_prior_execution_proof(self) -> None:
        for record in self.report["records"]:
            expectation = record["expectation"]
            expectation.update({
                "class": "unmapped",
                "explicit_expectation": False,
                "matched": False,
                "authority_valid": False,
                "test_contract_valid": False,
                "cargo_target_valid": False,
                "test_execution_valid": False,
            })
        self.write_report()

        binding = self.validate_generated_bootstrap_bundle()
        manifest = json.loads(
            (self.output_dir / generator.GENERATION_MANIFEST_FILENAME).read_text(encoding="utf-8")
        )
        expectations = json.loads(
            (self.output_dir / generator.EXPECTATIONS_FILENAME).read_text(encoding="utf-8")
        )
        rust = (self.output_dir / generator.EXPECTED_TEST_PATH).read_text(encoding="utf-8")

        self.assertEqual(binding["generation_mode"], "bootstrap_observation")
        self.assertEqual(binding["bootstrap_blocker_count"], 0)
        self.assertEqual(manifest["generation_mode"], "bootstrap_observation")
        self.assertEqual(manifest["status"], "generated_not_executed")
        self.assertTrue(manifest["bootstrap_observation"]["report_was_not_counted_as_strict_pass"])
        self.assertTrue(manifest["bootstrap_observation"]["strict_census_execution_required"])
        self.assertEqual(manifest["bootstrap_observation"]["candidate_denominator_preserved"], 2)
        self.assertEqual(manifest["outcomes"]["unmapped"], 0)
        self.assertEqual(len(manifest["rows"]), 2)
        self.assertEqual(len(expectations["inputs"]), 2)
        self.assertEqual(
            {entry["class"] for entry in expectations["inputs"].values()},
            {"required_compile", "expected_build_rejection"},
        )
        self.assertEqual(rust.count("#[test]"), 2)
        self.assertEqual(manifest["outcomes"]["expected_source_diagnostic"], 0)
        self.assertEqual(manifest["bootstrap_observation"]["pending_semantic_authorization_paths"], [])

    def test_bootstrap_keeps_exact_source_diagnostic_pending_and_never_assigns_route_authority(self) -> None:
        source = self.report["input_inventory"]["inputs"][1]
        record = self.make_source_diagnostic_record(source, 2)
        self.report["records"][1] = record
        self.write_report()

        binding = self.validate_generated_bootstrap_bundle()
        manifest = json.loads(
            (self.output_dir / generator.GENERATION_MANIFEST_FILENAME).read_text(encoding="utf-8")
        )
        expectations = json.loads(
            (self.output_dir / generator.EXPECTATIONS_FILENAME).read_text(encoding="utf-8")
        )
        row = manifest["rows"][1]
        entry = expectations["inputs"][source["path"]]
        blocker = row["bootstrap_blocker"]

        self.assertEqual(binding["bootstrap_blocker_count"], 1)
        self.assertEqual(entry["class"], "unmapped")
        self.assertEqual(entry["source_sha256"], source["sha256"])
        self.assertIsNone(entry.get("authority"))
        self.assertEqual(row["class"], "unmapped")
        self.assertIsNone(row["test_id"])
        self.assertEqual(row["route_disposition"], "rejected")
        self.assertEqual(row["route_classification_status"], "inferred_pre_route_rejection")
        self.assertEqual(blocker["category"], "source_diagnostic_pending_semantic_authorization")
        self.assertEqual(blocker["semantic_authorization"]["status"], "pending")
        self.assertFalse(blocker["semantic_authorization"]["route_or_roster_authority"])
        self.assertIn("error[E0432]", blocker["exact_stderr_text"])
        self.assertEqual(
            manifest["bootstrap_observation"]["pending_semantic_authorization_paths"],
            [source["path"]],
        )

    def test_bootstrap_forged_route_rejection_cannot_promote_raw_source_diagnostic(self) -> None:
        prefix_by_variant = {
            "plain": "",
            "bom": "\ufeff",
            "combining_mark": "\u0301",
            "arbitrary_text": "diagnostic wrapper: ",
            "internal_zwsp": "",
            "unknown_mir_code": "",
        }
        for variant, diagnostic_prefix in prefix_by_variant.items():
            with self.subTest(variant=variant):
                shutil.rmtree(self.output_dir, ignore_errors=True)
                self.report = self.make_report()
                source = self.report["input_inventory"]["inputs"][1]
                record = self.make_source_diagnostic_record(source, 2)
                fixture = self.root / source["path"]
                route_reason = "closed canonical capability gap"
                route_line = f"default Canonical MIR route rejected: {route_reason}"
                source_code = "MIR-FAKE-999" if variant == "unknown_mir_code" else "E0432"
                if variant == "internal_zwsp":
                    source_diagnostic = (
                        f"error\u200b[E0432] {fixture}:2:3-8 unknown name 'absent'"
                    )
                elif variant == "unknown_mir_code":
                    source_diagnostic = (
                        f"error[MIR-FAKE-999] {fixture}:2:3-8 unknown name 'absent'"
                    )
                else:
                    source_diagnostic = (
                        diagnostic_prefix
                        + f"error[{source_code}] {fixture}:2:3-8 unknown name 'absent'"
                    )
                capability_diagnostic = (
                    f"error[MIR-COVERAGE-001] {fixture}:1:1-2:1 canonical route capability unavailable"
                )
                forged_stderr = "\n".join(
                    (route_line, source_diagnostic, capability_diagnostic, "")
                )
                record["stdout"] = ""
                record["stdout_raw"] = self.raw_capture(
                    b"", f"forged-{variant}.stdout.bin"
                )
                record["stderr"] = forged_stderr
                record["stderr_raw"] = self.raw_capture(
                    forged_stderr.encode("utf-8"), f"forged-{variant}.stderr.bin"
                )
                record["exit_code"] = 1
                record["route"] = self.census._route_observation(
                    "", forged_stderr, 1, "completed"
                )
                record["failure_classification"] = self.census._failure_classification(
                    run_status="completed",
                    exit_code=1,
                    text=forged_stderr,
                    target_kind=source["target_kind"],
                    route=record["route"],
                )
                record["per_function"] = self.census._function_observations(
                    "", forged_stderr, None, record["route"]
                )
                record["output_artifact"] = None
                record["rejection_artifact_evidence"] = dict(
                    record["source_diagnostic_artifact_evidence"]
                )
                record["candidate_classification"] = "intentional_reject"
                claimed_diagnostic_codes = sorted({source_code, "MIR-COVERAGE-001"})
                record["expectation"].update({
                    "class": "expected_build_rejection",
                    "actual_exit_code": 1,
                    "actual_route": "rejected",
                    "actual_route_classification_status": "observed",
                    "actual_route_reason": route_reason,
                    "actual_failure_class": "capability",
                    "actual_diagnostic_codes": claimed_diagnostic_codes,
                    "actual_output_artifact_present": False,
                    "expected_exit_code": 1,
                    "expected_diagnostic_codes": claimed_diagnostic_codes,
                    "required_output_substrings": [route_line, source_diagnostic, capability_diagnostic],
                    "expected_route_disposition": "rejected",
                    "expected_route_classification_status": "observed",
                    "expected_route_reason": route_reason,
                    "expected_failure_class": "capability",
                })
                self.report["records"][1] = record
                self.write_report()

                binding = self.validate_generated_bootstrap_bundle()
                manifest = json.loads(
                    (self.output_dir / generator.GENERATION_MANIFEST_FILENAME).read_text(encoding="utf-8")
                )
                expectations = json.loads(
                    (self.output_dir / generator.EXPECTATIONS_FILENAME).read_text(encoding="utf-8")
                )
                row = manifest["rows"][1]
                blocker = row["bootstrap_blocker"]
                expectation = expectations["inputs"][source["path"]]

                self.assertEqual(binding["bootstrap_blocker_count"], 1)
                self.assertEqual(row["path"], source["path"])
                self.assertEqual(row["source_sha256"], source["sha256"])
                self.assertEqual(row["class"], "unmapped")
                self.assertIsNone(row["test_id"])
                self.assertEqual(expectation["class"], "unmapped")
                self.assertEqual(expectation["source_sha256"], source["sha256"])
                self.assertIsNone(expectation.get("authority"))
                self.assertEqual(
                    blocker["category"], "source_diagnostic_pending_semantic_authorization"
                )
                self.assertEqual(blocker["semantic_authorization"]["status"], "pending")
                self.assertFalse(blocker["semantic_authorization"]["route_or_roster_authority"])
                self.assertEqual(blocker["route"]["disposition"], "rejected")
                self.assertEqual(blocker["failure_class"], "capability")
                self.assertEqual(blocker["candidate_classification"], "intentional_reject")
                self.assertEqual(blocker["exact_stderr_text"], forged_stderr)
                self.assertEqual(
                    blocker["stderr_sha256"],
                    hashlib.sha256(forged_stderr.encode("utf-8")).hexdigest(),
                )
                raw_line = (source_diagnostic + "\n").encode("utf-8")
                self.assertEqual(
                    blocker["raw_diagnostic_lines"],
                    [{
                        "stream": "stderr",
                        "text": source_diagnostic,
                        "bytes_base64": base64.b64encode(raw_line).decode("ascii"),
                        "bytes_sha256": hashlib.sha256(raw_line).hexdigest(),
                    }],
                )

    def test_bootstrap_rejects_record_reordering_and_frozen_denominator_mismatch(self) -> None:
        self.report["records"].reverse()
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "preserve exact inventory order"):
            self.run_bootstrap_generator()
        self.assertFalse(self.output_dir.exists())

        self.report = self.make_report()
        self.report["manifest"]["denominator"] += 1
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "manifest digest/denominator"):
            self.run_bootstrap_generator()
        self.assertFalse(self.output_dir.exists())

    def test_bootstrap_unstable_global_snapshot_preserves_all_rows_as_blockers(self) -> None:
        self.report["source_snapshot_stable"] = False
        self.write_report()

        manifest = self.run_bootstrap_generator()
        expectation_document = json.loads(
            (self.output_dir / generator.EXPECTATIONS_FILENAME).read_text(encoding="utf-8")
        )
        self.assertEqual(manifest["outcomes"]["unmapped"], 2)
        self.assertEqual(len(manifest["rows"]), 2)
        self.assertTrue(
            all(item["class"] == "unmapped" for item in expectation_document["inputs"].values())
        )
        self.assertTrue(
            all(row["bootstrap_blocker"]["category"] == "unstable" for row in manifest["rows"])
        )

    def test_bootstrap_preserves_compatibility_fallback_unknown_and_unmapped_as_blockers(self) -> None:
        cases = ("compatibility", "fallback", "unknown", "unmapped")
        for case in cases:
            with self.subTest(case=case):
                shutil.rmtree(self.output_dir, ignore_errors=True)
                self.report = self.make_report()
                record = self.report["records"][0]
                stderr = record["stderr"]
                if case == "compatibility":
                    stderr = stderr.replace(
                        "canonical route disposition: canonical (default-route) mir_digest=" + "a" * 64,
                        "canonical route disposition: legacy (legacy route)",
                    )
                    record["candidate_classification"] = "compatibility"
                elif case == "fallback":
                    stderr = stderr.replace(
                        "1/1 function(s), 0 fell back; mir_digest=",
                        "1/1 function(s), 1 fell back; mir_digest=",
                    )
                elif case == "unknown":
                    record["candidate_classification"] = "unknown"
                else:
                    record["expectation"]["class"] = "unmapped"
                    record["expectation"]["matched"] = False
                    record["candidate_classification"] = "unmapped"
                if case in {"compatibility", "fallback"}:
                    record["stderr"] = stderr
                    record["stderr_raw"] = self.raw_capture(stderr.encode(), f"bootstrap-{case}.stderr.bin")
                    record["route"] = self.census._route_observation(
                        record["stdout"], stderr, record["exit_code"], record["run_status"]
                    )
                    record["failure_classification"] = self.census._failure_classification(
                        run_status=record["run_status"],
                        exit_code=record["exit_code"],
                        text=stderr + "\n" + record["stdout"],
                        target_kind=record["target_kind"],
                        route=record["route"],
                    )
                    record["per_function"] = self.census._function_observations(
                        record["stdout"], stderr, record["dispatch_stats"], record["route"]
                    )
                self.write_report()
                manifest = self.run_bootstrap_generator()
                row = manifest["rows"][0]
                self.assertEqual(row["class"], "unmapped")
                self.assertIsNone(row["test_id"])
                self.assertEqual(manifest["outcomes"]["unmapped"], 1)
                self.assertEqual(row["bootstrap_blocker"]["status"], "blocking")

    def test_bootstrap_unobserved_and_unstable_rows_cannot_become_authorities(self) -> None:
        for case in ("unobserved", "unstable"):
            with self.subTest(case=case):
                shutil.rmtree(self.output_dir, ignore_errors=True)
                self.report = self.make_report()
                record = self.report["records"][0]
                if case == "unobserved":
                    record["run_status"] = "timed_out"
                    record["exit_code"] = None
                    record["route"] = self.census._route_observation(
                        record["stdout"], record["stderr"], None, "timed_out"
                    )
                    record["failure_classification"] = self.census._failure_classification(
                        run_status="timed_out",
                        exit_code=None,
                        text=record["stderr"] + "\n" + record["stdout"],
                        target_kind=record["target_kind"],
                        route=record["route"],
                    )
                    record["per_function"] = self.census._function_observations(
                        record["stdout"], record["stderr"], None, record["route"]
                    )
                else:
                    record["source_stable_for_input"] = False
                    record["source_hash_before_execution"] = "0" * 64
                self.write_report()
                manifest = self.run_bootstrap_generator()
                row = manifest["rows"][0]
                self.assertEqual(row["class"], "unmapped")
                self.assertIsNone(row["test_id"])
                self.assertEqual(manifest["outcomes"]["unmapped"], 1)
                self.assertEqual(row["bootstrap_blocker"]["category"], case if case == "unstable" else "unobserved")

    def test_generates_route_expectation_schema_v5_and_one_direct_authority_per_row(self) -> None:
        manifest = self.run_generator()
        self.assertEqual(manifest["status"], "generated_not_executed")
        rust_path = self.output_dir / generator.EXPECTED_TEST_PATH
        expectation_path = self.output_dir / generator.EXPECTATIONS_FILENAME
        rust = rust_path.read_text(encoding="utf-8")
        expectations = json.loads(expectation_path.read_text(encoding="utf-8"))
        self.assertEqual(expectations["schema_version"], 5)
        self.assertEqual(len(expectations["inputs"]), 2)
        self.assertEqual(rust.count("#[test]"), 2)
        self.assertNotIn("for ", rust)
        self.assertNotIn("fn run_", rust)
        self.assertIn(generator.AUTHORITY_MIMI_HELPER_SOURCE, rust)
        self.assertNotIn("CARGO_BIN_EXE_mimi", rust)
        self.assertIn("MIMI_ROUTE_CENSUS_MIMI_SHA256", rust)
        self.assertIn("Command::new(route_census_mimi_path())", rust)
        self.assertIn('.env("MIMI_ROUTE_CENSUS", "1")', rust)
        self.assertEqual(rust.count('.env("MIMI_ROUTE_CENSUS", "1")'), 2)
        self.assertIn("collector_observed_before_and_after_build", rust + json.dumps(manifest))
        for item in expectations["inputs"].values():
            self.assertEqual(item["authority"]["assertion"]["path"], generator.EXPECTED_TEST_PATH)
            self.assertEqual(item["authority"]["assertion"]["sha256"], self.digest(rust.encode()))
        self.assertEqual(
            manifest["outcomes"],
            {
                "canonical": 1,
                "expected_build_rejection": 1,
                "expected_source_diagnostic": 0,
                "expected_tool_diagnostic": 0,
                "canonical_mir_input_rejection": 0,
            },
        )
        self.assertEqual(
            manifest["artifacts"][generator.EXPECTED_TEST_PATH]["sha256"], self.digest(rust.encode())
        )
        rustc = shutil.which("rustc")
        if rustc is not None:
            env = os.environ.copy()
            env["CARGO_MANIFEST_DIR"] = str(self.root)
            env[generator.AUTHORITY_MIMI_PATH_ENV] = str(self.binary.resolve())
            env[generator.AUTHORITY_MIMI_SHA256_ENV] = self.digest(self.binary.read_bytes())
            result = subprocess.run(
                [rustc, "--edition=2021", "--test", str(rust_path), "-o", str(self.base / "authority-tests")],
                cwd=self.root,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_source_diagnostic_generation_uses_raw_text_as_a_distinct_authority(self) -> None:
        self.report = self.make_report()
        source = self.report["input_inventory"]["inputs"][1]
        self.report["records"][1] = self.make_source_diagnostic_record(source, 2)
        self.write_report()

        manifest = self.run_generator_with_source_diagnostic_proof_fixture()
        rust = (self.output_dir / generator.EXPECTED_TEST_PATH).read_text(encoding="utf-8")
        expectations = json.loads(
            (self.output_dir / generator.EXPECTATIONS_FILENAME).read_text(encoding="utf-8")
        )
        entry = expectations["inputs"][source["path"]]
        row = next(item for item in manifest["rows"] if item["path"] == source["path"])

        self.assertEqual(entry["class"], "expected_source_diagnostic")
        self.assertEqual(entry["source_sha256"], source["sha256"])
        self.assertIn("error[E0432]", entry["expected_diagnostic_text"])
        self.assertEqual(entry["expected_diagnostic_codes"], ["E0432"])
        self.assertEqual(entry["expected_diagnostic_lines"], [
            line for line in entry["expected_diagnostic_text"].splitlines()
            if "error[E0432]" in line
        ])
        self.assertTrue(entry["expected_output_path"].startswith(
            "target/route-census-source-diagnostic-"
        ))
        self.assertTrue(row["source_diagnostic_authority_evidence"][
            "direct_test_asserts_nonzero_mimi_exit"
        ])
        self.assertTrue(row["source_diagnostic_authority_evidence"][
            "direct_test_asserts_exact_stderr_and_empty_stdout"
        ])
        self.assertFalse(row["observed_route_rejection"])
        self.assertEqual(row["route_classification_status"], "inferred_pre_route_rejection")
        self.assertEqual(row["candidate_classification"], "source_diagnostic")
        self.assertEqual(
            manifest["outcomes"],
            {
                "canonical": 1,
                "expected_build_rejection": 0,
                "expected_source_diagnostic": 1,
                "expected_tool_diagnostic": 0,
                "canonical_mir_input_rejection": 0,
            },
        )
        source_test = rust[rust.index(f"fn {row['test_id']}()") :]
        self.assertIn("::std::assert!(!output.status.success());", source_test)
        self.assertIn("::std::assert!(output.stdout.is_empty());", source_test)
        self.assertIn("::std::assert_eq!(output.stderr.as_slice(), expected_stderr.as_slice());", source_test)
        self.assertIn("::std::assert_eq!(output.status.code(), Some(1));", source_test)
        self.assertIn(".replace(\"@MIMI_SOURCE_PATH@\", &fixture.display().to_string()).into_bytes();", source_test)
        self.assertNotIn("String::from_utf8_lossy(&output.stderr)", source_test)
        self.assertIn("output_path.symlink_metadata()", source_test)
        self.assertIn('::std::assert!(!stderr.contains("canonical route disposition:"));', source_test)
        self.assertNotIn("fn route_census_run_", rust)

    def test_raw_source_diagnostic_parser_accepts_complete_build_summaries_only(self) -> None:
        fixture = self.root / "fixtures" / "source-error.mimi"
        fixture.write_text("func main() { }\n", encoding="utf-8")
        absolute_source = str(fixture.resolve())
        type_stderr = (
            f"{absolute_source} has 2 type error(s):\n"
            f"error[E0432] {absolute_source}:1:1-2 first error\n"
            f"error[E0432] {absolute_source}:1:3-4 second error\n"
            "error: type checking failed\n"
        )
        parse_stderr = (
            f"error {absolute_source}:1:1-2 unexpected token\n"
            "error: 1 parse error(s) found\n"
        )

        type_result = generator._source_diagnostic_from_raw_logs(
            type_stderr.encode("utf-8"),
            source_path="fixtures/source-error.mimi",
            root=self.root,
        )
        parse_result = generator._source_diagnostic_from_raw_logs(
            parse_stderr.encode("utf-8"),
            source_path="fixtures/source-error.mimi",
            root=self.root,
        )
        self.assertEqual(len(type_result["lines"]), 2)
        self.assertEqual(type_result["codes"], ["E0432"])
        self.assertEqual(len(parse_result["lines"]), 1)
        self.assertEqual(parse_result["codes"], [])

        invalid_outputs = (
            type_stderr.replace("has 2 type error(s)", "has 3 type error(s)"),
            parse_stderr.replace("parse error(s) found", "parse error(s)"),
            type_stderr.replace(absolute_source, str(self.root / "fixtures" / "other.mimi"), 1),
            type_stderr.replace("type checking failed", "type checking failed\nextra"),
        )
        for invalid in invalid_outputs:
            with self.subTest(invalid=invalid):
                with self.assertRaisesRegex(
                    generator.GenerationError,
                    "raw stderr contains an unknown, extra or unbound diagnostic",
                ):
                    generator._source_diagnostic_from_raw_logs(
                        invalid.encode("utf-8"),
                        source_path="fixtures/source-error.mimi",
                        root=self.root,
                    )

    def test_source_diagnostic_generation_rejects_extra_or_unknown_raw_diagnostics(self) -> None:
        self.report = self.make_report()
        source = self.report["input_inventory"]["inputs"][1]
        record = self.make_source_diagnostic_record(source, 2)
        extra = (
            record["stderr"]
            + f"error[E9999] {self.root / source['path']}:9:1-2 extra error\n"
        )
        record["stderr"] = extra
        record["stderr_raw"] = self.raw_capture(extra.encode("utf-8"), "000002.stderr.bin")
        record["route"] = self.census._route_observation("", extra, 1, "completed")
        record["failure_classification"] = self.census._failure_classification(
            run_status="completed",
            exit_code=1,
            text=extra,
            target_kind=source["target_kind"],
            route=record["route"],
        )
        record["expectation"]["actual_diagnostic_codes"] = ["E0432", "E9999"]
        record["per_function"] = self.census._function_observations(
            "", extra, None, record["route"]
        )
        self.report["records"][1] = record
        self.write_report()

        with self.assertRaisesRegex(generator.GenerationError, "typed source-diagnostic expectation|raw stderr"):
            self.run_generator_with_source_diagnostic_proof_fixture()
        self.assertFalse(self.output_dir.exists())

    def test_source_diagnostic_generation_rejects_invalid_utf8_replacement_collision(self) -> None:
        self.report = self.make_report()
        source = self.report["input_inventory"]["inputs"][1]
        record = self.make_source_diagnostic_record(source, 2)
        replacement_text = record["stderr"].replace("absent", "\uFFFD")
        expected_utf8 = replacement_text.encode("utf-8")
        invalid_utf8 = expected_utf8.replace("\uFFFD".encode("utf-8"), b"\xff", 1)
        self.assertEqual(
            invalid_utf8.decode("utf-8", errors="replace"), replacement_text
        )
        self.assertNotEqual(invalid_utf8, expected_utf8)

        record["stderr"] = replacement_text
        record["stderr_raw"] = self.raw_capture(invalid_utf8, "000002.stderr.bin")
        record["route"] = self.census._route_observation(
            "", replacement_text, 1, "completed"
        )
        record["failure_classification"] = self.census._failure_classification(
            run_status="completed",
            exit_code=1,
            text=replacement_text,
            target_kind=source["target_kind"],
            route=record["route"],
        )
        record["per_function"] = self.census._function_observations(
            "", replacement_text, None, record["route"]
        )
        record["expectation"].update({
            "expected_diagnostic_text": replacement_text,
            "expected_source_diagnostic_text": replacement_text,
            "expected_diagnostic_lines": self.census._source_diagnostic_lines(
                replacement_text
            ),
        })
        record["expectation"]["diagnostic_contract"].update({
            "actual_diagnostic_text": replacement_text,
            "expected_diagnostic_text": replacement_text,
            "actual_diagnostic_lines": self.census._source_diagnostic_lines(
                replacement_text
            ),
            "expected_diagnostic_lines": self.census._source_diagnostic_lines(
                replacement_text
            ),
            "diagnostic_text_matches_exactly": True,
        })
        self.report["records"][1] = record
        self.write_report()

        with self.assertRaisesRegex(
            generator.GenerationError, "source diagnostic stderr is not complete UTF-8"
        ):
            self.run_generator_with_source_diagnostic_proof_fixture()
        self.assertFalse(self.output_dir.exists())

    def test_source_diagnostic_generation_rejects_route_fallback_and_output_artifacts(self) -> None:
        self.report = self.make_report()
        source = self.report["input_inventory"]["inputs"][1]
        base_record = self.make_source_diagnostic_record(source, 2)
        mutations = {
            "route_marker": lambda record: self._append_source_diagnostic_text(
                record,
                "default Canonical MIR route rejected: forged route\n",
            ),
            "fallback_marker": lambda record: self._append_source_diagnostic_text(
                record,
                "warning: legacy fallback used\n",
            ),
            "output_artifact": lambda record: self._set_source_diagnostic_output_artifact(record),
            "output_path_created": lambda record: record[
                "source_diagnostic_artifact_evidence"
            ]["after"].update({"exists": True, "sha256": "f" * 64}),
        }
        for label, mutate in mutations.items():
            with self.subTest(label=label):
                self.output_dir = self.base / f"generated-{label}"
                self.report = self.make_report()
                record = self.make_source_diagnostic_record(source, 2)
                mutate(record)
                self.report["records"][1] = record
                self.write_report()
                with self.assertRaises(generator.GenerationError):
                    self.run_generator_with_source_diagnostic_proof_fixture()
                self.assertFalse(self.output_dir.exists())

    def _append_source_diagnostic_text(self, record: dict, suffix: str) -> None:
        text = record["stderr"] + suffix
        record["stderr"] = text
        record["stderr_raw"] = self.raw_capture(text.encode("utf-8"), "000002.stderr.bin")
        record["route"] = self.census._route_observation("", text, 1, "completed")
        record["failure_classification"] = self.census._failure_classification(
            run_status="completed",
            exit_code=1,
            text=text,
            target_kind=record["target_kind"],
            route=record["route"],
        )
        record["expectation"].update({
            "actual_route": record["route"]["disposition"],
            "actual_route_classification_status": record["route"]["classification_status"],
            "actual_route_reason": record["route"].get("reason"),
            "actual_failure_class": record["failure_classification"]["class"],
        })
        record["per_function"] = self.census._function_observations(
            "", text, None, record["route"]
        )

    @staticmethod
    def _set_source_diagnostic_output_artifact(record: dict) -> None:
        record["output_artifact"] = {"sha256": "f" * 64, "bytes": 1}
        record["expectation"]["actual_output_artifact_present"] = True

    def test_immutable_include_wrapper_requires_explicit_generated_source(self) -> None:
        self.run_generator()
        rustc = shutil.which("rustc")
        if rustc is None:
            self.skipTest("rustc is unavailable for compile-time include smoke test")
        wrapper = self.root / generator.EXPECTED_TEST_PATH
        generated = self.output_dir / generator.EXPECTED_TEST_PATH
        env = os.environ.copy()
        env["CARGO_MANIFEST_DIR"] = str(self.root)
        env[generator.AUTHORITY_MIMI_PATH_ENV] = str(self.binary.resolve())
        env[generator.AUTHORITY_MIMI_SHA256_ENV] = self.digest(self.binary.read_bytes())
        env[generator.AUTHORITY_SOURCE_ENV] = str(generated.resolve())
        included = subprocess.run(
            [rustc, "--edition=2021", "--test", str(wrapper), "-o", str(self.base / "wrapper-tests")],
            cwd=self.root,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(included.returncode, 0, included.stderr)

        env.pop(generator.AUTHORITY_SOURCE_ENV)
        missing = subprocess.run(
            [rustc, "--edition=2021", "--test", str(wrapper), "-o", str(self.base / "missing-env-tests")],
            cwd=self.root,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn(generator.AUTHORITY_SOURCE_ENV, missing.stderr)

    def test_duplicate_generated_authority_test_id_is_refused(self) -> None:
        with patch.object(generator, "_test_id", return_value="duplicate_authority"):
            with self.assertRaisesRegex(generator.GenerationError, "duplicate generated authority test id"):
                self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_rejection_authority_without_route_telemetry_is_refused(self) -> None:
        original_renderer = generator._render_expected_rejection_test

        def without_telemetry(*args, **kwargs):
            source, assertion_line = original_renderer(*args, **kwargs)
            source = source.replace('        .env("MIMI_ROUTE_CENSUS", "1")\n', "")
            return source, assertion_line

        generator._render_expected_rejection_test = without_telemetry
        try:
            with self.assertRaisesRegex(
                generator.GenerationError,
                "generated rejection test does not satisfy strict v4 recognizer",
            ):
                self.run_generator()
        finally:
            generator._render_expected_rejection_test = original_renderer
        self.assertFalse(self.output_dir.exists())

    def test_rejection_diagnostic_only_on_stdout_is_refused(self) -> None:
        record = self.report["records"][1]
        diagnostic = next(
            line for line in record["stderr"].splitlines() if "MIR-COVERAGE-001" in line
        )
        stderr = "\n".join(
            line for line in record["stderr"].splitlines() if line != diagnostic
        ) + "\n"
        stdout = diagnostic + "\n"
        record["stderr"] = stderr
        record["stdout"] = stdout
        record["stderr_raw"] = self.raw_capture(stderr.encode(), "000002.stderr.bin")
        record["stdout_raw"] = self.raw_capture(stdout.encode(), "000002.stdout.bin")
        self.write_report()

        with self.assertRaisesRegex(generator.GenerationError, "stderr line"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_inventory_only_report_is_refused_without_output(self) -> None:
        self.report["collection_mode"] = "inventory_only"
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "full_execution"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_previous_schema_v8_report_is_refused_without_output(self) -> None:
        self.report["schema_version"] = "0.1.11-full-route-census-v8"
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "schema_version must be .*v9"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_collector_hash_mismatch_is_refused_without_output(self) -> None:
        self.report["collector"]["sha256"] = "0" * 64
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "collector path/hash"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_mimi_binary_hash_mismatch_is_refused_without_output(self) -> None:
        self.report["toolchain"]["mimi"]["sha256"] = "0" * 64
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "not the same current executable"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_inventory_content_mismatch_is_refused_without_output(self) -> None:
        self.report["input_inventory"]["inputs"][0]["bytes"] += 1
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "exact current inventory"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_stale_candidate_source_hash_is_refused_without_output(self) -> None:
        (self.root / "fixtures" / "canonical.mimi").write_text(
            "func main() -> i32 { return 9 }\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(generator.GenerationError, "manifest digest|candidate/source-scope ledger|snapshot"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_duplicate_or_missing_record_path_is_refused(self) -> None:
        self.report["records"][1]["path"] = self.report["records"][0]["path"]
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "duplicate, missing or extra"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_compatibility_route_is_refused(self) -> None:
        def mutate(report):
            record = report["records"][0]
            record["candidate_classification"] = "compatibility"
            record["route"] = {
                "disposition": "compatibility",
                "classification_status": "observed",
                "reason": "legacy route",
                "mir_digest": None,
                "route_marker_count": 1,
                "observations": [{"disposition": "compatibility", "reason": "legacy route"}],
            }
            record["stderr"] = "canonical route disposition: legacy (legacy route)\n"
            record["stderr_raw"] = self.raw_capture(record["stderr"].encode(), "compat.stderr.bin")
            record["expectation"].update({
                "actual_route": "compatibility",
                "actual_route_classification_status": "observed",
                "actual_route_reason": "legacy route",
            })
        self.mutate_and_write(mutate)
        with self.assertRaisesRegex(generator.GenerationError, "cannot be reproduced|disallowed route disposition"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_inferred_pre_route_rejection_is_refused(self) -> None:
        def mutate(report):
            record = report["records"][1]
            record["route"]["classification_status"] = "inferred_pre_route_rejection"
            record["candidate_classification"] = "unknown"
        self.mutate_and_write(mutate)
        with self.assertRaisesRegex(generator.GenerationError, "route observation cannot be reproduced"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_unmapped_unknown_record_is_refused(self) -> None:
        def mutate(report):
            record = report["records"][1]
            record["candidate_classification"] = "unknown"
            record["expectation"]["class"] = "unmapped"
            record["expectation"]["matched"] = False
        self.mutate_and_write(mutate)
        with self.assertRaisesRegex(generator.GenerationError, "not explicitly mapped"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_disallowed_source_diagnostic_rejection_is_refused(self) -> None:
        def mutate(report):
            record = report["records"][1]
            record["expectation"]["expected_failure_class"] = "source_diagnostic"
        self.mutate_and_write(mutate)
        with self.assertRaisesRegex(generator.GenerationError, "allowed observed rejection"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_missing_output_absence_proof_is_refused(self) -> None:
        self.report["records"][1]["rejection_artifact_evidence"]["after"]["exists"] = True
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "exact-path absence"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_missing_rejection_artifact_observation_is_refused(self) -> None:
        self.report["records"][1].pop("rejection_artifact_evidence")
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "explicit before/after output artifact evidence"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_incomplete_canonical_roster_is_refused(self) -> None:
        self.report["records"][0]["per_function"]["has_fallback"] = True
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "per-function receipt field|canonical route receipt/roster"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_rejection_artifact_evidence_must_bind_command_destination(self) -> None:
        self.report["records"][1]["rejection_artifact_evidence"]["command_output_path"] = str(
            self.base / "forged-output"
        )
        self.write_report()
        with self.assertRaisesRegex(generator.GenerationError, "isolated exact build destination"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_output_long_flag_is_parsed_and_accepted(self) -> None:
        record = self.report["records"][1]
        record["command"][-2] = "--output"
        self.write_report()
        manifest = self.run_generator()
        self.assertEqual(manifest["status"], "generated_not_executed")

    def test_missing_registered_optional_target_is_refused(self) -> None:
        (self.root / "Cargo.toml").write_text(
            "[package]\nname='mimi'\nversion='0.1.11-dev'\nedition='2021'\n[features]\ndefault=[]\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(generator.GenerationError, "optional feature"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_authority_target_requires_exact_path_name_feature_and_wrapper(self) -> None:
        cargo_path = self.root / "Cargo.toml"
        original_cargo_toml = cargo_path.read_text(encoding="utf-8")
        cases = (
            (
                "wrong_path",
                lambda text: text.replace(
                    'path = "tests/route-census/authorities.rs"',
                    'path = "tests/route-census/other.rs"',
                ),
                "exactly one unambiguous",
            ),
            (
                "wrong_feature",
                lambda text: text.replace(
                    'required-features = ["route-census-authorities"]',
                    'required-features = ["some-other-feature"]',
                ),
                "required-features",
            ),
            (
                "wrong_name",
                lambda text: text.replace(
                    'name = "route-census-authorities"',
                    'name = "ordinary-integration-tests"',
                ),
                "exactly one unambiguous",
            ),
            (
                "extra_registration",
                lambda text: text + '\n[[test]]\nname = "route-census-authorities"\npath = "tests/route-census/other.rs"\nrequired-features = ["route-census-authorities"]\n',
                "exactly one unambiguous",
            ),
        )
        for name, mutate, message in cases:
            with self.subTest(case=name):
                cargo_path.write_text(original_cargo_toml, encoding="utf-8")
                cargo_path.write_text(mutate(cargo_path.read_text(encoding="utf-8")), encoding="utf-8")
                with self.assertRaisesRegex(generator.GenerationError, message):
                    self.run_generator()
                self.assertFalse(self.output_dir.exists())

        cargo_path.write_text(original_cargo_toml, encoding="utf-8")
        wrapper = self.root / generator.EXPECTED_TEST_PATH
        wrapper.write_text("// mutable placeholder\n", encoding="utf-8")
        with self.assertRaisesRegex(generator.GenerationError, "immutable generated-source include wrapper"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_collector_accepts_fresh_bundle_and_rejects_stale_outputs(self) -> None:
        binding = self.validate_generated_bundle()
        self.assertEqual(binding["status"], "validated")
        self.assertEqual(binding["authority_source_path"], str((self.output_dir / generator.EXPECTED_TEST_PATH).resolve()))
        self.assertEqual(binding["target_path"], generator.EXPECTED_TEST_PATH)
        self.assertEqual(binding["feature"], generator.EXPECTED_FEATURE_NAME)

        source_path = self.output_dir / generator.EXPECTED_TEST_PATH
        source_path.write_bytes(source_path.read_bytes() + b"// stale mutation\n")
        tracked_paths, _tracked = self.census.tracked_mimi_paths(self.root)
        sources, _exclusions, _scope = self.census.enumerate_source_scope(self.root, tracked_paths)
        with self.assertRaisesRegex(ValueError, "artifact identity is stale"):
            self.census._validate_route_authority_bundle(
                self.output_dir, root=self.root, sources=sources, mimi_path=self.binary
            )

    def test_collector_rejects_mismatched_missing_empty_and_placeholder_artifacts(self) -> None:
        self.run_generator()
        expectations_path = self.output_dir / generator.EXPECTATIONS_FILENAME
        expectations = json.loads(expectations_path.read_text(encoding="utf-8"))
        first_key = next(iter(expectations["inputs"]))
        expectations["inputs"][first_key]["source_sha256"] = "0" * 64
        expectations_path.write_text(
            json.dumps(expectations, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
            encoding="utf-8",
        )
        tracked_paths, _tracked = self.census.tracked_mimi_paths(self.root)
        sources, _exclusions, _scope = self.census.enumerate_source_scope(self.root, tracked_paths)
        def validate_bundle():
            self.census._validate_route_authority_bundle(
                self.output_dir, root=self.root, sources=sources, mimi_path=self.binary
            )

        with self.assertRaisesRegex(ValueError, "artifact identity is stale"):
            validate_bundle()

        # Recreate the clean bundle, then prove required metadata and nonempty outputs.
        shutil.rmtree(self.output_dir)
        self.run_generator()
        (self.output_dir / generator.GENERATION_MANIFEST_FILENAME).unlink()
        with self.assertRaisesRegex(ValueError, "bundle is incomplete or contains unexpected artifacts"):
            validate_bundle()

        shutil.rmtree(self.output_dir)
        self.run_generator()
        self._refresh_generated_source_artifact_identity(b"")
        with self.assertRaisesRegex(ValueError, "empty or still a placeholder"):
            validate_bundle()

        shutil.rmtree(self.output_dir)
        self.run_generator()
        self._refresh_generated_source_artifact_identity(b"// route-census-authorities must be replaced\n")
        with self.assertRaisesRegex(ValueError, "empty or still a placeholder"):
            validate_bundle()

    def test_authority_feature_with_dependencies_is_refused(self) -> None:
        cargo_path = self.root / "Cargo.toml"
        cargo_path.write_text(
            cargo_path.read_text(encoding="utf-8").replace(
                "route-census-authorities = []",
                'route-census-authorities = ["some-dependency"]',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(generator.GenerationError, "empty optional feature"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())

    def test_authority_feature_enabled_transitively_by_default_is_refused(self) -> None:
        cargo_path = self.root / "Cargo.toml"
        cargo_path.write_text(
            cargo_path.read_text(encoding="utf-8").replace(
                "default = []",
                'default = ["ordinary"]\nordinary = ["route-census-authorities"]',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(generator.GenerationError, "transitively by default"):
            self.run_generator()
        self.assertFalse(self.output_dir.exists())


if __name__ == "__main__":
    unittest.main()
