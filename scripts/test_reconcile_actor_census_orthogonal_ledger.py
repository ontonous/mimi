"""Static contract tests for the source-taxonomy-only census reconciler."""

from __future__ import annotations

import ast
import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("reconcile_actor_census_orthogonal_ledger.py")
SPEC = importlib.util.spec_from_file_location("actor_census_orthogonal_reconciler", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
reconciler = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reconciler)


class ReconciliationContracts(unittest.TestCase):
    def test_conflict_supplement_precedes_shard_and_r7_is_comparison_only(self) -> None:
        shard = {"inventory_index": 1, "path": "a.mimi", "expected_class": "required_compile"}
        r7 = {
            "inventory_index": 1,
            "path": "a.mimi",
            "source_semantic_class": {"value": "expected_source_diagnostic"},
        }
        supplement = {
            "inventory_index": 1,
            "path": "a.mimi",
            "final_source_semantic_class": "unresolved",
        }
        claims, selected = reconciler.class_claims(shard, r7, supplement, None)
        self.assertEqual(selected["selection"]["value"], "unresolved")
        self.assertEqual(
            selected["selection"]["selected_source"],
            "independent_65_row_adjudication_supplement",
        )
        self.assertEqual(selected["r7_prior_claim"], "expected_source_diagnostic")
        r7_claim = next(claim for claim in claims if claim["source"] == "schema_v4_r7_ledger")
        self.assertEqual(r7_claim["role"], "prior_claim_comparison_only")

    def test_route_reclassification_class_is_separate_from_route_expectation(self) -> None:
        shard = {"inventory_index": 1, "path": "a.mimi", "expected_class": "expected_build_rejection"}
        route16 = {
            "inventory_index": 1,
            "source_path": "a.mimi",
            "source_semantic_class": "required_compile",
            "default_route": {"expected": "legacy"},
            "explicit_mir_route": {"expected": "reject"},
        }
        _, selected = reconciler.class_claims(shard, None, None, route16)
        default_route = reconciler.route_dimension("default_route_disposition", shard, None, None, route16)
        mir_only = reconciler.route_dimension("canonical_mir_only_input_rejection", shard, None, None, route16)
        self.assertEqual(selected["selection"]["value"], "required_compile")
        self.assertEqual(default_route["expected_disposition_hint"], {"expected": "legacy"})
        self.assertEqual(mir_only["expected_disposition_hint"], {"expected": "reject"})
        self.assertFalse(default_route["route_acceptance"])
        self.assertFalse(mir_only["route_acceptance"])
        _, shard_only = reconciler.class_claims(
            shard,
            None,
            None,
            {**route16, "source_semantic_class": "expected_build_rejection"},
        )
        self.assertEqual(shard_only["selection"]["value"], "not_separately_adjudicated")

    def test_any_live_source_hash_mismatch_forces_unresolved(self) -> None:
        source = "tests/fixtures/a.mimi"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / source
            fixture.parent.mkdir(parents=True)
            fixture.write_text("current", encoding="utf-8")
            current = hashlib.sha256(b"current").hexdigest()
            stale = hashlib.sha256(b"stale").hexdigest()
            row = {
                "inventory_index": 1,
                "source_path": source,
                "source_sha256_current": stale,
                "expected_class": "required_compile",
                "direct_test_contracts": [],
                "accepted": False,
                "accepted_as_route_pass": False,
                "status": "not_run",
            }
            r7 = {
                "inventory_index": 1,
                "path": source,
                "source_sha256": current,
                "source_semantic_class": {"value": "expected_source_diagnostic"},
                "direct_rust_test_contracts": [],
            }
            rows = reconciler.merge_rows(
                {
                    "shards": {source: row},
                    "r7": {source: r7},
                    "supplements": {},
                    "route16": {},
                },
                root,
            )
            self.assertEqual(rows[0]["source_sha256_current"], current)
            self.assertEqual(rows[0]["source_semantic_class"]["value"], "unresolved")
            self.assertEqual(rows[0]["source_semantic_class"]["status"], "unresolved_source_hash_mismatch")
            self.assertEqual(rows[0]["execution_status"], "not_run")
            self.assertFalse(rows[0]["accepted"])
            self.assertFalse(rows[0]["accepted_as_route_pass"])

    def test_contract_raw_record_is_preserved_with_conservative_pin_and_completeness(self) -> None:
        source = "tests/fixtures/a.mimi"
        raw_contract = {
            "test_path": "tests/a.rs",
            "test_id": "direct_case",
            "pins_exact_mimi_source_sha256": False,
            "execution_status": "not_run",
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / source
            fixture.parent.mkdir(parents=True)
            fixture.write_text("current", encoding="utf-8")
            current = hashlib.sha256(b"current").hexdigest()
            shard = {
                "inventory_index": 1,
                "source_path": source,
                "source_sha256_current": current,
                "expected_class": "required_compile",
                "direct_test_contracts": [raw_contract],
                "status": "not_run",
            }
            r7 = {
                "inventory_index": 1,
                "path": source,
                "source_sha256": current,
                "source_semantic_class": {"value": "required_compile"},
                "direct_rust_test_contracts": [],
            }
            merged = reconciler.merge_rows(
                {"shards": {source: shard}, "r7": {source: r7}, "supplements": {}, "route16": {}},
                root,
            )[0]
        status = merged["direct_test_contract_status"]
        found = [
            contract
            for container in status["containers"]
            for contract in container["contracts"]
        ]
        self.assertEqual(found[0]["raw_contract"], raw_contract)
        self.assertFalse(found[0]["pin_assessment"]["exact_source_sha256_pin"])
        self.assertEqual(found[0]["completeness_assessment"], "unproven_incomplete_execution_not_run")
        self.assertIn("no inventoried direct contract pins the exact Mimi source SHA-256", merged["blockers"])
        self.assertFalse(merged["accepted"])

    def test_missing_direct_contract_remains_a_blocker(self) -> None:
        status = reconciler.summarize_contracts([])
        self.assertEqual(status["status"], "missing_direct_contract_inventory_blocker")
        self.assertEqual(status["contract_count"], 0)

    def test_strict_validator_rejects_route_acceptance_and_non_not_run(self) -> None:
        row = {
            "path": "a.mimi",
            "inventory_index": 1,
            "execution_status": "not_run",
            "accepted": False,
            "accepted_as_route_pass": False,
            "route_acceptance": False,
            "source_hash_provenance": {"status": "all_current_bindings_match"},
            "source_semantic_class": {"value": "required_compile", "selected_source": "repaired_independent_shard"},
            "default_route_disposition": {"route_acceptance": False},
            "canonical_mir_only_input_rejection": {"route_acceptance": False},
            "direct_test_contract_status": {},
        }
        reconciler.validate_rows([row], expected_count=1)
        bad = dict(row, accepted=True)
        with self.assertRaises(ValueError):
            reconciler.validate_rows([bad], expected_count=1)
        bad = dict(row, source_semantic_class={"value": "expected_build_rejection"})
        with self.assertRaises(ValueError):
            reconciler.validate_rows([bad], expected_count=1)
        bad = dict(row, execution_status="passed")
        with self.assertRaises(ValueError):
            reconciler.validate_rows([bad], expected_count=1)

    def test_script_ast_has_no_process_or_shell_execution_apis(self) -> None:
        tree = ast.parse(SCRIPT.read_text(encoding="utf-8"), filename=str(SCRIPT))
        imported = {
            alias.name.split(".", 1)[0]
            for node in ast.walk(tree)
            if isinstance(node, (ast.Import, ast.ImportFrom))
            for alias in node.names
        }
        self.assertNotIn("subprocess", imported)
        calls = []
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            function = node.func
            if isinstance(function, ast.Attribute):
                calls.append((ast.unparse(function.value), function.attr))
            elif isinstance(function, ast.Name):
                calls.append(("", function.id))
        self.assertNotIn(("os", "system"), calls)
        self.assertFalse(any(name in {"run_mimi", "run_cargo", "run_tests"} for _, name in calls))


if __name__ == "__main__":
    unittest.main()
