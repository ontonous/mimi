import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import dispatch_stat


class CompileOutcomeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.src = self.root / "case.mimi"
        self.src.write_text("func main() -> i32 { 0 }\n", encoding="utf-8")
        self.out_dir = self.root / "out"
        self.out_dir.mkdir()
        self.build_tmpdir = self.root / "build-tmp"
        self.build_tmpdir.mkdir()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def run_mocked(self, completed=None, side_effect=None):
        with patch.object(dispatch_stat, "mimi_binary", return_value=Path("mimi")):
            with patch.object(
                dispatch_stat.subprocess,
                "run",
                return_value=completed,
                side_effect=side_effect,
            ):
                return dispatch_stat.compile_with_stat(
                    self.src,
                    self.out_dir,
                    self.build_tmpdir,
                )

    def test_build_failure_retains_exit_code_and_bounded_diagnostic(self) -> None:
        proc = dispatch_stat.subprocess.CompletedProcess(
            args=["mimi", "build"],
            returncode=2,
            stdout="",
            stderr="front end rejected input\nE0432: invalid type\n",
        )
        stats, outcome = self.run_mocked(completed=proc)
        self.assertIsNone(stats)
        self.assertEqual(outcome["status"], "build_failed")
        self.assertEqual(outcome["exit_code"], 2)
        self.assertIn("E0432: invalid type", outcome["diagnostic"])

    def test_success_without_stats_and_invalid_stats_are_distinct(self) -> None:
        proc = dispatch_stat.subprocess.CompletedProcess(
            args=["mimi", "build"], returncode=0, stdout="", stderr=""
        )
        stats, outcome = self.run_mocked(completed=proc)
        self.assertIsNone(stats)
        self.assertEqual(outcome["status"], "stats_missing")

        (self.out_dir / "src-invalid.json").write_text("{broken", encoding="utf-8")
        stats, outcome = self.run_mocked(completed=proc)
        self.assertIsNone(stats)
        self.assertEqual(outcome["status"], "stats_invalid_json")
        self.assertEqual(outcome["stats_file"], "src-invalid.json")

    def test_successful_stats_are_bound_to_input_path(self) -> None:
        def build_and_write(*args, **kwargs):
            output = Path(args[0][-1]).parent
            (output / "src-case.json").write_text(
                json.dumps({"eligible": 1, "legacy_fallback": 0}),
                encoding="utf-8",
            )
            return dispatch_stat.subprocess.CompletedProcess(
                args=args[0], returncode=0, stdout="", stderr=""
            )

        with patch.object(dispatch_stat, "ROOT", self.root):
            stats, outcome = self.run_mocked(side_effect=build_and_write)
        self.assertEqual(outcome["status"], "stats_available")
        self.assertEqual(outcome["program"], "case.mimi")
        self.assertEqual(stats["program"], "case.mimi")
        self.assertEqual(stats["eligible"], 1)

    def test_timeout_preserves_captured_diagnostics(self) -> None:
        timeout = dispatch_stat.subprocess.TimeoutExpired(
            cmd=["mimi", "build"],
            timeout=120,
            output=b"partial stdout",
            stderr=b"still compiling",
        )
        stats, outcome = self.run_mocked(side_effect=timeout)
        self.assertIsNone(stats)
        self.assertEqual(outcome["status"], "timeout")
        self.assertIn("still compiling", outcome["diagnostic"])

    def test_build_census_enables_route_trace_without_redefining_verbose(self) -> None:
        completed = dispatch_stat.subprocess.CompletedProcess(
            args=["mimi", "build"], returncode=0, stdout="", stderr=""
        )
        with patch.object(dispatch_stat, "mimi_binary", return_value=Path("mimi")):
            with patch.object(
                dispatch_stat.subprocess,
                "run",
                return_value=completed,
            ) as run:
                dispatch_stat.compile_with_stat(
                    self.src,
                    self.out_dir,
                    self.build_tmpdir,
                )
        env = run.call_args.kwargs["env"]
        self.assertEqual(env["MIMI_VERBOSE"], "1")
        self.assertEqual(env["MIMI_ROUTE_CENSUS"], "1")

    def test_route_observation_distinguishes_canonical_legacy_and_rejected(self) -> None:
        canonical = dispatch_stat._route_observation(
            "",
            "canonical route disposition: canonical (default-route) mir_digest=" + "a" * 64,
        )
        self.assertEqual(canonical["route_disposition"], "canonical")
        self.assertEqual(canonical["route_reason"], "default-route")
        self.assertEqual(canonical["mir_digest"], "a" * 64)

        legacy = dispatch_stat._route_observation(
            "", "canonical route disposition: legacy (outside-migrated-profile)"
        )
        self.assertEqual(legacy["route_disposition"], "legacy")
        self.assertEqual(legacy["route_reason"], "outside-migrated-profile")

        rejected = dispatch_stat._route_observation(
            "", "error: default Canonical MIR route rejected: invalid profile"
        )
        self.assertEqual(rejected["route_disposition"], "rejected")
        self.assertEqual(
            rejected["route_reason"], "default-canonical-mir-route-rejected"
        )

    def test_route_without_telemetry_remains_unobserved(self) -> None:
        route = dispatch_stat._route_observation("Compiled executable", "")
        self.assertEqual(route["route_disposition"], "unobserved")


