#!/usr/bin/env python3
"""Reconcile Actor census source taxonomy without accepting any route.

This is a read-only crosswalk builder. It combines the repaired independent
shards, the 65 row-specific source-semantic supplements, shard-004's 16 row
route review, and the r7 source-bound ledger. Evidence is embedded verbatim in
each output row; this script never executes Mimi, Cargo, or a test suite.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from collections import Counter
from pathlib import Path
from typing import Any, Iterable, Mapping


REPO_ROOT = Path(__file__).resolve().parents[1]
EVIDENCE_ROOT = Path("/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census")
SHARD_DIR = EVIDENCE_ROOT / "actor-census-full-independent-repaired-20261113-r1/copies"
R7_LEDGER = (
    EVIDENCE_ROOT
    / "actor-schema-v4-source-bound-current-20261113-r1/ledger-r7-provenance-fail-closed/"
    / "schema-v4-source-bound-ledger-1187.jsonl"
)
SUPPLEMENT_PATHS = (
    EVIDENCE_ROOT / "actor-census-disagreement-adjudication-001-022-20261113/adjudication.json",
    EVIDENCE_ROOT / "actor-census-independent-adjudication-23-44-20261113-r1/rows-23-44.jsonl",
    EVIDENCE_ROOT / "actor-census-independent-supplement-rows-45-65-20261113/rows-45-65-independent-adjudication.json",
)
ROUTE16_PATH = (
    EVIDENCE_ROOT
    / "actor-census-full-independent-20261113/shard-004-build-rejection-review.jsonl"
)
DEFAULT_OUTPUT = (
    EVIDENCE_ROOT / "actor-census-orthogonal-taxonomy-reconcile-20261113-r1"
)

ALLOWED_CLASSES = {
    "required_compile",
    "expected_source_diagnostic",
    "expected_build_rejection",
    "interpreter_only",
    "known_gap",
    "unmapped",
    "environment_failure",
    "unresolved",
}
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
HASH_FIELDS = (
    "source_sha256_current",
    "current_source_sha256",
    "source_sha256",
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n"


def read_json_or_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"evidence input is unavailable or not regular: {path}")
    if path.suffix == ".jsonl":
        rows: list[dict[str, Any]] = []
        with path.open("rb") as stream:
            for line_number, raw in enumerate(stream, 1):
                if not raw.strip():
                    continue
                try:
                    value = json.loads(raw.decode("utf-8"))
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise ValueError(f"{path}:{line_number}: invalid JSONL: {error}") from error
                if not isinstance(value, dict):
                    raise ValueError(f"{path}:{line_number}: row must be an object")
                rows.append(value)
        return rows

    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{path}: invalid JSON: {error}") from error
    if isinstance(value, list):
        values = value
    elif isinstance(value, dict) and isinstance(value.get("rows"), list):
        values = value["rows"]
    else:
        raise ValueError(f"{path}: expected a JSON array or object with rows[]")
    if not all(isinstance(row, dict) for row in values):
        raise ValueError(f"{path}: each row must be an object")
    return values


def source_path(row: Mapping[str, Any]) -> str:
    value = row.get("path") or row.get("source_path")
    if not isinstance(value, str) or not value:
        raise ValueError("evidence row has no source path")
    return value


def index_value(row: Mapping[str, Any]) -> int:
    value = row.get("inventory_index", row.get("index"))
    if not isinstance(value, int) or isinstance(value, bool) or value < 1:
        raise ValueError(f"invalid inventory index for {source_path(row)}: {value!r}")
    return value


def index_by_path(rows: Iterable[dict[str, Any]], source: str) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    indices: set[int] = set()
    for row in rows:
        path = source_path(row)
        index = index_value(row)
        if path in result:
            raise ValueError(f"duplicate {source} source path: {path}")
        if index in indices:
            raise ValueError(f"duplicate {source} inventory index: {index}")
        result[path] = row
        indices.add(index)
    return result


def read_inputs() -> dict[str, Any]:
    shard_paths = sorted(SHARD_DIR.glob("shard-*.jsonl"))
    if len(shard_paths) != 9:
        raise ValueError(f"expected nine repaired shard JSONL files; found {len(shard_paths)}")
    shard_rows: list[dict[str, Any]] = []
    for path in shard_paths:
        shard_rows.extend(read_json_or_jsonl(path))
    r7_rows = read_json_or_jsonl(R7_LEDGER)
    supplements = [row for path in SUPPLEMENT_PATHS for row in read_json_or_jsonl(path)]
    route16_rows = read_json_or_jsonl(ROUTE16_PATH)
    return {
        "shard_files": shard_paths,
        "shards": index_by_path(shard_rows, "independent shard"),
        "r7": index_by_path(r7_rows, "r7 ledger"),
        "supplements": index_by_path(supplements, "65-row supplement"),
        "route16": index_by_path(route16_rows, "route-16 supplement"),
        "input_files": [*shard_paths, R7_LEDGER, *SUPPLEMENT_PATHS, ROUTE16_PATH],
    }


def get_source_hash(row: Mapping[str, Any], source: str) -> str | None:
    """Return only the row's primary current/source binding, never a test-file hash."""
    if source == "r7":
        value = row.get("source_sha256")
    else:
        value = row.get("source_sha256_current")
        if value is None:
            value = row.get("current_source_sha256")
    return value if isinstance(value, str) else None


