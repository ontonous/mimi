"""Synthetic Python-only contracts for the schema-v4 r3 census merger."""

from __future__ import annotations

import ast
import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path
from typing import Any


SCRIPT = Path(__file__).with_name("merge_actor_census_unresolved_adjudication_r3.py")
SPEC = importlib.util.spec_from_file_location("actor_census_adjudication_r3", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
r3 = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(r3)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixture_row(
    root: Path,
    index: int,
    path: str,
    *,
    r1_claims: list[dict[str, Any]] | None = None,
    direct_candidate: str = "unresolved",
    adjudication_candidate: str = "unresolved",
    route_expectation: dict[str, Any] | None = None,
    direct_contract: dict[str, Any] | None = None,
    existing_contracts: list[dict[str, Any]] | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    source = root / path
    source.parent.mkdir(parents=True, exist_ok=True)
    source.write_text(f"synthetic source {index}\n", encoding="utf-8")
    current = digest(source)
    r2 = {
        "schema": "source-bound-schema-v4-direct-contract-ledger",
        "inventory_index": index,
        "path": path,
        "source_semantic_class": {
            "value": "unresolved",
            "ledger_candidate_value": "unresolved",
            "supplement_candidate_value": direct_candidate,
            "accepted": False,
        },
        "source_hash_validation": {
            "live_source_sha256": current,
            "r1_ledger_source_sha256": current,
            "direct_supplement_source_sha256": current,
            "r1_prior_provenance_status": "all_current_bindings_match",
            "r1_prior_unresolved_retained": False,
        },
        "r1_orthogonal_ledger_row_verbatim": {
            "inventory_index": index,
            "path": path,
            "source_sha256_current": current,
            "source_semantic_class": {"value": "unresolved", "accepted": False},
            "source_semantic_class_claims": r1_claims or [
                {
                    "source": "repaired_independent_shard",
                    "field": "expected_class",
                    "role": "independent_shard_candidate",
                    "value": "unresolved",
                }
            ],
            "source_evidence_verbatim": {
                "repaired_independent_shard": {
                    "inventory_index": index,
                    "source_path": path,
                    "source_sha256_current": current,
                    "expected_class": "unresolved",
                    "normative_basis": ["synthetic prior basis"],
                },
                "independent_65_row_adjudication_supplement": None,
                "shard_004_route_reclassification_supplement": None,
                "schema_v4_r7_ledger_prior_claim": {"class": "unresolved"},
            },
            "default_route_disposition": {"candidate": "legacy", "route_acceptance": False},
            "canonical_mir_only_input_rejection": {
                "expected_build_rejection": None,
                "route_acceptance": False,
            },
            "accepted": False,
            "route_acceptance": False,
        },
        "direct_contract_supplement_row_verbatim": {
            "inventory_index": index,
            "source_path": path,
            "source_sha256_current": current,
            "candidate_semantic_class": direct_candidate,
            "accepted": False,
            "accepted_as_route_pass": False,
            "source_semantic_acceptance": False,
        },
        "default_route_disposition": {"candidate": "legacy", "route_acceptance": False},
        "canonical_mir_only_input_rejection": {
            "expected_build_rejection": None,
            "route_acceptance": False,
        },
        "direct_test_contract_status": {
            "execution_status": "not_run",
            "accepted": False,
            "route_acceptance": False,
            "records": (
                [
                    {
                        "category": "proposed_or_unimplemented_source_contract",
                        "source_field": "prior_direct",
                        "raw_contract_verbatim": {
                            "implementation_status": "proposed_unimplemented",
                            "status": "not_run",
                            "accepted": False,
                        },
                        "execution_status": "not_run",
                        "accepted": False,
                        "route_acceptance": False,
                        "completeness_status": "unproven_not_run",
                        "pin_assessment": {
                            "exact_source_sha256_pin": False,
                            "status": "exact_source_sha256_pin_not_proven",
                        },
                    }
                ]
                if direct_contract is not None
                else []
            ),
        },
        "accepted": False,
        "accepted_as_route_pass": False,
        "route_acceptance": False,
        "source_semantic_acceptance": False,
        "execution_status": "not_run",
        "runtime_or_bootstrap_authority": False,
    }
    adj = {
        "inventory_index": index,
        "source_path": path,
        "source_sha256_current": current,
        "independent_candidate_class": adjudication_candidate,
        "normative_basis": ["synthetic normative basis"],
        "route_acceptance": False,
        "accepted": False,
        "accepted_as_route_pass": False,
        "mimi_or_cargo_or_test_or_build_or_bootstrap_executed": False,
        "bootstrap_or_runtime_observation_used": False,
        "direct_contract": direct_contract
        or {
            "status": "proposed_unimplemented",
            "implementation_status": "proposed_unimplemented",
            "execution_status": "not_run",
            "accepted": False,
            "source_path": path,
            "source_sha256": current,
        },
    }
    if route_expectation is not None:
        adj["route_expectation"] = route_expectation
    if existing_contracts is not None:
        adj["related_existing_test_contracts"] = existing_contracts
    return r2, adj


class ActorCensusAdjudicationR3Contracts(unittest.TestCase):
    def test_actor_719_720_prior_class_conflicts_remain_unresolved(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pair = [
                fixture_row(
                    root,
                    719,
                    "tests/real_world/actor_nested_func_capture.mimi",
                    r1_claims=[
                        {
                            "source": "repaired_independent_shard",
                            "field": "expected_class",
                            "role": "independent_shard_candidate",
                            "value": "unresolved",
                        },
                        {
                            "source": "schema_v4_r7_ledger",
                            "field": "source_semantic_class",
                            "role": "prior_claim_comparison_only",
                            "value": "required_compile",
                        },
                    ],
                    direct_candidate="expected_source_diagnostic",
                    adjudication_candidate="expected_source_diagnostic",
                ),
                fixture_row(
                    root,
                    720,
                    "tests/real_world/actor_nested_func_shadow.mimi",
                    r1_claims=[
                        {
                            "source": "repaired_independent_shard",
                            "field": "expected_class",
                            "role": "independent_shard_candidate",
                            "value": "unresolved",
                        },
                        {
                            "source": "schema_v4_r7_ledger",
                            "field": "source_semantic_class",
                            "role": "prior_claim_comparison_only",
                            "value": "required_compile",
                        },
                    ],
                    direct_candidate="expected_source_diagnostic",
                    adjudication_candidate="expected_source_diagnostic",
                ),
            ]
            rows = r3.merge_rows(
                [item[0] for item in pair],
                [item[1] for item in pair],
                root,
                expected_count=None,
            )
        self.assertEqual([row["inventory_index"] for row in rows], [719, 720])
        for row in rows:
            self.assertEqual(row["source_semantic_class"]["value"], "unresolved")
            self.assertEqual(
                row["source_semantic_class"]["status"],
                "source_class_claim_conflict_forces_unresolved",
            )
            self.assertFalse(row["source_semantic_acceptance"])
            self.assertTrue(row["source_semantic_class"]["class_claim_conflicts"])

    def test_hash_drift_forces_unresolved_and_blocks_finalization(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            r2_row, adj = fixture_row(
                root,
                1,
                "fixture.mimi",
                adjudication_candidate="required_compile",
            )
            (root / "fixture.mimi").write_text("replacement bytes\n", encoding="utf-8")
            rows = r3.merge_rows([r2_row], [adj], root, expected_count=1)
        row = rows[0]
        self.assertEqual(row["source_semantic_class"]["value"], "unresolved")
        self.assertEqual(
            row["source_semantic_class"]["status"],
            "live_source_hash_mismatch_forces_unresolved",
        )
        self.assertTrue(
            row["source_hash_validation"]["live_source_hash_mismatch_reasons"]
        )
        self.assertEqual(len(r3.finalization_blockers(rows)), 1)

    def test_build_rejection_route_claim_does_not_replace_source_diagnostic(self) -> None:
        route = {
            "expected_build_rejection": "conditional selected-backend rejection",
            "accepted_as_route_pass": False,
            "note": "route expectation only",
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            r2_row, adj = fixture_row(
                root,
                1,
                "fixture.mimi",
                adjudication_candidate="expected_source_diagnostic",
                route_expectation=route,
            )
            rows = r3.merge_rows([r2_row], [adj], root, expected_count=1)
        row = rows[0]
        self.assertEqual(row["source_semantic_class"]["value"], "expected_source_diagnostic")
        self.assertEqual(
            row["route_expectation_claims_verbatim"][0]["raw_value_verbatim"],
            route,
        )
        self.assertFalse(row["route_acceptance"])
        self.assertFalse(row["source_semantic_acceptance"])
        self.assertEqual(
            row["source_semantic_class"]["independent_candidate_value"],
            "expected_source_diagnostic",
        )

    def test_direct_contracts_merge_without_acceptance_or_execution(self) -> None:
        prior = {
            "status": "not_run",
            "implementation_status": "proposed_unimplemented",
            "accepted": False,
        }
        adjud = {
            "contract_status": "blocked_unresolved",
            "implementation_status": "proposed_unimplemented",
            "status": "not_run",
            "execution_status": "not_run",
            "accepted": False,
            "source_path": "fixture.mimi",
        }
        existing = [{"implementation_status": "existing", "test_name": "existing_test"}]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            r2_row, adj = fixture_row(
                root,
                1,
                "fixture.mimi",
                direct_contract=prior,
                existing_contracts=existing,
            )
            adj["direct_contract"] = adjud
            rows = r3.merge_rows([r2_row], [adj], root, expected_count=1)
        records = rows[0]["direct_test_contract_status"]["records"]
        self.assertEqual(
            [record["lifecycle_class"] for record in records],
            ["proposed", "blocked", "existing"],
        )
        self.assertEqual([record["execution_status"] for record in records], ["not_run"] * 3)
        self.assertTrue(all(record["accepted"] is False for record in records))
        self.assertTrue(all(record["route_acceptance"] is False for record in records))
        self.assertEqual(records[1]["raw_contract_verbatim"], adjud)
        self.assertEqual(records[2]["raw_contract_verbatim"], existing[0])

    def test_path_uniqueness_is_a_hard_roster_gate(self) -> None:
        duplicate_path = [
            {"inventory_index": 1, "path": "same.mimi"},
            {"inventory_index": 2, "path": "same.mimi"},
        ]
        with self.assertRaisesRegex(ValueError, "duplicate r2 ledger source path"):
            r3.unique_index_and_path(duplicate_path, "r2 ledger", expected_count=2)

    def test_path_uniqueness_uses_normalized_and_physical_paths(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "same.mimi"
            source.write_text("same source\n", encoding="utf-8")
            normalized_aliases = [
                {"inventory_index": 1, "path": "same.mimi"},
                {"inventory_index": 2, "path": "./same.mimi"},
            ]
            with self.assertRaisesRegex(ValueError, "duplicate r2 ledger source path"):
                r3.unique_index_and_path(
                    normalized_aliases,
                    "r2 ledger",
                    expected_count=2,
                    repo_root=root,
                )
            hardlink = root / "alias.mimi"
            hardlink.hardlink_to(source)
            physical_aliases = [
                {"inventory_index": 1, "path": "same.mimi"},
                {"inventory_index": 2, "path": "alias.mimi"},
            ]
            with self.assertRaisesRegex(ValueError, "duplicate r2 ledger physical source file"):
                r3.unique_index_and_path(
                    physical_aliases,
                    "r2 ledger",
                    expected_count=2,
                    repo_root=root,
                )

    def test_adjudication_source_path_provenance_must_match_source_row(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, adj = fixture_row(root, 1, "fixture.mimi")
            (root / "other.mimi").write_text("other source\n", encoding="utf-8")
            adj["direct_contract"]["source_binding"] = {
                "source_path": "other.mimi",
                "source_sha256": digest(root / "other.mimi"),
            }
            with self.assertRaisesRegex(ValueError, "source-path provenance mismatch"):
                r3.validate_adjudication_source_provenance(adj, root)

    def test_missing_or_extra_adjudication_rows_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            r2_row, adj = fixture_row(root, 1, "one.mimi")
            with self.assertRaisesRegex(ValueError, "adjudication index roster"):
                r3.merge_rows([r2_row], [], root, expected_count=1)
            extra = dict(adj)
            extra["inventory_index"] = 2
            extra["source_path"] = "two.mimi"
            (root / "two.mimi").write_text("synthetic source 2\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "adjudication index roster"):
                r3.merge_rows([r2_row], [adj, extra], root, expected_count=1)

    def test_route_only_semantic_label_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            r2_row, adj = fixture_row(root, 1, "one.mimi")
            adj["independent_candidate_class"] = "expected_build_rejection"
            with self.assertRaisesRegex(ValueError, "route-only disposition"):
                r3.merge_rows([r2_row], [adj], root, expected_count=1)

    def test_unknown_source_class_and_conflicting_output_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            r2_row, adj = fixture_row(
                root,
                1,
                "one.mimi",
                adjudication_candidate="required_compile",
            )
            adj["independent_candidate_class"] = "invented_class"
            with self.assertRaisesRegex(ValueError, "unknown source-semantic class"):
                r3.merge_rows([r2_row], [adj], root, expected_count=1)

            r2_row, adj = fixture_row(
                root,
                1,
                "one.mimi",
                r1_claims=[
                    {
                        "source": "prior",
                        "field": "expected_class",
                        "value": "invented_class",
                    }
                ],
                adjudication_candidate="required_compile",
            )
            with self.assertRaisesRegex(ValueError, "unknown source-semantic class"):
                r3.merge_rows([r2_row], [adj], root, expected_count=1)

            r2_row, adj = fixture_row(
                root,
                1,
                "one.mimi",
                r1_claims=[
                    {
                        "source": "repaired_independent_shard",
                        "field": "expected_class",
                        "role": "independent_shard_candidate",
                        "value": "unresolved",
                    },
                    {
                        "source": "schema_v4_r7_ledger",
                        "field": "source_semantic_class",
                        "role": "prior_claim_comparison_only",
                        "value": "required_compile",
                    },
                ],
                adjudication_candidate="expected_source_diagnostic",
            )
            rows = r3.merge_rows([r2_row], [adj], root, expected_count=1)
            rows[0]["source_semantic_class"]["value"] = "required_compile"
            with self.assertRaisesRegex(ValueError, "candidate/conflict results"):
                r3.validate_rows(rows, expected_count=1, repo_root=root)

    def test_independent_class_claim_conflict_remains_unresolved(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            r2_row, adj = fixture_row(
                root,
                1,
                "one.mimi",
                adjudication_candidate="required_compile",
            )
            adj["candidate_semantic_class"] = "known_gap"
            rows = r3.merge_rows([r2_row], [adj], root, expected_count=1)
        semantic = rows[0]["source_semantic_class"]
        self.assertEqual(semantic["value"], "unresolved")
        self.assertEqual(
            semantic["status"],
            "source_class_claim_conflict_forces_unresolved",
        )
        self.assertFalse(semantic["update_gate_passed"])
        self.assertEqual(
            semantic["class_claim_conflicts"][0]["kind"],
            "independent_adjudication_candidate_fields_disagree",
        )

    def test_invalid_authoritative_source_hash_cannot_be_masked_by_valid_pin(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, adj = fixture_row(root, 1, "one.mimi")
            adj["direct_contract"]["source_sha256_current"] = "not-a-sha256"
            with self.assertRaisesRegex(ValueError, "malformed authoritative source hash"):
                r3.adjudication_hash_claims(adj)

    def test_unknown_path_hash_pair_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, adj = fixture_row(root, 1, "one.mimi")
            adj["direct_contract"]["source_binding"] = {
                "artifact_path": "one.mimi",
                "digest": digest(root / "one.mimi"),
            }
            with self.assertRaisesRegex(ValueError, "unknown source path/hash schema"):
                r3.validate_adjudication_source_provenance(adj, root)

    def test_control_status_fields_require_boolean_false_recursively(self) -> None:
        invalid_rows = [
            {"nested": [{"accepted": "false"}]},
            {"nested": [{"mimi_or_cargo_or_test_or_build_executed": "false"}]},
            {"nested": [{"runtime_or_bootstrap_observation_used": 0}]},
            {"nested": [{"semantic_authority": "false"}]},
            {"nested": [{"execution_status": False}]},
            {"nested": [{"not_executed": "true"}]},
        ]
        for row in invalid_rows:
            with self.subTest(row=row), self.assertRaises(ValueError):
                r3.validate_input_nonexecution(row, "synthetic")
            with self.subTest(output=row), self.assertRaises(ValueError):
                r3._assert_false_fields(row, "synthetic")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, adj = fixture_row(root, 1, "one.mimi")
            adj["authority_package_current_source_hash_matches"] = None
            with self.assertRaisesRegex(ValueError, "non-boolean source hash review flag"):
                r3._source_hash_review_flags(adj)

    def test_write_package_rechecks_live_hashes_before_creating_output(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            r2_row, adj = fixture_row(
                root,
                1,
                "one.mimi",
                adjudication_candidate="required_compile",
            )
            rows = r3.merge_rows([r2_row], [adj], root, expected_count=1)
            (root / "one.mimi").write_text("changed after merge\n", encoding="utf-8")
            output = root / "external-package"
            with self.assertRaisesRegex(ValueError, "live source SHA changed before package write"):
                r3.write_package(output, rows, {}, {}, root)
            self.assertFalse(output.exists())

    def test_hash_and_provenance_blockers_have_separate_summary_counts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            drifted_r2, drifted_adj = fixture_row(
                root,
                1,
                "drifted.mimi",
                adjudication_candidate="required_compile",
            )
            (root / "drifted.mimi").write_text("drifted bytes\n", encoding="utf-8")
            retained_r2, retained_adj = fixture_row(
                root,
                2,
                "retained.mimi",
                r1_claims=[
                    {
                        "source": "repaired_independent_shard",
                        "field": "expected_class",
                        "role": "independent_shard_candidate",
                        "value": "unresolved",
                    }
                ],
            )
            retained_r2["source_hash_validation"]["r1_prior_unresolved_retained"] = True
            retained_r2["source_hash_validation"]["r1_prior_provenance_status"] = "mismatch_unresolved"
            rows = r3.merge_rows(
                [drifted_r2, retained_r2],
                [drifted_adj, retained_adj],
                root,
                expected_count=2,
            )
            summary = r3.make_summary(
                rows,
                {"unresolved_indices": {1, 2}, "adjudication_by_index": {1: {}, 2: {}}, "shard_files": []},
            )
        self.assertEqual(summary["live_source_hash_mismatch_rows"], 1)
        self.assertEqual(summary["source_hash_provenance_blocker_rows"], 1)
        self.assertEqual(summary["finalization_blocker_rows"], 2)
        self.assertNotIn("source_hash_drift_rows", summary)

    def test_manifest_records_merger_hash_and_python_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            r2 = root / "r2.jsonl"
            r2.write_text('{"inventory_index":1}\n', encoding="utf-8")
            shard_files = []
            for ordinal in range(9):
                shard = root / f"shard-{ordinal}.jsonl"
                shard.write_text('{"inventory_index":1}\n', encoding="utf-8")
                shard_files.append(shard)
            manifest = r3.input_manifest(
                {
                    "r2_path": r2,
                    "r2_rows": [{"inventory_index": 1}],
                    "shard_files": shard_files,
                }
            )
        self.assertEqual(manifest["merger"]["sha256"], digest(SCRIPT))
        self.assertTrue(manifest["runtime"]["python_implementation"])
        self.assertTrue(manifest["runtime"]["python_version"])
        self.assertTrue(manifest["runtime"]["python_full_version"])

    def test_source_has_no_process_execution_api(self) -> None:
        tree = ast.parse(SCRIPT.read_text(encoding="utf-8"), filename=str(SCRIPT))
        imports = {
            alias.name.split(".", 1)[0]
            for node in ast.walk(tree)
            if isinstance(node, (ast.Import, ast.ImportFrom))
            for alias in node.names
        }
        self.assertNotIn("subprocess", imports)
        self.assertNotIn("cargo", imports)
        self.assertNotIn("mimi", imports)


if __name__ == "__main__":
    unittest.main()