class CensusSafetyTests(unittest.TestCase):
    def test_unavailable_inputs_fail_the_census_gate(self) -> None:
        outcomes = [
            {"program": "complete.mimi", "status": "stats_available"},
            {"program": "unknown.mimi", "status": "stats_missing"},
        ]
        with patch.object(dispatch_stat.sys, "stderr") as stderr:
            self.assertTrue(dispatch_stat._report_unavailable_gate(outcomes))
        self.assertEqual(
            [item["program"] for item in dispatch_stat._unavailable_outcomes(outcomes)],
            ["unknown.mimi"],
        )
        self.assertTrue(stderr.write.called)

    def test_outcome_report_summarizes_routes_and_refuses_overwrite(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "outcomes.json"
            outcomes = [
                {
                    "status": "stats_available",
                    "route_disposition": "legacy",
                    "dispatch_summary": {
                        "total_functions": 4,
                        "eligible": 3,
                        "legacy_fallback": 1,
                        "emit_failed": 0,
                    },
                },
                {"status": "stats_missing", "route_disposition": "canonical"},
                {"status": "build_failed", "route_disposition": "rejected"},
            ]
            dispatch_stat._write_outcomes(path, outcomes, input_count=3)
            report = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(report["schema_version"], "0.41-dispatch-probe-outcomes-v3")
            self.assertEqual(
                report["route_counts"],
                {"canonical": 1, "legacy": 1, "rejected": 1},
            )
            self.assertEqual(report["dispatch_totals"]["instrumented_programs"], 1)
            self.assertEqual(
                report["dispatch_totals"],
                {
                    "instrumented_programs": 1,
                    "total_functions": 4,
                    "eligible": 3,
                    "legacy_fallback": 1,
                    "emit_failed": 0,
                },
            )
            with self.assertRaises(FileExistsError):
                dispatch_stat._write_outcomes(path, outcomes, input_count=3)

    def test_collect_all_does_not_delete_fixed_shared_temp_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "target"
            shared = target / "dispatch-stat-tmp"
            shared.mkdir(parents=True)
            marker = shared / "keep.txt"
            marker.write_text("pre-existing", encoding="utf-8")
            mimi = target / "debug" / "mimi"
            mimi.parent.mkdir(parents=True)
            mimi.write_text("placeholder", encoding="utf-8")
            source = root / "sample.mimi"
            source.write_text("func main() -> i32 { 0 }\n", encoding="utf-8")
            fake_stats = {"eligible": 1, "total_functions": 1, "legacy_fallback": 0}
            fake_outcome = {
                "program": "sample.mimi",
                "status": "stats_available",
                "phase": "dispatch-stats",
                "exit_code": 0,
                "elapsed_ms": 1.0,
                "stats_file": "src-sample.json",
                "diagnostic": "",
            }

            with (
                patch.object(dispatch_stat, "ROOT", root),
                patch.object(dispatch_stat, "corpus", return_value=[source]),
                patch.object(dispatch_stat, "mimi_binary", return_value=mimi),
                patch.object(
                    dispatch_stat,
                    "compile_with_stat",
                    return_value=(fake_stats, fake_outcome),
                ),
            ):
                results, outcomes = dispatch_stat.collect_all()

            self.assertIn("sample.mimi", results)
            self.assertEqual(len(outcomes), 1)
            self.assertEqual(marker.read_text(encoding="utf-8"), "pre-existing")
            self.assertEqual(list(target.glob("dispatch-stat-*")), [shared])


if __name__ == "__main__":
    unittest.main()
