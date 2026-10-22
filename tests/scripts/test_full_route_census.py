from __future__ import annotations

import base64
import json
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
import full_route_census as census


class InventoryTests(unittest.TestCase):
    def test_manifest_contains_tracked_missing_and_untracked_sources(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "tests/real_world").mkdir(parents=True)
            (root / "std").mkdir()
            (root / "target/ignored").mkdir(parents=True)
            (root / "tests/real_world/app.mimi").write_text(
                "func main() -> i32 { 0 }\n", encoding="utf-8"
            )
            (root / "std/list.mimi").write_text(
                "pub func size(xs: List<i32>) -> i32 { 0 }\n", encoding="utf-8"
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
                "std/list.mimi",
                "target/intentional.mimi",
                "tests/real_world/alias.mimi",
                "tests/real_world/app.mimi",
                "tests/real_world/deleted.mimi",
            ],
        )
        self.assertEqual(records[0]["category"], "stdlib_source")
        self.assertEqual(records[0]["target_kind"], "shared_library_probe")
        self.assertEqual(records[1]["presence_kind"], "missing_tracked_path")
        self.assertFalse(records[1]["present"])
        self.assertTrue(records[1]["tracked_by_git"])
        self.assertEqual(records[2]["presence_kind"], "symlink")
        self.assertEqual(records[3]["category"], "real_world_test")
        self.assertEqual(records[3]["target_kind"], "executable")
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


class StrictGateTests(unittest.TestCase):
    @staticmethod
    def _record(path: str, *, route: str = "canonical", fallback: bool = False) -> dict:
        stdout = b"out"
        stderr = b"err"
        return {
            "path": path,
            "category": "test_source",
            "present": True,
            "tracked_by_git": False,
            "run_status": "completed",
            "exit_code": 0,
            "route": {"disposition": route},
            "failure_classification": {"class": "none", "status": "not_applicable"},
            "expectation": {"matched": True},
            "per_function": {"named_roster_complete": True, "has_fallback": fallback},
            "output_artifact": {"sha256": "a" * 64, "bytes": 6},
            "stdout_raw": {
                "path": "000001.stdout.bin",
                "bytes": len(stdout),
                "sha256": census.sha256_bytes(stdout),
                "base64": base64.b64encode(stdout).decode(),
            },
            "stderr_raw": {
                "path": "000001.stderr.bin",
                "bytes": len(stderr),
                "sha256": census.sha256_bytes(stderr),
                "base64": base64.b64encode(stderr).decode(),
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
    ) -> dict:
        actor_manifest = [
            {"path": path, "present": True, "tracked_by_git": False}
            for path in census.ACTOR_FALLBACK_INPUTS
        ]
        actor_records = [cls._record(path) for path in census.ACTOR_FALLBACK_INPUTS]
        manifest_by_path = {row["path"]: row for row in actor_manifest}
        record_by_path = {row["path"]: row for row in actor_records}
        for path, record in zip(paths, records):
            manifest_by_path[path] = {"path": path, "present": True, "tracked_by_git": False}
            record_by_path[path] = record
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
        actors = census._actor_fallback_evidence(manifest, all_records)
        owners = [
            {
                "name": name,
                "production_accessor_call_sites": 0,
                "production_access_status": "sealed",
                "dependency_class": "fixture-dependency",
                "owner_deletion_ready": False,
                "owner_deletion_blocker": "retained compatibility evidence",
                "deletion_condition": "remove after explicit migration completion",
                "deletion_condition_sha256": "d" * 64,
            }
            for name in census.LEGACY_OWNER_NAMES
        ]
        owner_audit = {
            "accepted_as_complete_inventory": True,
            "owner_count_reported": len(owners),
            "owner_deletion_condition_set_sha256": "e" * 64,
            "owners": owners,
        }
        return census._gate_report(all_records, manifest, True, [], tracked, actors, owner_audit)

    def test_gate_can_pass_only_for_exact_canonical_zero_fallback_roster(self) -> None:
        gate = self._gate(["tests/a.mimi"], [self._record("tests/a.mimi")])
        self.assertTrue(gate["passed"])

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
        self.assertFalse(gate["checks"]["all_whole_program_routes_are_canonical"])
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
        self.assertFalse(gate["checks"]["all_builds_succeeded"])
        self.assertFalse(gate["checks"]["all_outcomes_match_expectations"])
        self.assertFalse(gate["checks"]["all_whole_program_routes_are_canonical"])

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
        self.assertFalse(gate["checks"]["all_builds_succeeded"])
        self.assertFalse(gate["checks"]["all_outcomes_match_expectations"])
        self.assertTrue(gate["checks"]["all_whole_program_routes_are_canonical"])

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

    def test_missing_actor_record_or_owner_row_fails_closed(self) -> None:
        manifest = [
            {"path": path, "present": True, "tracked_by_git": False}
            for path in census.ACTOR_FALLBACK_INPUTS
        ]
        records = [self._record(path) for path in census.ACTOR_FALLBACK_INPUTS]
        actors = census._actor_fallback_evidence(manifest, records[:1])
        owner_audit = {
            "accepted_as_complete_inventory": True,
            "owner_count_reported": 4,
            "owner_deletion_condition_set_sha256": "e" * 64,
            "owners": [],
        }
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
        self.assertFalse(gate["checks"]["all_four_legacy_owners_are_audited"])


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

            def fake_run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[bytes]:
                env = kwargs["env"]
                assert isinstance(env, dict)
                Path(command[-1]).write_bytes(b"binary")
                return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr=stderr)

            with patch.object(census.subprocess, "run", side_effect=fake_run):
                record = census._run_one(
                    root=root,
                    source=census.source_record(source_path, root),
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
            self.assertTrue(record["expectation"]["matched"])
            self.assertTrue(record["per_function"]["named_roster_complete"])
            self.assertEqual(record["scratch_cleanup"]["status"], "removed")
            self.assertFalse((scratch / "input-000001").exists())

    def test_explicit_expected_rejection_must_match_exit_and_route(self) -> None:
        expectation = {"case.mimi": {"exit_code": 1, "route": "rejected", "reason": "negative test"}}
        route = {"disposition": "rejected"}
        result = census._expectation_result("case.mimi", 1, route, expectation)
        self.assertTrue(result["matched"])
        mismatch = census._expectation_result("case.mimi", 0, route, expectation)
        self.assertFalse(mismatch["matched"])


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
