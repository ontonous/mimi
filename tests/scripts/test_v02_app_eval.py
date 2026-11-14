#!/usr/bin/env python3
"""Focused tests for the V0.2 cumulative application sequence harness."""

from __future__ import annotations

import copy
import difflib
import importlib.util
import json
import shutil
import tempfile
import unittest
from pathlib import Path
from typing import Any


SCRIPT = Path(__file__).with_name("v02_app_eval.py")
SPEC = importlib.util.spec_from_file_location("v02_app_eval", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
EVAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVAL)


class CumulativeSequenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="v02-sequence-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.fixtures = self.root / "fixtures"
        self.fixtures.mkdir()
        self.artifacts = self.root / "artifacts"
        self.artifacts.mkdir()
        self.mimi = self.root / "mimi-test-double"
        self._write_fake_mimi()
        self.sequence = self._make_sequence()

    def _write_fake_mimi(self) -> None:
        self.mimi.write_text(
            "#!/usr/bin/env python3\n"
            "import pathlib, sys\n"
            "cmd = sys.argv[1]\n"
            "if cmd == 'check':\n"
            "    raise SystemExit(0)\n"
            "if cmd == 'run':\n"
            "    source = pathlib.Path(sys.argv[2])\n"
            "    text = source.read_text(encoding='utf-8')\n"
            "    output = source.parent / 'outputs' / (source.stem + '.txt')\n"
            "    output.parent.mkdir(exist_ok=True)\n"
            "    output.write_text(text, encoding='utf-8')\n"
            "    sys.stdout.write(text)\n"
            "    raise SystemExit(0)\n"
            "if cmd == 'build':\n"
            "    source = pathlib.Path(sys.argv[2])\n"
            "    output = pathlib.Path(sys.argv[sys.argv.index('-o') + 1])\n"
            "    text = source.read_text(encoding='utf-8')\n"
            "    output.write_text('#!/usr/bin/env python3\\nfrom pathlib import Path\\nimport sys\\ntext = ' + repr(text) + '\\nPath(\"outputs\").mkdir(exist_ok=True)\\nPath(' + repr('outputs/' + source.stem + '.txt') + ').write_text(text, encoding=\"utf-8\")\\nsys.stdout.write(text)\\n', encoding='utf-8')\n"
            "    output.chmod(0o755)\n"
            "    raise SystemExit(0)\n"
            "raise SystemExit(64)\n",
            encoding="utf-8",
        )
        self.mimi.chmod(0o755)

    def _make_sequence(self) -> dict[str, Any]:
        base = self.fixtures / "w2"
        (base / "tests").mkdir(parents=True)
        (self.fixtures / "changes").mkdir()
        (self.fixtures / "oracle").mkdir()
        (base / "w2_document_core.mimi").write_text("baseline entrypoint\n", encoding="utf-8")
        (base / "state.txt").write_text("baseline\n", encoding="utf-8")
        oracle: dict[str, dict[str, str]] = {}
        target_sources: list[dict[str, str]] = []
        for fixture in EVAL.EXPECTED_SEQUENCES["SEQ-W2"]:
            expected = f"{fixture}-result\n"
            rel = f"tests/{fixture}.mimi"
            (base / rel).write_text(expected, encoding="utf-8")
            oracle[fixture] = {"stdout": expected, "files": {f"outputs/{fixture}.txt": expected}}
            target_sources.append({"path": rel, "sha256": EVAL.digest(base / rel), "role": "source"})
        expected_path = self.fixtures / "oracle" / "expected.json"
        expected_path.write_text(json.dumps(oracle), encoding="utf-8")
        for fixture in EVAL.EXPECTED_SEQUENCES["SEQ-W2"][1:]:
            (self.fixtures / "changes" / f"{fixture}.mimi").write_text(f"anchor {fixture}\n", encoding="utf-8")

        source_manifest = [
            {"path": "w2_document_core.mimi", "sha256": EVAL.digest(base / "w2_document_core.mimi"), "role": "source"},
            {"path": "state.txt", "sha256": EVAL.digest(base / "state.txt"), "role": "data"},
            *target_sources,
        ]
        baseline_targets = [self._target("W2")]
        steps = []
        current = "baseline\n"
        patches = self.fixtures / "w2" / "sequences" / "patches"
        patches.mkdir(parents=True)
        prefix = ["W2"]
        for fixture in EVAL.EXPECTED_SEQUENCES["SEQ-W2"][1:]:
            before = current
            current += f"{fixture}\n"
            patch_text = "".join(difflib.unified_diff(
                before.splitlines(keepends=True),
                current.splitlines(keepends=True),
                fromfile="a/state.txt",
                tofile="b/state.txt",
                n=3,
            ))
            patch_path = patches / f"{fixture}.patch"
            patch_path.write_text(patch_text, encoding="utf-8")
            prefix = [*prefix, fixture]
            steps.append({
                "fixture": fixture,
                "entrypoint": "w2_document_core.mimi",
                "anchor": f"changes/{fixture}.mimi",
                "status": "ready",
                "patch": {
                    "path": f"w2/sequences/patches/{fixture}.patch",
                    "sha256": EVAL.digest(patch_path),
                    "format": "unified_diff",
                },
                "cumulative_regressions": prefix.copy(),
                "regression_targets": [self._target(case_id) for case_id in prefix],
            })
        return {
            "id": "SEQ-W2",
            "execution": "cumulative_patch_chain",
            "status": "ready",
            "accepted": False,
            "blockers": [],
            "baseline": {
                "fixture": "W2",
                "root": "w2",
                "entrypoint": "w2_document_core.mimi",
                "source_manifest": source_manifest,
                "cumulative_regressions": ["W2"],
                "regression_targets": baseline_targets,
            },
            "steps": steps,
        }

    def _target(self, fixture: str) -> dict[str, Any]:
        return {
            "fixture": fixture,
            "entrypoint": f"tests/{fixture}.mimi",
            "oracle": {
                "mode": "mimi_stdout",
                "file": "oracle/expected.json",
                "key": fixture,
                "sha256": EVAL.digest(self.fixtures / "oracle" / "expected.json"),
                "support_files": [],
            },
        }

    def _run(self, sequence: dict[str, Any] | None = None) -> dict[str, Any]:
        return EVAL.run_sequence(
            sequence or self.sequence,
            mimi=self.mimi,
            timeout=5,
            fixtures_root=self.fixtures,
            artifact_root=self.artifacts,
            cc="cc",
        )

    def test_applies_all_patches_to_one_cumulative_tree_and_hashes_each_parent(self) -> None:
        result = self._run()
        self.assertEqual(result["status"], "pass")
        rows = result["checkpoints"]
        self.assertEqual([row["fixture"] for row in rows], EVAL.EXPECTED_SEQUENCES["SEQ-W2"])
        self.assertIsNone(rows[0]["parent_hash"])
        self.assertTrue(rows[0]["output_hash"])
        self.assertNotEqual(rows[0]["initial_hash"], rows[0]["output_hash"])
        self.assertEqual(
            rows[0]["regressions"][0]["cumulative_state_changes"][0]["path"],
            "tests/outputs/W2.txt",
        )
        for parent, child in zip(rows, rows[1:]):
            self.assertEqual(child["parent_hash"], parent["output_hash"])
            snapshot = self.artifacts / child["worktree"]
            state = (snapshot / "state.txt").read_text(encoding="utf-8")
            row_index = rows.index(child)
            cumulative_steps = [row["fixture"] for row in rows[1 : row_index + 1]]
            self.assertEqual(state, "baseline\n" + "".join(f"{fixture}\n" for fixture in cumulative_steps))
            self.assertEqual(EVAL.tree_digest(snapshot), child["output_hash"])
            self.assertEqual(len(child["regressions"]), row_index + 1)
            for fixture in EVAL.EXPECTED_SEQUENCES["SEQ-W2"][: row_index + 1]:
                self.assertEqual(
                    (snapshot / "tests" / "outputs" / f"{fixture}.txt").read_text(encoding="utf-8"),
                    f"{fixture}-result\n",
                )
        final_state = self.artifacts / rows[-1]["worktree"] / "state.txt"
        self.assertEqual(final_state.read_text(encoding="utf-8"), "baseline\nD1\nD2\nD3\nD4\nD5\n")
        saved = json.loads((self.artifacts / "SEQ-W2" / "sequence.json").read_text(encoding="utf-8"))
        self.assertEqual(saved["checkpoints"][-1]["output_hash"], rows[-1]["output_hash"])
        self.assertFalse(saved["accepted"])

    def test_reordered_checkpoints_fail_before_creating_sequence_artifacts(self) -> None:
        bad = copy.deepcopy(self.sequence)
        bad["steps"][0], bad["steps"][1] = bad["steps"][1], bad["steps"][0]
        with self.assertRaisesRegex(EVAL.SequenceError, "missing, extra, or reordered"):
            self._run(bad)
        self.assertEqual(list(self.artifacts.iterdir()), [])

    def test_reordered_patch_artifacts_fail_even_when_step_ids_are_ordered(self) -> None:
        bad = copy.deepcopy(self.sequence)
        bad["steps"][0]["patch"], bad["steps"][1]["patch"] = (
            bad["steps"][1]["patch"],
            bad["steps"][0]["patch"],
        )
        result = self._run(bad)
        d1 = result["checkpoints"][1]
        self.assertEqual(result["status"], "failed")
        self.assertEqual(d1["status"], "failed")
        self.assertIn("ordered artifact", d1["detail"])
        self.assertIsNone(d1["output_hash"])

    def test_context_mismatch_does_not_mutate_the_cumulative_worktree(self) -> None:
        bad = copy.deepcopy(self.sequence)
        patch = self.fixtures / bad["steps"][0]["patch"]["path"]
        patch.write_text(
            "--- a/state.txt\n+++ b/state.txt\n@@ -1 +1,2 @@\n-not-the-baseline\n+D1\n+baseline\n",
            encoding="utf-8",
        )
        bad["steps"][0]["patch"]["sha256"] = EVAL.digest(patch)
        result = self._run(bad)
        d1 = result["checkpoints"][1]
        self.assertEqual(result["status"], "failed")
        self.assertEqual(d1["status"], "failed")
        self.assertIn("context mismatch", d1["detail"])
        self.assertIsNone(d1["output_hash"])
        baseline = self.artifacts / d1["parent_hash"]  # hash is metadata, not a path
        self.assertFalse(baseline.exists())
        baseline_snapshot = self.artifacts / result["checkpoints"][0]["worktree"]
        self.assertEqual((baseline_snapshot / "state.txt").read_text(encoding="utf-8"), "baseline\n")
        self.assertIsNone(result["checkpoints"][2]["output_hash"])

    def test_patch_hash_mismatch_fails_closed_without_applying_it(self) -> None:
        bad = copy.deepcopy(self.sequence)
        patch = self.fixtures / bad["steps"][0]["patch"]["path"]
        patch.write_text(patch.read_text(encoding="utf-8") + "\n", encoding="utf-8")
        result = self._run(bad)
        d1 = result["checkpoints"][1]
        self.assertEqual(d1["status"], "failed")
        self.assertIn("patch hash mismatch", d1["detail"])
        self.assertIsNone(d1["output_hash"])
        baseline_snapshot = self.artifacts / result["checkpoints"][0]["worktree"]
        self.assertEqual((baseline_snapshot / "state.txt").read_text(encoding="utf-8"), "baseline\n")

    def test_missing_patch_blocks_checkpoint_and_preserves_parent_hash(self) -> None:
        bad = copy.deepcopy(self.sequence)
        bad["steps"][0]["patch"] = None
        bad["steps"][0]["status"] = "blocked"
        result = self._run(bad)
        d1 = result["checkpoints"][1]
        self.assertEqual(result["status"], "blocked")
        self.assertEqual(d1["status"], "blocked")
        self.assertIn("no declared cumulative patch", d1["detail"])
        self.assertEqual(d1["parent_hash"], result["checkpoints"][0]["output_hash"])
        self.assertIsNone(d1["output_hash"])
        self.assertIsNone(result["checkpoints"][2]["output_hash"])

    def test_empty_or_reordered_regression_targets_never_pass(self) -> None:
        bad = copy.deepcopy(self.sequence)
        bad["baseline"]["regression_targets"] = []
        result = self._run(bad)
        self.assertNotEqual(result["status"], "pass")
        self.assertEqual(result["checkpoints"][0]["status"], "blocked")
        # The source patches are still replayed into retained checkpoints, but
        # the missing baseline oracle prevents the chain from being accepted.
        self.assertIsNotNone(result["checkpoints"][1]["output_hash"])

        with self.assertRaisesRegex(EVAL.SequenceError, "exact sequence prefix"):
            EVAL.validate_sequence_prefix(
                {"cumulative_regressions": ["D1", "W2"], "regression_targets": [self._target("D1"), self._target("W2")]},
                ["W2", "D1"],
                checkpoint="SEQ-W2/D1",
            )

    def test_checked_in_w2_patch_artifacts_replay_in_declared_order(self) -> None:
        fixture_root = EVAL.FIXTURES
        with tempfile.TemporaryDirectory(prefix="v02-w2-patches-") as temporary:
            worktree = Path(temporary)
            shutil.copy2(
                fixture_root / "w2" / "w2_document_core.mimi",
                worktree / "w2_document_core.mimi",
            )
            parent_hash = EVAL.tree_digest(worktree)
            output_hashes = []
            for index, fixture in enumerate(EVAL.EXPECTED_SEQUENCES["SEQ-W2"][1:], start=1):
                patch = fixture_root / "w2" / "sequences" / "patches" / f"{fixture}.patch"
                self.assertTrue(patch.is_file(), f"missing checked-in patch for {fixture}")
                self.assertTrue(EVAL.apply_unified_diff(worktree, patch.read_bytes()))
                output_hash = EVAL.tree_digest(worktree)
                self.assertNotEqual(output_hash, parent_hash)
                output_hashes.append(output_hash)
                parent_hash = output_hash
            self.assertEqual(len(set(output_hashes)), len(output_hashes))
            self.assertTrue((worktree / "w2_export_io.mimi").is_file())

    def test_checked_in_w4_patch_artifacts_replay_through_s4_relocation(self) -> None:
        fixture_root = EVAL.FIXTURES / "w4"
        with tempfile.TemporaryDirectory(prefix="v02-w4-patches-") as temporary:
            worktree = Path(temporary)
            shutil.copytree(fixture_root / "baseline", worktree / "baseline")
            parent_hash = EVAL.tree_digest(worktree)
            output_hashes = []
            for fixture in EVAL.EXPECTED_SEQUENCES["SEQ-W4"][1:]:
                patch = fixture_root / "sequences" / "patches" / f"{fixture}.patch"
                self.assertTrue(patch.is_file(), f"missing checked-in patch for {fixture}")
                self.assertTrue(EVAL.apply_unified_diff(worktree, patch.read_bytes()))
                output_hash = EVAL.tree_digest(worktree)
                self.assertNotEqual(output_hash, parent_hash)
                output_hashes.append(output_hash)
                parent_hash = output_hash
            self.assertEqual(len(set(output_hashes)), len(output_hashes))
            self.assertFalse((worktree / "baseline" / "src" / "main.mimi").exists())
            self.assertTrue((worktree / "offline-release" / "src" / "main.mimi").is_file())

    def test_nested_and_root_oracle_keys_resolve_without_merging_c_oracle(self) -> None:
        changes_path = EVAL.FIXTURES / "oracle" / "w4_changes_expected.json"
        changes_oracle = {
            "file": "oracle/w4_changes_expected.json",
            "key": "S3.endpoint_scope",
            "sha256": EVAL.digest(changes_path),
        }
        endpoint = EVAL._load_oracle(changes_oracle, EVAL.FIXTURES)
        mimi = EVAL._load_oracle({**changes_oracle, "key": "S3.mimi"}, EVAL.FIXTURES)
        w4_path = EVAL.FIXTURES / "w4" / "oracle" / "w4_expected.json"
        w4_root = EVAL._load_oracle({"file": "w4/oracle/w4_expected.json", "key": "$", "sha256": EVAL.digest(w4_path)}, EVAL.FIXTURES)
        self.assertEqual(endpoint["stdout"], "S3_SCOPE_ORACLE_OK denied_syscalls=0 allowed_listener_syscalls=1\n")
        self.assertEqual(mimi["stdout"], "S3_SCOPE_DENIED_BEFORE_SOCKET\n")
        self.assertIn("checksum", w4_root)


if __name__ == "__main__":
    unittest.main()
