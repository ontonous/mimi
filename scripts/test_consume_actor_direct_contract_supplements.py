"""Python-only contract tests for the direct-contract evidence consumer."""

from __future__ import annotations

import ast
import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("consume_actor_direct_contract_supplements.py")
SPEC = importlib.util.spec_from_file_location("actor_direct_contract_consumer", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
consumer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(consumer)


class DirectContractConsumerContracts(unittest.TestCase):
    def test_shard001_and_004_schema_shapes_remain_orthogonal(self) -> None:
        one = {
            "candidate_semantic_class": "expected_source_diagnostic",
            "direct_check_contract": {
                "implementation_status": "proposed_unimplemented",
                "assertions": [],
                "acceptance": False,
                "status": "not_run",
            },
            "related_existing_test_contracts": [],
        }
        one_contracts = consumer.extract_contracts(1, one)
        self.assertEqual(one_contracts["records"][0]["category"], "proposed_or_unimplemented_source_contract")

        four = {
            "candidate_semantic_class": {
                "value": "expected_source_diagnostic",
                "selected_from": "candidate-only",
                "candidate_status": "not_accepted",
                "accepted": False,
            },
            "source_semantic_contract": {"status": "candidate_not_accepted", "expected_behavior": None, "acceptance": False},
            "direct_test_contracts": [{"exact_source_sha256_pin": True, "execution_status": "not_run", "accepted": False}],
            "proposed_or_unimplemented_contract": {"status": "no_new_contract_proposed", "proposed_expectation": None},
        }
        four_contracts = consumer.extract_contracts(4, four)
        self.assertEqual(four_contracts["records"][0]["category"], "existing_test_contract")
        self.assertTrue(four_contracts["records"][0]["pin_assessment"]["exact_source_sha256_pin"])
        self.assertEqual(four_contracts["records"][1]["category"], "proposed_or_unimplemented_source_contract")
        self.assertEqual(four_contracts["records"][2]["category"], "no_new_source_contract_proposed")

    def test_shard006_mixed_contract_container_is_split_by_status(self) -> None:
        row = {
            "direct_test_contracts": [
                {"implementation_status": "existing", "contract_kind": "fixture_source_pin", "execution_status": "not_run", "accepted": False},
                {"implementation_status": "proposed_unimplemented", "contract_kind": "direct_source_semantics_check", "execution_status": "not_run", "accepted": False, "source_binding": {"hash_pinned_by_proposed_contract": True, "sha256": "a" * 64}},
                {"implementation_status": "blocked_unresolved", "contract_kind": "blocked_semantic_check", "execution_status": "not_run", "accepted": False},
            ],
            "blocked_unresolved_contract": {"status": "blocked_unresolved", "expected_check_or_runtime_result": None, "acceptance": False},
        }
        status = consumer.extract_contracts(6, row)
        categories = [record["category"] for record in status["records"]]
        self.assertEqual(categories[:3], [
            "existing_test_contract",
            "proposed_or_unimplemented_source_contract",
            "blocked_unresolved_source_contract",
        ])
        self.assertTrue(status["records"][1]["pin_assessment"]["exact_source_sha256_pin"])
        self.assertEqual(status["records"][1]["raw_contract_verbatim"], row["direct_test_contracts"][1])

    def test_unresolved_behavior_is_inactive_and_input_authority_is_rejected(self) -> None:
        status = consumer.extract_contracts(7, {
            "blocked_unresolved_contract": {
                "status": "blocked_unresolved",
                "expected_check_or_runtime_result": None,
                "acceptance": False,
            }
        })
        review = consumer.unresolved_source_contract_behavior(status, True)
        self.assertEqual(review["status"], "no_behavior_expectation_on_unresolved_source")
        self.assertIsNone(review["effective_behavior"])
        with self.assertRaises(ValueError):
            consumer.validate_input_execution_authority({"route_acceptance": True})
        with self.assertRaises(ValueError):
            consumer.validate_input_execution_authority({"execution_status": "passed"})
        with self.assertRaises(ValueError):
            consumer.validate_input_execution_authority({"bootstrap_or_runtime_observation_used": True})

    def test_candidate_mismatch_forces_unresolved_without_acceptance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ledger: dict[int, dict[str, object]] = {}
            for index in range(1, consumer.COUNT + 1):
                relative = f"fixtures/{index:04d}.mimi"
                source_file = root / relative
                source_file.parent.mkdir(parents=True, exist_ok=True)
                source_file.write_text(f"fixture {index}\n", encoding="utf-8")
                digest = hashlib.sha256(source_file.read_bytes()).hexdigest()
                ledger[index] = {
                    "inventory_index": index,
                    "path": relative,
                    "source_sha256_current": digest,
                    "source_hash_provenance": {"status": "all_current_bindings_match"},
                    "source_semantic_class": {"value": "required_compile", "status": "candidate_only"},
                    "default_route_disposition": {"route_acceptance": False},
                    "canonical_mir_only_input_rejection": {"route_acceptance": False},
                }
            first = ledger[1]
            supplement = {
                "inventory_index": 1,
                "source_path": first["path"],
                "source_sha256_current": first["source_sha256_current"],
                "candidate_semantic_class": "expected_source_diagnostic",
                "direct_check_contract": {"implementation_status": "proposed_unimplemented", "assertions": ["raw proposal retained, not used"], "acceptance": False, "status": "not_run"},
                "execution_status": "not_run",
                "accepted": False,
                "route_acceptance": False,
            }
            rows = consumer.build_rows({"ledger": ledger, "supplements": {1: supplement}}, root)
            consumer.validate_rows(rows)
        self.assertEqual(rows[0]["source_semantic_class"]["value"], "unresolved")
        self.assertEqual(rows[0]["source_semantic_class"]["candidate_match_status"], "mismatch_unresolved")
        self.assertFalse(rows[0]["source_semantic_acceptance"])
        self.assertFalse(rows[0]["route_acceptance"])
        self.assertEqual(rows[0]["execution_status"], "not_run")
        contract = rows[0]["direct_test_contract_status"]["records"][0]
        self.assertEqual(contract["effective_status"], "disabled_while_source_semantic_class_unresolved")
        self.assertIsNone(contract["effective_behavior"])
        self.assertEqual(rows[0]["unresolved_source_contract_behavior_review"]["status"], "blocked_behavior_disabled_no_source_semantic_assertion")

    def test_complete_live_supplement_roster_and_ledger_are_sha_bound(self) -> None:
        inputs = consumer.load_inputs()
        self.assertEqual(len(inputs["ledger"]), 1187)
        self.assertEqual(len(inputs["supplements"]), 1187)
        self.assertEqual(len(inputs["shard_files"]), 9)
        rows = consumer.build_rows(inputs, consumer.REPO_ROOT)
        consumer.validate_rows(rows)
        matches = sum(row["source_semantic_class"]["candidate_match_status"] == "matches_orthogonal_ledger" for row in rows)
        mismatches = sum(row["source_semantic_class"]["candidate_match_status"] == "mismatch_unresolved" for row in rows)
        self.assertEqual((matches, mismatches), (1185, 2))
        self.assertTrue(all(row["source_semantic_class"]["value"] == "unresolved" for row in rows if row["source_semantic_class"]["candidate_match_status"] == "mismatch_unresolved"))
        self.assertTrue(all(row["source_hash_validation"]["live_matches_r1_ledger"] for row in rows))
        self.assertTrue(all(row["source_hash_validation"]["live_matches_direct_supplement"] for row in rows))
        self.assertEqual(sum(row["source_hash_validation"]["r1_prior_unresolved_retained"] for row in rows), 5)
        summary = consumer.make_summary(rows, inputs)
        self.assertEqual(summary["package_status"], "blocked_candidate_class_mismatch")
        self.assertEqual(summary["input_roster_status"], "complete_input_roster")
        self.assertEqual(summary["direct_contract_inventory_status"], "complete_1187_candidate_only")
        self.assertEqual(summary["candidate_class_mismatch_rows_retained_unresolved"], 2)
        self.assertEqual(summary["source_semantic_acceptance"], "none; r1 classes remain candidate-only")
        self.assertEqual(summary["route_acceptance"], "none; no route result was executed or accepted")

    def test_validator_rejects_source_semantic_acceptance_and_route_pass(self) -> None:
        row = {
            "inventory_index": 1,
            "path": "a.mimi",
            "execution_status": "not_run",
            "accepted": False,
            "source_semantic_acceptance": False,
            "accepted_as_route_pass": False,
            "route_acceptance": False,
            "runtime_or_bootstrap_authority": False,
            "source_semantic_class": {"value": "required_compile", "accepted": False, "candidate_match_status": "matches_orthogonal_ledger"},
            "source_semantic_adjudication_evidence": {"status": "not_consumed_by_direct_contract_r2", "future_input_interface": {"semantic_acceptance": False, "route_acceptance": False}},
            "route_metadata_only": {"route_acceptance": False},
            "source_hash_validation": {"live_matches_r1_ledger": True, "live_matches_direct_supplement": True, "r1_prior_unresolved_retained": False},
            "direct_contract_supplement_row_verbatim": {},
            "direct_test_contract_status": {"execution_status": "not_run", "accepted": False, "route_acceptance": False, "records": []},
            "unresolved_source_contract_behavior_review": {"effective_behavior": None},
        }
        consumer.validate_rows([row], expected_count=1)
        with self.assertRaises(ValueError):
            consumer.validate_rows([{**row, "source_semantic_acceptance": True}], expected_count=1)
        with self.assertRaises(ValueError):
            consumer.validate_rows([{**row, "route_acceptance": True}], expected_count=1)

    def test_consumer_has_no_process_execution_api(self) -> None:
        tree = ast.parse(SCRIPT.read_text(encoding="utf-8"), filename=str(SCRIPT))
        imports = {
            alias.name.split(".", 1)[0]
            for node in ast.walk(tree)
            if isinstance(node, (ast.Import, ast.ImportFrom))
            for alias in node.names
        }
        self.assertNotIn("subprocess", imports)
        self.assertNotIn("os", imports)


if __name__ == "__main__":
    unittest.main()
