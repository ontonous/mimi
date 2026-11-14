"""Python-only adversarial tests for the full Actor census merger.

These tests build temporary 1,187-row synthetic inputs. They do not invoke
Mimi, Cargo, Rust tests, a compiler, or the real evidence package.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).with_name("merge_actor_census_full_independent.py")
SPEC = importlib.util.spec_from_file_location("full_actor_census_merger_tested", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MERGER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MERGER
SPEC.loader.exec_module(MERGER)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class FullIndependentCensusMergerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.temp_root = Path(tempfile.mkdtemp(prefix="actor-census-merger-synthetic-"))
        cls.repo = cls.temp_root / "synthetic-repo"
        cls.evidence = cls.temp_root / "evidence"
        cls.repo.mkdir()
        cls.evidence.mkdir()
        (cls.repo / "docs").mkdir()
        (cls.repo / "docs" / "spec").mkdir()
        (cls.repo / "tests").mkdir()
        (cls.repo / "src" / "census").mkdir(parents=True)
        (cls.repo / "docs" / "language-spec.md").write_text(
            "# Synthetic normative source\nStable contract text.\n", encoding="utf-8"
        )
        cls.test_source = cls.repo / "tests" / "census_contract_test.py"
        cls.test_source.write_text("def test_current_contract():\n    assert True\n", encoding="utf-8")
        cls.test_sha = digest(cls.test_source.read_bytes())
        cls.spec_sha = digest((cls.repo / "docs" / "language-spec.md").read_bytes())
        (cls.repo / "docs" / "spec" / "resolved-ir.md").write_text(
            "# Synthetic backend capability gate\n", encoding="utf-8"
        )
        real_repo = SCRIPT.resolve().parents[1]
        nested_test_paths = (
            "tests/canonical_actor_nested_fallback_production_route.rs",
            "tests/real_world_cli.rs",
        )
        cls.nested_test_hashes = {}
        for relative in nested_test_paths:
            target = cls.repo / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((real_repo / relative).read_bytes())
            cls.nested_test_hashes[relative] = digest(target.read_bytes())
        cls.fixture_paths: dict[int, Path] = {}
        records = []
        source_rows = []
        for index in range(1, MERGER.COUNT + 1):
            relative = f"src/census/row-{index:04d}.mimi"
            source = cls.repo / relative
            if index in MERGER.APPROVED_LOCATOR_REFRESHES:
                refreshed_relative = MERGER.APPROVED_LOCATOR_REFRESHES[index]["path"]
                real_source = real_repo / refreshed_relative
                source = cls.repo / refreshed_relative
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_bytes(real_source.read_bytes())
                relative = refreshed_relative
            else:
                source.write_text(f"source row {index}\n", encoding="utf-8")
            cls.fixture_paths[index] = source
            source_bytes = source.read_bytes()
            source_sha = digest(source_bytes)
            locator_sha = (
                MERGER.APPROVED_LOCATOR_REFRESHES[index]["r2_sha256"]
                if index in MERGER.APPROVED_LOCATOR_REFRESHES
                else source_sha
            )
            records.append(
                {
                    "input_index": index,
                    "path": relative,
                    "sha256": locator_sha,
                    # Poison fields must be ignored: only the path/index/hash
                    # locator projection may influence the output identity.
                    "candidate_classification": "expected_source_diagnostic",
                    "expectation": {
                        "semantic_class": "tool_says_accept",
                        "execution_status": None if index == 1 else "already_ran",
                        "accepted": None if index == 1 else True,
                        "route": {"default": "route_pass"},
                    },
                    "run_status": "already_ran",
                    "route": "route_pass",
                    "runtime_observation": "not an authority",
                }
            )
            expected_class = (
                "expected_build_rejection"
                if index % 4 == 0
                else "expected_source_diagnostic"
                if index % 4 == 1
                else "required_compile"
                if index % 4 == 2
                else "unresolved"
            )
            base_row = {
                    "accepted": False,
                    "accepted_as_route_pass": False,
                    "bootstrap_or_runtime_observation_used": False,
                    "conflicts_and_limits": [],
                    "direct_test_contracts": [
                        {
                            "file": "tests/census_contract_test.py",
                            "function": "test_current_contract",
                            "lines": "1-2",
                            "sha256": cls.test_sha,
                            "contract": "Current test source locator only; not semantic authority.",
                            "exact_source_sha256_pin": False,
                        }
                    ],
                    "direct_test_gaps": [],
                    "expected_class": expected_class,
                    "inventory_index": index,
                    "mimi_or_cargo_or_test_or_build_executed": False,
                    "normative_basis": [
                        {
                            "file": "docs/language-spec.md",
                            "section": "synthetic §1",
                            "lines": "1-2",
                            "rule": "Synthetic rule citation; current hash is recorded at merge time.",
                        }
                    ],
                    "semantic_basis": f"Independent synthetic source-class basis for row {index}.",
                    "source_path": relative,
                    "source_sha256_current": source_sha,
                    "status": "not_run",
                }
            if index in (719, 720):
                prior_sha = digest(f"prior semantic snapshot {index}".encode())
                first_test = "tests/canonical_actor_nested_fallback_production_route.rs"
                second_test = "tests/real_world_cli.rs"
                base_row = {
                    "inventory_index": index,
                    "source_path": relative,
                    "source_sha256_current": source_sha,
                    "source_sha256_at_review": source_sha,
                    "source_hash_drift_during_review": False,
                    "source_evidence_lines": [
                        {"line": line, "text": source_bytes.decode().splitlines()[line - 1]}
                        for line in ((13, 14, 15, 16, 18) if index == 719 else (4, 5, 17, 18, 19))
                    ],
                    "candidate_semantic_class": "unresolved",
                    "expected_class": "unresolved",
                    "semantic_basis": "Nested Actor helper capture/shadow semantics are not fully settled by current normative text.",
                    "normative_basis": [
                        {
                            "file": "docs/language-spec.md",
                            "section": "§6.1 Functions",
                            "lines": "1-2",
                            "relevance": "Closure rule citation, with no review-time normative hash pin.",
                        }
                    ],
                    "default_route_expectation": {
                        "exact_command": f"mimi build {relative}",
                        "expectation_state": "blocked_unresolved",
                        "expected_result": "blocked: source validity is unresolved; no route-pass oracle is asserted",
                        "status": "not_run",
                        "accepted_as_route_pass": False,
                        "normative_route_basis": [
                            {
                                "file": "docs/spec/resolved-ir.md",
                                "section": "§6 Backend Capability Gate",
                                "lines": "1",
                                "relevance": "Route contract locator only.",
                            }
                        ],
                    },
                    "explicit_mir_route_expectation": {
                        "exact_command": f"mimi build {relative} --mir",
                        "expectation_state": "blocked_unresolved",
                        "expected_result": "blocked: source validity is unresolved; no route-pass oracle is asserted",
                        "status": "not_run",
                        "accepted_as_route_pass": False,
                        "normative_route_basis": [
                            {
                                "file": "docs/spec/resolved-ir.md",
                                "section": "§6 Backend Capability Gate",
                                "lines": "1",
                                "relevance": "Route contract locator only.",
                            }
                        ],
                    },
                    "direct_test_contracts": [
                        {
                            "contract_status": "blocked_unresolved",
                            "source_under_test": relative,
                            "source_sha256": source_sha,
                            "exact_command": f"mimi check {relative}",
                            "expected_oracle": None,
                            "blocker": "Current normative closure rule does not define this exact capture shape.",
                            "status": "not_run",
                            "accepted": False,
                        },
                        {
                            "kind": "existing_test_contract",
                            "test_source_path": first_test,
                            "test_source_sha256": cls.nested_test_hashes[first_test],
                            "test_source_sha256_rechecked": cls.nested_test_hashes[first_test],
                            "function": "original_nested_actor_capture_and_shadow_use_shared_canonical_production_routes",
                            "line": 187,
                            "assertion": "Test source is a direct test locator only, not semantic authority.",
                            "source_pin_status": "exact_match",
                        },
                        {
                            "kind": "existing_test_contract",
                            "test_source_path": second_test,
                            "test_source_sha256": cls.nested_test_hashes[second_test],
                            "test_source_sha256_rechecked": cls.nested_test_hashes[second_test],
                            "function": "canonical_actor_nested_fixture_route",
                            "line": 27177 if index == 719 else 27182,
                            "assertion": "CLI test locator only; not executed or used as semantic authority.",
                            "source_pin_status": "no_exact_sha_pin_found",
                        },
                    ],
                    "conflicts_and_limits": [
                        {
                            "kind": "prior_semantic_snapshot_drift",
                            "prior_semantic_snapshot_sha256": prior_sha,
                            "current_live_sha256": source_sha,
                            "treatment": "Historical snapshot mismatch is provenance only.",
                        },
                        {
                            "kind": "r2_locator_drift",
                            "r2_locator_sha256": locator_sha,
                            "current_live_sha256": source_sha,
                            "treatment": "The r2 source locator hash is refreshed to current live bytes.",
                        },
                    ],
                    "direct_test_gaps": [],
                    "status": "not_run",
                    "accepted": False,
                    "accepted_as_route_pass": False,
                    "bootstrap_or_runtime_observation_used": False,
                    "mimi_or_cargo_or_test_or_build_executed": False,
                    "source_sha256_prior_semantic_snapshot": prior_sha,
                    "source_sha256_r2_locator": locator_sha,
                    "source_hash_differs_from_prior_semantic_snapshot": True,
                    "prior_semantic_snapshot_drift": True,
                    "r2_locator_drift": True,
                }
            source_rows.append(base_row)

        cls.locator_path = cls.evidence / "inventory-current-r2.json"
        cls.locator_path.write_text(
            json.dumps(
                {"records": records, "expectations": {"classes": ["fake accepted class"]}},
                ensure_ascii=False,
                sort_keys=True,
            ) + "\n",
            encoding="utf-8",
        )
        cls.shard_paths = []
        for shard_number, (start, end) in enumerate(MERGER.SHARD_RANGES, 1):
            shard = cls.evidence / f"shard-{shard_number:03d}-{start:04d}-{end:04d}.jsonl"
            shard.write_text(
                "".join(
                    json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n"
                    for row in source_rows[start - 1 : end]
                ),
                encoding="utf-8",
            )
            cls.shard_paths.append(shard)
        cls.baseline_locator = cls.locator_path.read_bytes()
        cls.baseline_shards = {path: path.read_bytes() for path in cls.shard_paths}
        cls.baseline_sources = {path: path.read_bytes() for path in cls.fixture_paths.values()}

    @classmethod
    def tearDownClass(cls) -> None:
        shutil.rmtree(cls.temp_root)

    def setUp(self) -> None:
        self.locator_path.write_bytes(self.baseline_locator)
        for path, data in self.baseline_shards.items():
            path.write_bytes(data)
        for path, data in self.baseline_sources.items():
            if path.exists():
                path.unlink()
            path.write_bytes(data)
        self.package = self.evidence / f"candidate-{self._testMethodName}"
        if self.package.exists():
            shutil.rmtree(self.package)

    def _build(self) -> dict:
        return MERGER.build_candidate_package(
            locator_ledger=self.locator_path,
            shard_paths=self.shard_paths,
            repo_root=self.repo,
            output_dir=self.package,
            args_record={"synthetic": True},
            allow_synthetic_inputs=True,
        )

    def _validate(self) -> dict:
        return MERGER.validate_candidate_package(
            self.package,
            repo_root=self.repo,
            allow_synthetic_inputs=True,
        )

    def _load_locator(self) -> dict:
        return json.loads(self.locator_path.read_text(encoding="utf-8"))

    def _save_locator(self, document: dict) -> None:
        self.locator_path.write_text(json.dumps(document, sort_keys=True) + "\n", encoding="utf-8")

    def _load_shard(self, shard_index: int) -> list[dict]:
        return [json.loads(line) for line in self.shard_paths[shard_index].read_text().splitlines()]

    def _save_shard(self, shard_index: int, rows: list[dict]) -> None:
        self.shard_paths[shard_index].write_text(
            "".join(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n" for row in rows),
            encoding="utf-8",
        )

    def _rehash_package(self) -> None:
        manifest_path = self.package / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        for name in ("README.md", "census.jsonl", "summary.json"):
            manifest["outputs"][name] = digest((self.package / name).read_bytes())
        manifest_path.write_text(
            json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
            encoding="utf-8",
        )
        sums = {
            name: digest((self.package / name).read_bytes())
            for name in ("README.md", "census.jsonl", "summary.json", "manifest.json")
        }
        (self.package / "SHA256SUMS").write_text(
            "".join(f"{value}  {name}\n" for name, value in sorted(sums.items())),
            encoding="ascii",
        )

    def test_valid_exact_roster_keeps_semantics_and_route_axes_separate(self) -> None:
        manifest = self._build()
        self.assertEqual(manifest["row_count"], 1187)
        self.assertEqual(
            [item["inventory_index"] for item in manifest["locator_refreshes"]], [240, 244, 719, 720]
        )
        result = self._validate()
        self.assertFalse(result["accepted"])
        rows = [json.loads(line) for line in (self.package / "census.jsonl").read_text().splitlines()]
        self.assertEqual(len(rows), 1187)
        self.assertEqual(rows[0]["source_semantic_class"]["value"], "expected_source_diagnostic")
        self.assertEqual(rows[3]["source_semantic_class"]["value"], "expected_build_rejection")
        self.assertEqual(rows[3]["route_expectations"]["default"]["expectation"], "not_separately_adjudicated")
        self.assertEqual(rows[3]["route_expectations"]["explicit_mir"]["expectation"], "not_separately_adjudicated")
        self.assertFalse(rows[3]["route_expectations"]["source_class_used_as_route_oracle"])
        self.assertFalse(rows[0]["accepted"])
        self.assertEqual(rows[0]["status"], "not_run")
        self.assertEqual(rows[718]["route_expectations"]["default"]["expectation"], "blocked_unresolved")
        self.assertEqual(rows[718]["route_expectations"]["explicit_mir"]["expectation"], "blocked_unresolved")
        projection = manifest["locator_projection"]
        self.assertEqual(projection["r2_input_sha256"], digest(self.baseline_locator))
        self.assertEqual(projection["identity_fields"], ["input_index", "path"])
        self.assertEqual(projection["r2_hash_use"], "provenance_only; current source hash is recomputed from live repository bytes")
        self.assertFalse(projection["expectation_semantics_consumed"])
        self.assertFalse(projection["route_metadata_consumed"])
        self.assertFalse(projection["status_or_execution_metadata_consumed"])
        self.assertEqual(projection["ignored_record_field_occurrence_counts"]["run_status"], 1187)
        self.assertEqual(projection["ignored_record_field_occurrence_counts"]["expectation.semantic_class"], 1187)
        self.assertEqual(projection["ignored_record_null_value_counts"]["expectation.accepted"], 1)
        self.assertEqual(projection["ignored_record_null_value_counts"]["expectation.execution_status"], 1)
        self.assertEqual(projection["ignored_top_level_fields"], ["expectations"])
        # The poisoned r2 expectation/classification fields never appear.
        self.assertNotIn("candidate_classification", (self.package / "census.jsonl").read_text())

    def test_re_review_candidate_class_conflict_and_route_command_confusion_fail(self) -> None:
        rows = self._load_shard(5)
        extended_offset = 719 - 661
        rows[extended_offset]["candidate_semantic_class"] = "required_compile"
        self._save_shard(5, rows)
        with self.assertRaisesRegex(MERGER.CensusError, "candidate_semantic_class conflicts"):
            self._build()

        self.setUp()
        rows = self._load_shard(5)
        rows[extended_offset]["default_route_expectation"]["exact_command"] = (
            f"mimi build --mir {rows[extended_offset]['source_path']}"
        )
        self._save_shard(5, rows)
        with self.assertRaisesRegex(MERGER.CensusError, "exact command must be"):
            self._build()

    def test_locator_hash_field_must_be_valid_and_only_allowlisted_drift_is_accepted(self) -> None:
        doc = self._load_locator()
        doc["records"][0]["sha256"] = "z" * 64
        self._save_locator(doc)
        with self.assertRaisesRegex(MERGER.CensusError, "lowercase SHA-256"):
            self._build()

        self.setUp()
        doc = self._load_locator()
        doc["records"][718]["sha256"] = "0" * 64
        self._save_locator(doc)
        with self.assertRaisesRegex(MERGER.CensusError, "unapproved source hash drift"):
            self._build()

        self.setUp()
        doc = self._load_locator()
        doc["records"][239]["sha256"] = "0" * 64
        self._save_locator(doc)
        with self.assertRaisesRegex(MERGER.CensusError, "unapproved source hash drift"):
            self._build()

    def test_roster_missing_or_duplicate_fails_closed(self) -> None:
        doc = self._load_locator()
        doc["records"].pop()
        self._save_locator(doc)
        with self.assertRaisesRegex(MERGER.CensusError, "exact 1..1187"):
            self._build()

        self.setUp()
        doc = self._load_locator()
        doc["records"][1]["input_index"] = doc["records"][0]["input_index"]
        self._save_locator(doc)
        with self.assertRaisesRegex(MERGER.CensusError, "duplicate inventory index"):
            self._build()

    def test_unknown_fields_unknown_classes_and_invalid_status_types_fail(self) -> None:
        rows = self._load_shard(0)
        rows[0]["unexpected"] = "must be rejected"
        self._save_shard(0, rows)
        with self.assertRaisesRegex(MERGER.CensusError, "unknown=.*unexpected"):
            self._build()

        self.setUp()
        rows = self._load_shard(0)
        rows[0]["expected_class"] = "tool_says_pass"
        self._save_shard(0, rows)
        with self.assertRaisesRegex(MERGER.CensusError, "unknown source class"):
            self._build()

        self.setUp()
        rows = self._load_shard(0)
        rows[0]["accepted"] = "false"
        self._save_shard(0, rows)
        with self.assertRaisesRegex(MERGER.CensusError, "accepted must be the JSON boolean false"):
            self._build()

    def test_missing_shard_duplicate_row_and_wrong_range_fail(self) -> None:
        with self.assertRaisesRegex(MERGER.CensusError, "exactly nine"):
            MERGER.build_candidate_package(
                locator_ledger=self.locator_path,
                shard_paths=self.shard_paths[:-1],
                repo_root=self.repo,
                output_dir=self.package,
            )

        rows = self._load_shard(0)
        rows[-1]["inventory_index"] = rows[0]["inventory_index"]
        self._save_shard(0, rows)
        with self.assertRaisesRegex(MERGER.CensusError, "outside assigned range|duplicate roster index"):
            self._build()

    def test_noncanonical_and_physical_path_aliases_fail(self) -> None:
        doc = self._load_locator()
        doc["records"][0]["path"] = "./src/census/row-0001.mimi"
        self._save_locator(doc)
        with self.assertRaisesRegex(MERGER.CensusError, "noncanonical path alias"):
            self._build()

        self.setUp()
        first = self.fixture_paths[1]
        second = self.fixture_paths[2]
        second.unlink()
        os.link(first, second)
        doc = self._load_locator()
        doc["records"][1]["sha256"] = digest(first.read_bytes())
        self._save_locator(doc)
        with self.assertRaisesRegex(MERGER.CensusError, "path alias"):
            self._build()

    def test_unapproved_r2_source_drift_and_shard_to_live_drift_fail(self) -> None:
        source = self.fixture_paths[11]
        source.write_text("changed after locator and shard review\n", encoding="utf-8")
        with self.assertRaisesRegex(MERGER.CensusError, "unapproved source hash drift"):
            self._build()

        self.setUp()
        source = self.fixture_paths[11]
        source.write_text("current locator updated but shard remains old\n", encoding="utf-8")
        current = digest(source.read_bytes())
        doc = self._load_locator()
        doc["records"][10]["sha256"] = current
        self._save_locator(doc)
        with self.assertRaisesRegex(MERGER.CensusError, "shard source hash conflicts"):
            self._build()

    def test_normative_and_direct_test_hash_bindings_are_rechecked(self) -> None:
        rows = self._load_shard(0)
        rows[0]["direct_test_contracts"][0]["sha256"] = "0" * 64
        self._save_shard(0, rows)
        with self.assertRaisesRegex(MERGER.CensusError, "direct-test source hash drift"):
            self._build()

        self.setUp()
        self._build()
        (self.repo / "docs" / "language-spec.md").write_text("changed after package\n", encoding="utf-8")
        with self.assertRaisesRegex(MERGER.CensusError, "live source/spec/test hash drift"):
            self._validate()

    def test_pre_and_post_publish_drift_fail_and_remove_candidate(self) -> None:
        self.setUp()
        source = self.fixture_paths[7]
        original_rename = os.rename

        def rename_then_drift(src, dst):
            original_rename(src, dst)
            source.write_text("post-publish mutation\n", encoding="utf-8")

        with mock.patch.object(MERGER.os, "rename", side_effect=rename_then_drift):
            with self.assertRaisesRegex(
                MERGER.CensusError,
                "live source/spec/test hash drift|source/spec/test file drift|unapproved source hash drift",
            ):
                self._build()
        self.assertFalse(self.package.exists())

    def test_package_tamper_and_output_dir_collision_fail_closed(self) -> None:
        self._build()
        with self.assertRaisesRegex(MERGER.CensusError, "output directory already exists"):
            self._build()
        with (self.package / "census.jsonl").open("ab") as stream:
            stream.write(b"tamper\n")
        with self.assertRaisesRegex(MERGER.CensusError, "package checksum mismatch"):
                self._validate()

        self.setUp()
        self._build()
        manifest_path = self.package / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["accepted"] = True
        manifest_path.write_text(json.dumps(manifest) + "\n")
        self._rehash_package()
        with self.assertRaisesRegex(MERGER.CensusError, "acceptance flags must remain false"):
            self._validate()

    def test_rehashed_semantic_and_projection_tampering_is_rejected(self) -> None:
        self._build()
        rows = [json.loads(line) for line in (self.package / "census.jsonl").read_text().splitlines()]
        rows[0]["source_semantic_class"]["value"] = "required_compile"
        (self.package / "census.jsonl").write_bytes(MERGER._serialize_jsonl(rows))
        (self.package / "summary.json").write_bytes(MERGER._canonical_json_bytes(MERGER._build_summary(rows)))
        self._rehash_package()
        with self.assertRaisesRegex(MERGER.CensusError, "do not exactly match the bound independent shards"):
            self._validate()

        self.setUp()
        self._build()
        manifest_path = self.package / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["locator_projection"]["expectation_semantics_consumed"] = True
        manifest_path.write_text(json.dumps(manifest) + "\n")
        self._rehash_package()
        with self.assertRaisesRegex(MERGER.CensusError, "projection audit does not match"):
            self._validate()


if __name__ == "__main__":
    unittest.main(verbosity=2)