def source_hash_evidence(
    path: str,
    current_hash: str | None,
    source_rows: Mapping[str, Mapping[str, Any] | None],
) -> dict[str, Any]:
    bindings: list[dict[str, Any]] = []
    mismatch = current_hash is None
    if current_hash is None:
        bindings.append({"source": "worktree", "reported_sha256": None, "status": "source_missing"})
    else:
        bindings.append({"source": "worktree", "reported_sha256": current_hash, "status": "current"})
    for origin, row in source_rows.items():
        if row is None:
            continue
        reported = get_source_hash(row, origin)
        if reported is None or not SHA256_RE.fullmatch(reported):
            status = "missing_or_malformed_source_sha256"
            mismatch = True
        elif current_hash is None or reported != current_hash:
            status = "mismatch"
            mismatch = True
        else:
            status = "current"
        bindings.append({"source": origin, "reported_sha256": reported, "status": status})
    for origin, row in source_rows.items():
        if row is None:
            continue
        if origin in {"supplement_65", "route16"}:
            match_flags = [
                (key, row.get(key))
                for key in (
                    "source_sha_matches_crosswalk_schema_and_shard",
                    "crosswalk_source_sha_match",
                    "source_hashes_all_match",
                    "source_sha_matches_ledger_and_shard",
                )
                if key in row
            ]
            for key, flag in match_flags:
                if flag is not True:
                    mismatch = True
                    bindings.append({
                        "source": origin,
                        "reported_sha256": None,
                        "status": "crosswalk_match_flag_not_true",
                        "field": key,
                        "reported_value": flag,
                    })
    return {
        "current_source_sha256": current_hash,
        "status": "mismatch_unresolved" if mismatch else "all_current_bindings_match",
        "bindings": bindings,
    }


def class_claims(
    shard: Mapping[str, Any],
    r7: Mapping[str, Any] | None,
    supplement: Mapping[str, Any] | None,
    route16: Mapping[str, Any] | None,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    claims: list[dict[str, Any]] = []
    if supplement is not None:
        if "final_source_semantic_class" in supplement:
            claims.append({
                "source": "independent_65_row_adjudication_supplement",
                "field": "final_source_semantic_class",
                "value": supplement.get("final_source_semantic_class"),
                "role": "independent_source_semantic_candidate",
            })
        elif "independent_revised_source_semantic_class" in supplement:
            claims.append({
                "source": "independent_65_row_adjudication_supplement",
                "field": "independent_revised_source_semantic_class",
                "value": supplement.get("independent_revised_source_semantic_class"),
                "role": "independent_source_semantic_candidate",
            })
        else:
            ruling = supplement.get("independent_ruling")
            value = ruling.get("source_semantic_class") if isinstance(ruling, Mapping) else None
            claims.append({
                "source": "independent_65_row_adjudication_supplement",
                "field": "independent_ruling.source_semantic_class",
                "value": value,
                "role": "independent_source_semantic_candidate",
            })
    if route16 is not None:
        claims.append({
            "source": "shard_004_route_reclassification_supplement",
            "field": "source_semantic_class",
            "value": route16.get("source_semantic_class"),
            "role": "independent_source_semantic_candidate",
        })
    claims.append({
        "source": "repaired_independent_shard",
        "field": "expected_class",
        "value": shard.get("expected_class"),
        "role": "independent_shard_candidate",
    })
    r7_class = None
    if r7 is not None:
        r7_value = r7.get("source_semantic_class")
        if isinstance(r7_value, Mapping):
            r7_class = r7_value.get("value")
        if r7_class is None:
            r7_class = r7.get("semantic_class")
        claims.append({
            "source": "schema_v4_r7_ledger",
            "field": "source_semantic_class.value_or_semantic_class",
            "value": r7_class,
            "role": "prior_claim_comparison_only",
        })

    # The independent supplement sources are the only source-semantic rulings
    # ahead of the shard candidate; r7 remains comparison-only by construction.
    selected: dict[str, Any]
    if supplement is not None:
        candidate = next(claim["value"] for claim in claims if claim["source"] == "independent_65_row_adjudication_supplement")
        origin = "independent_65_row_adjudication_supplement"
        field = next(claim["field"] for claim in claims if claim["source"] == origin)
    elif route16 is not None:
        candidate = route16.get("source_semantic_class")
        origin = "shard_004_route_reclassification_supplement"
        field = "source_semantic_class"
    else:
        candidate = shard.get("expected_class")
        origin = "repaired_independent_shard"
        field = "expected_class"

    if candidate not in ALLOWED_CLASSES:
        selected = {
            "value": "unresolved",
            "status": "missing_or_unsupported_source_semantic_candidate",
            "selected_source": origin,
            "selected_field": field,
            "candidate_value": candidate,
        }
    elif candidate == "expected_build_rejection":
        selected = {
            "value": "not_separately_adjudicated",
            "status": "route_rejection_does_not_define_source_semantics",
            "selected_source": origin,
            "selected_field": field,
            "candidate_value": candidate,
        }
    else:
        selected = {
            "value": candidate,
            "status": (
                "independent_supplement_candidate_not_accepted"
                if origin != "repaired_independent_shard"
                else "independent_shard_candidate_not_accepted"
            ),
            "selected_source": origin,
            "selected_field": field,
        }
    return claims, {"selection": selected, "r7_prior_claim": r7_class}


def route_dimension(
    dimension: str,
    shard: Mapping[str, Any],
    r7: Mapping[str, Any] | None,
    supplement: Mapping[str, Any] | None,
    route16: Mapping[str, Any] | None,
) -> dict[str, Any]:
    claims: list[dict[str, Any]] = []
    if dimension == "default_route_disposition":
        candidates = (
            ("independent_65_row_adjudication_supplement", supplement, "default_route_disposition"),
            ("shard_004_route_reclassification_supplement", route16, "default_route"),
            ("schema_v4_r7_ledger", r7, "default_route_disposition"),
        )
    else:
        candidates = (
            ("independent_65_row_adjudication_supplement", supplement, "explicit_mir_disposition"),
            ("shard_004_route_reclassification_supplement", route16, "explicit_mir_route"),
            ("schema_v4_r7_ledger", r7, "canonical_mir_only_input_rejection"),
        )
    for origin, row, key in candidates:
        if row is not None and key in row:
            claims.append({"source": origin, "field": key, "raw_claim": row[key]})
    if not claims:
        expected = None
        status = "not_adjudicated_no_route_claim"
    else:
        # Keep every claim; selected_claim is a display hint only and has no
        # authority because no route invocation has been performed.
        selected_claim = claims[0]
        expected = selected_claim["raw_claim"]
        status = "route_expectation_metadata_only_not_executed"
    return {
        "status": status,
        "expected_disposition_hint": expected,
        "raw_claims": claims,
        "route_acceptance": False,
        "execution_status": "not_run",
    }


def walk_mappings(value: Any, path: str = "$") -> Iterable[tuple[str, Mapping[str, Any]]]:
    if isinstance(value, Mapping):
        yield path, value
        for key, child in value.items():
            yield from walk_mappings(child, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk_mappings(child, f"{path}[{index}]")


def source_pin_assessment(contract: Any) -> dict[str, Any]:
    true_sha_pin = False
    false_sha_pin = False
    path_pin = False
    markers: list[dict[str, Any]] = []
    for location, obj in walk_mappings(contract):
        for key, value in obj.items():
            lower = key.lower()
            if lower in {"pins_exact_mimi_source_sha256", "exact_source_sha256_pin"}:
                markers.append({"path": location, "field": key, "value": value})
                if value is True:
                    true_sha_pin = True
                elif value is False:
                    false_sha_pin = True
            if lower in {"source_fixture_sha256", "mimi_source_sha256", "exact_source_sha256"}:
                markers.append({"path": location, "field": key, "value": value})
                if isinstance(value, str) and SHA256_RE.fullmatch(value):
                    true_sha_pin = True
            if lower in {"exact_source_pin_status", "source_pin_status", "exact_source_pin"} and isinstance(value, str):
                lower_value = value.lower()
                if "exact fixture path" in lower_value or "exact source path" in lower_value or "include_str" in lower_value:
                    path_pin = True
                if "no exact" in lower_value or "no explicit" in lower_value or "not digest" in lower_value or "no hard-coded" in lower_value:
                    false_sha_pin = True
    if true_sha_pin:
        hash_status = "explicit_exact_source_sha256_pin_present"
    elif false_sha_pin:
        hash_status = "explicitly_no_exact_source_sha256_pin"
    else:
        hash_status = "no_exact_source_sha256_pin_evidence"
    return {
        "exact_source_sha256_pin": true_sha_pin,
        "exact_source_path_pin_evidence": path_pin,
        "sha256_pin_status": hash_status,
        "pin_evidence_fields": markers,
    }


def contract_containers(
    shard: Mapping[str, Any],
    r7: Mapping[str, Any] | None,
    supplement: Mapping[str, Any] | None,
    route16: Mapping[str, Any] | None,
) -> list[dict[str, Any]]:
    containers: list[dict[str, Any]] = []

    def add(origin: str, key: str, raw: Any, list_contracts: bool) -> None:
        if raw is None:
            return
        contracts = raw if list_contracts and isinstance(raw, list) else []
        malformed = list_contracts and not isinstance(raw, list)
        containers.append({
            "source": origin,
            "field": key,
            "raw_container": raw,
            "malformed_contract_list": malformed,
            "contracts": [
                {
                    "ordinal": ordinal,
                    "raw_contract": contract,
                    "pin_assessment": source_pin_assessment(contract),
                    "completeness_assessment": "unproven_incomplete_execution_not_run",
                }
                for ordinal, contract in enumerate(contracts, 1)
            ],
        })

    add("repaired_independent_shard", "direct_test_contracts", shard.get("direct_test_contracts", []), True)
    if r7 is not None:
        add("schema_v4_r7_ledger", "direct_rust_test_contracts", r7.get("direct_rust_test_contracts", []), True)
        add("schema_v4_r7_ledger", "direct_contract_status", r7.get("direct_contract_status"), False)
        add("schema_v4_r7_ledger", "tool_diagnostic_contract", r7.get("tool_diagnostic_contract"), False)
    if supplement is not None:
        add("independent_65_row_adjudication_supplement", "direct_contract_status", supplement.get("direct_contract_status"), False)
        add("independent_65_row_adjudication_supplement", "direct_contracts", supplement.get("direct_contracts", []), True)
    if route16 is not None:
        add(
            "shard_004_route_reclassification_supplement",
            "direct_test_contract_completeness",
            route16.get("direct_test_contract_completeness"),
            False,
        )
    return containers


def summarize_contracts(containers: list[dict[str, Any]]) -> dict[str, Any]:
    flat = [
        contract
        for container in containers
        for contract in container["contracts"]
    ]
    exact_pin_count = sum(contract["pin_assessment"]["exact_source_sha256_pin"] for contract in flat)
    missing_inventory = not flat
    if missing_inventory:
        status = "missing_direct_contract_inventory_blocker"
    elif exact_pin_count == 0:
        status = "contracts_inventoried_without_exact_source_hash_pin"
    else:
        status = "contracts_inventoried_exact_pin_scope_unproven"
    return {
        "status": status,
        "contract_count": len(flat),
        "exact_source_sha256_pin_count": exact_pin_count,
        "exact_source_sha256_pin_completeness": "counted_per_raw_contract_no_acceptance",
        "contract_completeness": "not_proven_complete_execution_not_run",
        "execution_status": "not_run",
        "containers": containers,
    }


def merge_rows(inputs: Mapping[str, Any], repo_root: Path) -> list[dict[str, Any]]:
    shards = inputs["shards"]
    r7_rows = inputs["r7"]
    supplements = inputs["supplements"]
    route16_rows = inputs["route16"]
    if set(shards) != set(r7_rows):
        raise ValueError("shard/r7 source rosters differ")
    if not set(supplements).issubset(shards):
        raise ValueError("65-row supplements contain a source outside the repaired shards")
    if not set(route16_rows).issubset(shards):
        raise ValueError("route-16 supplement contains a source outside the repaired shards")

    result: list[dict[str, Any]] = []
    for path, shard in sorted(shards.items(), key=lambda pair: index_value(pair[1])):
        r7 = r7_rows[path]
        supplement = supplements.get(path)
        route16 = route16_rows.get(path)
        current_path = repo_root / path
        if current_path.is_file() and not current_path.is_symlink():
            current_hash = sha256_file(current_path)
        else:
            current_hash = None
        source_rows = {
            "repaired_shard": shard,
            "r7": r7,
            "supplement_65": supplement,
            "route16": route16,
        }
        hashes = source_hash_evidence(path, current_hash, source_rows)
        claims, selection = class_claims(shard, r7, supplement, route16)
        class_result = dict(selection["selection"])
        blockers: list[str] = []
        if hashes["status"] != "all_current_bindings_match":
            class_result = {
                "value": "unresolved",
                "status": "unresolved_source_hash_mismatch",
                "selected_source": None,
                "selected_field": None,
            }
            blockers.append("one or more source evidence hashes do not match the live source")
        if class_result["value"] in {"unresolved", "not_separately_adjudicated"}:
            blockers.append("source-semantic class remains unresolved or not separately adjudicated")

        contracts = summarize_contracts(contract_containers(shard, r7, supplement, route16))
        if contracts["contract_count"] == 0:
            blockers.append("no direct test contract is inventoried")
        else:
            blockers.append("direct contract coverage is not proven complete and execution is not run")
            if contracts["exact_source_sha256_pin_count"] == 0:
                blockers.append("no inventoried direct contract pins the exact Mimi source SHA-256")

        prior_class = selection["r7_prior_claim"]
        prior_comparison = {
            "r7_class": prior_class,
            "candidate_class": class_result["value"],
            "status": (
                "not_comparable_source_hash_mismatch"
                if hashes["status"] != "all_current_bindings_match"
                else "r7_prior_claim_matches_candidate"
                if prior_class == class_result["value"]
                else "r7_prior_claim_differs_from_candidate"
            ),
            "r7_is_prior_claim_only": True,
        }
        raw_evidence = {
            "repaired_independent_shard": shard,
            "schema_v4_r7_ledger_prior_claim": r7,
            "independent_65_row_adjudication_supplement": supplement,
            "shard_004_route_reclassification_supplement": route16,
        }
        row = {
            "schema": "actor-census-source-bound-schema-v4-orthogonal-row-v1",
            "inventory_index": index_value(shard),
            "path": path,
            "source_sha256_current": current_hash,
            "source_hash_provenance": hashes,
            "source_semantic_class": class_result,
            "source_semantic_class_claims": claims,
            "r7_prior_claim_comparison": prior_comparison,
            "default_route_disposition": route_dimension(
                "default_route_disposition", shard, r7, supplement, route16
            ),
            "canonical_mir_only_input_rejection": route_dimension(
                "canonical_mir_only_input_rejection", shard, r7, supplement, route16
            ),
            "direct_test_contract_status": contracts,
            "execution_status": "not_run",
            "accepted": False,
            "accepted_as_route_pass": False,
            "route_acceptance": False,
            "scope_statement": "source taxonomy reconciliation only; no route acceptance",
            "blockers": blockers,
            "source_evidence_verbatim": raw_evidence,
        }
        result.append(row)
    return result


def make_summary(rows: list[dict[str, Any]], inputs: Mapping[str, Any]) -> dict[str, Any]:
    classes = Counter(row["source_semantic_class"]["value"] for row in rows)
    class_statuses = Counter(row["source_semantic_class"]["status"] for row in rows)
    selected_origins = Counter(
        row["source_semantic_class"].get("selected_source") or "unresolved_hash_mismatch"
        for row in rows
    )
    source_hash_mismatch = sum(
        row["source_hash_provenance"]["status"] != "all_current_bindings_match"
        for row in rows
    )
    direct_missing = sum(
        row["direct_test_contract_status"]["contract_count"] == 0 for row in rows
    )
    direct_contracts = [
        contract
        for row in rows
        for container in row["direct_test_contract_status"]["containers"]
        for contract in container["contracts"]
    ]
    r7_diff = Counter(row["r7_prior_claim_comparison"]["status"] for row in rows)
    return {
        "schema": "actor-census-source-bound-schema-v4-orthogonal-summary-v1",
        "purpose": "source taxonomy reconciliation only",
        "route_acceptance": "none; no route result was executed or accepted",
        "inventory_rows": len(rows),
        "all_execution_status_not_run": all(row["execution_status"] == "not_run" for row in rows),
        "all_accepted_false": all(row["accepted"] is False for row in rows),
        "all_accepted_as_route_pass_false": all(row["accepted_as_route_pass"] is False for row in rows),
        "all_route_acceptance_false": all(row["route_acceptance"] is False for row in rows),
        "source_semantic_class_counts": dict(sorted(classes.items())),
        "source_semantic_class_status_counts": dict(sorted(class_statuses.items())),
        "candidate_origin_counts": dict(sorted(selected_origins.items())),
        "source_hash_mismatch_rows": source_hash_mismatch,
        "r7_prior_claim_comparison_counts": dict(sorted(r7_diff.items())),
        "direct_contract_records": len(direct_contracts),
        "direct_contracts_missing_rows": direct_missing,
        "direct_contracts_with_exact_source_sha256_pin": sum(
            contract["pin_assessment"]["exact_source_sha256_pin"] for contract in direct_contracts
        ),
        "direct_contract_completeness": "not proven; every execution_status remains not_run",
        "blocker_row_counts": {
            key: sum(key in row["blockers"] for row in rows)
            for key in sorted({blocker for row in rows for blocker in row["blockers"]})
        },
        "input_row_counts": {
            "repaired_independent_shards": len(inputs["shards"]),
            "schema_v4_r7_prior_ledger": len(inputs["r7"]),
            "independent_adjudication_supplements": len(inputs["supplements"]),
            "shard_004_route_reclassification_supplements": len(inputs["route16"]),
        },
    }


def validate_rows(rows: list[dict[str, Any]], expected_count: int = 1187) -> None:
    if len(rows) != expected_count:
        raise ValueError(f"expected {expected_count} ledger rows; found {len(rows)}")
    seen_paths: set[str] = set()
    seen_indices: set[int] = set()
    for row in rows:
        path = row.get("path")
        index = row.get("inventory_index")
        if not isinstance(path, str) or path in seen_paths:
            raise ValueError(f"invalid or duplicate source path: {path!r}")
        if not isinstance(index, int) or isinstance(index, bool) or index in seen_indices:
            raise ValueError(f"invalid or duplicate inventory index: {index!r}")
        seen_paths.add(path)
        seen_indices.add(index)
        if row.get("execution_status") != "not_run":
            raise ValueError(f"execution status is not not_run: {path}")
        if row.get("accepted") is not False or row.get("accepted_as_route_pass") is not False:
            raise ValueError(f"acceptance must remain false: {path}")
        if row.get("route_acceptance") is not False:
            raise ValueError(f"route acceptance must remain false: {path}")
        for field in (
            "source_semantic_class",
            "default_route_disposition",
            "canonical_mir_only_input_rejection",
            "direct_test_contract_status",
        ):
            if not isinstance(row.get(field), Mapping):
                raise ValueError(f"missing orthogonal dimension {field}: {path}")
        if row["default_route_disposition"].get("route_acceptance") is not False:
            raise ValueError(f"default route dimension claims acceptance: {path}")
        if row["canonical_mir_only_input_rejection"].get("route_acceptance") is not False:
            raise ValueError(f"MIR-only route dimension claims acceptance: {path}")
        if row["source_semantic_class"].get("value") == "expected_build_rejection":
            raise ValueError(f"route-only build rejection leaked into source-semantic class: {path}")
        if row["source_hash_provenance"]["status"] != "all_current_bindings_match":
            if row["source_semantic_class"].get("value") != "unresolved":
                raise ValueError(f"hash mismatch was not made unresolved: {path}")
        if row["source_semantic_class"].get("selected_source") == "schema_v4_r7_ledger":
            raise ValueError(f"r7 prior claim cannot select the source class: {path}")
    if seen_indices != set(range(1, expected_count + 1)):
        raise ValueError("inventory indices are not the complete contiguous 1..N roster")


def validate_package(output: Path, expected_count: int = 1187) -> None:
    ledger_path = output / "source-bound-schema-v4-orthogonal-ledger-1187.jsonl"
    summary_path = output / "summary.json"
    if not ledger_path.is_file() or not summary_path.is_file():
        raise ValueError("package lacks ledger or summary")
    rows = read_json_or_jsonl(ledger_path)
    validate_rows(rows, expected_count)
    summary = json.loads(summary_path.read_text(encoding="utf-8"))
    if summary.get("route_acceptance") != "none; no route result was executed or accepted":
        raise ValueError("summary does not explicitly deny route acceptance")
    if not all(summary.get(key) is True for key in (
        "all_execution_status_not_run",
        "all_accepted_false",
        "all_accepted_as_route_pass_false",
        "all_route_acceptance_false",
    )):
        raise ValueError("summary acceptance/execution contract failed")


def write_package(output: Path, rows: list[dict[str, Any]], summary: dict[str, Any], inputs: Mapping[str, Any]) -> None:
    if output.exists():
        raise FileExistsError(f"refusing to overwrite an existing evidence package: {output}")
    output.mkdir(parents=True)
    ledger_path = output / "source-bound-schema-v4-orthogonal-ledger-1187.jsonl"
    with ledger_path.open("w", encoding="utf-8", newline="\n") as stream:
        for row in rows:
            stream.write(json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n")
    summary_path = output / "summary.json"
    summary_path.write_text(canonical_json(summary), encoding="utf-8")
    manifest = {
        "schema": "actor-census-source-bound-schema-v4-input-manifest-v1",
        "input_files": [
            {"path": str(path), "sha256": sha256_file(path), "bytes": path.stat().st_size}
            for path in inputs["input_files"]
        ],
        "builder": {
            "path": str(Path(__file__).resolve()),
            "sha256": sha256_file(Path(__file__).resolve()),
            "bytes": Path(__file__).stat().st_size,
        },
        "static_contract_tests": {
            "path": str(Path(__file__).with_name("test_reconcile_actor_census_orthogonal_ledger.py").resolve()),
            "sha256": sha256_file(Path(__file__).with_name("test_reconcile_actor_census_orthogonal_ledger.py")),
        },
        "python_runtime": {
            "implementation": sys.implementation.name,
            "version": sys.version.split()[0],
        },
        "repo_root": str(REPO_ROOT),
        "source_roster_count": len(inputs["shards"]),
    }
    (output / "input-manifest.json").write_text(canonical_json(manifest), encoding="utf-8")
    (output / "SUMMARY.md").write_text(
        "# Actor census orthogonal schema-v4 reconciliation\n\n"
        "Scope: source taxonomy reconciliation only. This package makes **no route acceptance** claim. "
        "No Mimi, Cargo, build, or test execution was performed. All rows remain `not_run`, "
        "`accepted=false`, and `accepted_as_route_pass=false`.\n\n"
        "The source-semantic candidate is kept separate from default-route disposition, "
        "canonical-MIR-only input rejection, direct-test contract status, and execution status. "
        "A MIR-only capability rejection is route evidence and never changes the source-semantic class. "
        "Live source hash mismatches fail closed to `unresolved`; r7 classes remain prior claims only.\n\n"
        f"Rows: {summary['inventory_rows']}\n\n"
        "## Source-semantic candidate counts\n\n"
        + "\n".join(f"- `{key}`: {value}" for key, value in summary["source_semantic_class_counts"].items())
        + "\n\n## Blockers\n\n"
        + "\n".join(f"- {key}: {value}" for key, value in summary["blocker_row_counts"].items())
        + "\n",
        encoding="utf-8",
    )
    (output / "command.txt").write_text(
        "python3 scripts/reconcile_actor_census_orthogonal_ledger.py --output "
        + str(output)
        + "\n",
        encoding="utf-8",
    )
    (output / "exit-code.txt").write_text("0\n", encoding="utf-8")
    checksums = []
    for path in sorted(p for p in output.iterdir() if p.is_file() and p.name != "SHA256SUMS"):
        checksums.append(f"{sha256_file(path)}  {path.name}")
    (output / "SHA256SUMS").write_text("\n".join(checksums) + "\n", encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--repo-root", type=Path, default=REPO_ROOT)
    parser.add_argument("--validate-only", action="store_true")
    parser.add_argument("--expected-count", type=int, default=1187)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.validate_only:
        validate_package(args.output, args.expected_count)
        print(canonical_json({"status": "valid", "package": str(args.output)}), end="")
        return 0
    inputs = read_inputs()
    rows = merge_rows(inputs, args.repo_root)
    validate_rows(rows, args.expected_count)
    summary = make_summary(rows, inputs)
    write_package(args.output, rows, summary, inputs)
    validate_package(args.output, args.expected_count)
    print(canonical_json({"status": "built_and_validated", "package": str(args.output), "summary": summary}), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
