#!/usr/bin/env python3
"""Append direct-test-contract supplements to the source-bound Actor ledger.

This consumer preserves the existing source-semantic ledger as candidate-only
evidence. It does not adjudicate source semantics, accept a route, or execute
Mimi, Cargo, or project tests. Missing shard evidence is represented as a
blocker in a full-roster output rather than being synthesized.
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
SUPPLEMENT_DIR = EVIDENCE_ROOT / "actor-census-direct-contract-supplements-20261113"
R1_DIR = EVIDENCE_ROOT / "actor-census-orthogonal-taxonomy-reconcile-20261113-r1"
R1_LEDGER = R1_DIR / "source-bound-schema-v4-orthogonal-ledger-1187.jsonl"
DEFAULT_OUTPUT = EVIDENCE_ROOT / "actor-census-direct-contract-consumer-20261113-r2"
LEDGER_NAME = "source-bound-schema-v4-direct-contract-ledger-1187.jsonl"
COUNT = 1187
SHA_RE = re.compile(r"^[0-9a-f]{64}$")

SHARDS: dict[int, tuple[int, int, str]] = {
    1: (1, 132, "shard-001-0001-0132.jsonl"),
    2: (133, 264, "shard-002-0133-0264.jsonl"),
    3: (265, 396, "shard-003-0265-0396.jsonl"),
    4: (397, 528, "shard-004-0397-0528.jsonl"),
    5: (529, 660, "shard-005-0529-0660.jsonl"),
    6: (661, 792, "shard-006-0661-0792.jsonl"),
    7: (793, 924, "shard-007-0793-0924.jsonl"),
    8: (925, 1056, "shard-008-0925-1056.jsonl"),
    9: (1057, 1187, "shard-009-1057-1187.jsonl"),
}

# These fields contain source-semantic check proposals or blockers, not
# existing test records. Their raw values are always retained verbatim.
SOURCE_CONTRACT_FIELDS: dict[int, tuple[str, ...]] = {
    1: ("direct_check_contract",),
    2: ("source_check_contract",),
    3: ("proposed_unimplemented_contract", "blocked_unresolved_contract"),
    4: ("source_semantic_contract", "proposed_or_unimplemented_contract"),
    5: ("source_semantic_contract",),
    7: ("proposed_source_contract", "blocked_unresolved_contract"),
    8: ("direct_test_contract",),
    9: ("direct_contract.semantic_check",),
}
EXISTING_CONTRACT_FIELDS: dict[int, tuple[str, ...]] = {
    1: ("related_existing_test_contracts",),
    2: ("existing_direct_test_contracts",),
    3: ("direct_test_contracts",),
    4: ("direct_test_contracts",),
    5: ("existing_direct_test_contracts",),
    6: ("direct_test_contracts",),
    7: ("existing_test_contracts",),
    8: ("existing_test_contracts",),
    9: ("direct_contract.existing_contracts",),
}


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n"


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"input is unavailable or not a regular file: {path}")
    rows: list[dict[str, Any]] = []
    with path.open("rb") as stream:
        for line_number, raw in enumerate(stream, 1):
            if not raw.strip():
                continue
            try:
                row = json.loads(raw.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise ValueError(f"{path}:{line_number}: invalid JSONL: {error}") from error
            if not isinstance(row, dict):
                raise ValueError(f"{path}:{line_number}: row must be a JSON object")
            rows.append(row)
    return rows


def source_path(row: Mapping[str, Any]) -> str:
    value = row.get("path") or row.get("source_path")
    if not isinstance(value, str) or not value:
        raise ValueError("evidence row has no source path")
    return value


def inventory_index(row: Mapping[str, Any]) -> int:
    value = row.get("inventory_index", row.get("index"))
    if not isinstance(value, int) or isinstance(value, bool) or value < 1:
        raise ValueError(f"invalid inventory index: {value!r}")
    return value


def unique_index_and_path(rows: Iterable[dict[str, Any]], label: str) -> dict[int, dict[str, Any]]:
    by_index: dict[int, dict[str, Any]] = {}
    paths: set[str] = set()
    for row in rows:
        index = inventory_index(row)
        path = source_path(row)
        if index in by_index:
            raise ValueError(f"duplicate {label} index {index}")
        if path in paths:
            raise ValueError(f"duplicate {label} source path {path}")
        by_index[index] = row
        paths.add(path)
    return by_index


def safe_source_file(repo_root: Path, path: str) -> Path | None:
    relative = Path(path)
    if relative.is_absolute() or ".." in relative.parts:
        return None
    candidate = repo_root / relative
    try:
        resolved = candidate.resolve(strict=True)
        resolved.relative_to(repo_root.resolve(strict=True))
    except (OSError, ValueError):
        return None
    if not resolved.is_file() or candidate.is_symlink():
        return None
    return resolved


def current_source_sha(repo_root: Path, path: str) -> str | None:
    source_file = safe_source_file(repo_root, path)
    return sha256_file(source_file) if source_file is not None else None


def ledger_class(row: Mapping[str, Any]) -> str | None:
    value = row.get("source_semantic_class")
    if isinstance(value, Mapping):
        value = value.get("value")
    return value if isinstance(value, str) else None


def supplement_class(row: Mapping[str, Any]) -> str | None:
    value = row.get("candidate_semantic_class")
    if isinstance(value, Mapping):
        value = value.get("value")
    return value if isinstance(value, str) else None


def get_nested(row: Mapping[str, Any], dotted: str) -> Any:
    value: Any = row
    for part in dotted.split("."):
        if not isinstance(value, Mapping):
            return None
        value = value.get(part)
    return value


def source_hash_claims(value: Any, location: str = "$") -> list[dict[str, Any]]:
    """Extract source/fixture pins while excluding hashes of test sources."""
    claims: list[dict[str, Any]] = []
    if isinstance(value, Mapping):
        for key, child in value.items():
            key_text = str(key)
            child_location = f"{location}.{key_text}"
            context = (location + "." + key_text).lower()
            if "test" not in context and isinstance(child, str) and SHA_RE.fullmatch(child):
                lower_key = key_text.lower()
                if (
                    "source_sha256" in lower_key
                    or "fixture_sha256" in lower_key
                    or "mimi_source_sha256" in lower_key
                    or lower_key in {"ledger_source_sha256", "original_shard_source_sha256"}
                    or (lower_key == "sha256" and any(tag in context for tag in ("source_hash_recheck", "source_binding", "mimi_source")))
                ):
                    claims.append({"field_path": child_location, "sha256": child})
            claims.extend(source_hash_claims(child, child_location))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            claims.extend(source_hash_claims(child, f"{location}[{index}]"))
    return claims


def source_hash_flag_conflicts(value: Any, location: str = "$") -> list[dict[str, Any]]:
    conflicts: list[dict[str, Any]] = []
    if isinstance(value, Mapping):
        for key, child in value.items():
            key_text = str(key)
            child_location = f"{location}.{key_text}"
            context = (location + "." + key_text).lower()
            lower_key = key_text.lower()
            if "test" not in context and isinstance(child, bool):
                if ("match" in lower_key or "stable" in lower_key or "rechecked" in lower_key) and child is False:
                    conflicts.append({"field_path": child_location, "value": child, "reason": "source_hash_match_or_stability_false"})
                if ("mismatch" in lower_key or "drift" in lower_key) and child is True:
                    conflicts.append({"field_path": child_location, "value": child, "reason": "source_hash_mismatch_or_drift_true"})
            conflicts.extend(source_hash_flag_conflicts(child, child_location))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            conflicts.extend(source_hash_flag_conflicts(child, f"{location}[{index}]"))
    return conflicts


def pin_assessment(contract: Any) -> dict[str, Any]:
    evidence: list[dict[str, Any]] = []
    exact = False
    if isinstance(contract, Mapping):
        for location, obj in walk_mappings(contract):
            source_binding = obj.get("source_binding")
            if isinstance(source_binding, Mapping):
                sha = source_binding.get("expected_sha256") or source_binding.get("sha256")
                exact_path = source_binding.get("exact_path") is True
                proposed_pin = source_binding.get("hash_pinned_by_proposed_contract") is True
                if isinstance(sha, str) and SHA_RE.fullmatch(sha) and (exact_path or proposed_pin):
                    evidence.append({"path": location + ".source_binding", "field": "expected_sha256_or_sha256", "value": sha})
                    exact = True
            for key, value in obj.items():
                lower = str(key).lower()
                if lower in {"exact_source_sha256_pin", "pins_exact_mimi_source_sha256"}:
                    evidence.append({"path": location, "field": key, "value": value})
                    exact = exact or value is True
                elif lower in {"pinned_source_sha256", "source_fixture_sha256", "mimi_source_sha256"}:
                    evidence.append({"path": location, "field": key, "value": value})
                    exact = exact or isinstance(value, str) and SHA_RE.fullmatch(value) is not None
    return {
        "exact_source_sha256_pin": exact,
        "status": "explicit_exact_source_sha256_pin_present" if exact else "exact_source_sha256_pin_not_proven",
        "evidence_fields": evidence,
    }


def walk_mappings(value: Any, path: str = "$") -> Iterable[tuple[str, Mapping[str, Any]]]:
    if isinstance(value, Mapping):
        yield path, value
        for key, child in value.items():
            yield from walk_mappings(child, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk_mappings(child, f"{path}[{index}]")


def validate_input_execution_authority(value: Any, location: str = "$") -> None:
    """Reject any direct supplement that claims execution, acceptance, or authority."""
    if isinstance(value, Mapping):
        for key, child in value.items():
            name = str(key).lower()
            child_location = f"{location}.{key}"
            if name in {"accepted", "accepted_as_route_pass", "route_acceptance", "source_semantic_acceptance", "bootstrap_or_runtime_observation_used_as_authority"} and child is not False:
                raise ValueError(f"supplement acceptance/authority flag is not false: {child_location}={child!r}")
            if name in {"execution_status", "test_execution_status", "ledger_execution_status"} and child != "not_run":
                raise ValueError(f"supplement execution status is not not_run: {child_location}={child!r}")
            if ("mimi" in name or "cargo" in name or "test" in name or name == "executed") and "executed" in name and child is True:
                raise ValueError(f"supplement claims execution: {child_location}")
            if ("runtime_observation_used" in name or "bootstrap_or_runtime_observation_used" in name) and child is True:
                raise ValueError(f"supplement grants runtime/bootstrap observation authority: {child_location}")
            validate_input_execution_authority(child, child_location)
    elif isinstance(value, list):
        for index, child in enumerate(value):
            validate_input_execution_authority(child, f"{location}[{index}]")


def contract_record(category: str, source_field: str, raw: Any, ordinal: int) -> dict[str, Any]:
    return {
        "ordinal": ordinal,
        "category": category,
        "source_field": source_field,
        "raw_contract_verbatim": raw,
        "pin_assessment": pin_assessment(raw),
        "execution_status": "not_run",
        "accepted": False,
        "route_acceptance": False,
        "completeness_status": "unproven_not_run",
    }


def extract_contracts(shard_id: int, supplement: Mapping[str, Any] | None) -> dict[str, Any]:
    records: list[dict[str, Any]] = []
    if supplement is None:
        return {
            "status": "missing_required_shard_blocker",
            "records": [],
            "record_count": 0,
            "exact_pin_count": 0,
            "completeness_status": "missing_and_unproven",
            "execution_status": "not_run",
            "accepted": False,
            "route_acceptance": False,
        }

    for field in EXISTING_CONTRACT_FIELDS.get(shard_id, ()):
        raw = get_nested(supplement, field)
        if raw is None:
            continue
        if not isinstance(raw, list):
            records.append(contract_record("existing_test_contract_container_malformed", field, raw, 1))
            continue
        for ordinal, contract in enumerate(raw, 1):
            if shard_id == 6 and isinstance(contract, Mapping):
                implementation = str(contract.get("implementation_status", "")).lower()
                if implementation == "proposed_unimplemented":
                    category = "proposed_or_unimplemented_source_contract"
                elif "blocked" in implementation or "unresolved" in implementation:
                    category = "blocked_unresolved_source_contract"
                else:
                    category = "existing_test_contract"
            else:
                category = "existing_test_contract"
            records.append(contract_record(category, field, contract, ordinal))

    for field in SOURCE_CONTRACT_FIELDS.get(shard_id, ()):
        raw = get_nested(supplement, field)
        if raw is None:
            continue
        status = raw.get("status") if isinstance(raw, Mapping) else None
        status_text = str(status or "").lower()
        if "blocked" in status_text or "unresolved" in status_text:
            category = "blocked_unresolved_source_contract"
        elif "no_new_contract" in status_text or "not_adjudicated" in status_text:
            category = "no_new_source_contract_proposed"
        else:
            category = "proposed_or_unimplemented_source_contract"
        records.append(contract_record(category, field, raw, 1))

    for ordinal, record in enumerate(records, 1):
        record["ordinal_in_row"] = ordinal
        record["effective_behavior"] = None
        record["effective_behavior_authority"] = "none; raw contract is candidate evidence only"
    pin_count = sum(item["pin_assessment"]["exact_source_sha256_pin"] for item in records)
    return {
        "status": "contracts_preserved_candidate_only" if records else "missing_direct_contract_inventory_blocker",
        "records": records,
        "record_count": len(records),
        "exact_pin_count": pin_count,
        "completeness_status": "unproven_not_run" if records else "missing_and_unproven",
        "execution_status": "not_run",
        "accepted": False,
        "route_acceptance": False,
    }


def unresolved_source_contract_behavior(contract_status: Mapping[str, Any], unresolved: bool) -> dict[str, Any]:
    if not unresolved:
        return {"status": "not_applicable_candidate_not_unresolved", "effective_behavior": None}
    behavior_fields = {
        "assertions",
        "expected_behavior",
        "candidate_expected_behavior",
        "proposed_expectation",
        "expected_check_or_runtime_result",
        "behavioral_assertions",
        "behavioral_command",
        "command",
        "reproducible_command_not_run",
    }
    findings: list[dict[str, Any]] = []
    for record in contract_status["records"]:
        if record["category"] == "existing_test_contract":
            continue
        raw = record["raw_contract_verbatim"]
        for location, obj in walk_mappings(raw):
            for key, value in obj.items():
                if str(key).lower() in behavior_fields and value not in (None, [], ""):
                    findings.append({"source_field": record["source_field"], "path": f"{location}.{key}"})
    return {
        "status": "blocked_behavior_disabled_no_source_semantic_assertion" if findings else "no_behavior_expectation_on_unresolved_source",
        "raw_candidate_behavior_findings": findings,
        "effective_behavior": None,
        "effective_behavior_authority": "none_while_unresolved",
    }


def shard_id_for_index(index: int) -> int:
    for shard_id, (first, last, _) in SHARDS.items():
        if first <= index <= last:
            return shard_id
    raise ValueError(f"inventory index outside required roster: {index}")


def load_inputs() -> dict[str, Any]:
    ledger_rows = read_jsonl(R1_LEDGER)
    ledger = unique_index_and_path(ledger_rows, "r1 ledger")
    if len(ledger) != COUNT or set(ledger) != set(range(1, COUNT + 1)):
        raise ValueError("r1 ledger must contain exactly the contiguous 1..1187 roster")

    supplements: dict[int, dict[str, Any]] = {}
    shard_files: dict[int, Path] = {}
    for shard_id, (_, _, filename) in SHARDS.items():
        path = SUPPLEMENT_DIR / filename
        if not path.exists():
            continue
        rows = read_jsonl(path)
        first, last, _ = SHARDS[shard_id]
        expected = set(range(first, last + 1))
        indexed = unique_index_and_path(rows, f"direct supplement shard {shard_id:03d}")
        if set(indexed) != expected:
            raise ValueError(
                f"shard {shard_id:03d} must contain indices {first}..{last}; "
                f"found {len(indexed)} rows"
            )
        for index, row in indexed.items():
            base = ledger.get(index)
            if base is None or source_path(row) != source_path(base):
                raise ValueError(f"supplement row {index} source path differs from r1 ledger")
            validate_input_execution_authority(row)
            supplements[index] = row
        shard_files[shard_id] = path
    return {"ledger": ledger, "supplements": supplements, "shard_files": shard_files}


def route_metadata(base: Mapping[str, Any], supplement: Mapping[str, Any] | None) -> dict[str, Any]:
    return {
        "r1_ledger_claim": {
            "default_route_disposition": base.get("default_route_disposition"),
            "canonical_mir_only_input_rejection": base.get("canonical_mir_only_input_rejection"),
        },
        "direct_supplement_route_metadata": (
            {
                key: value for key, value in supplement.items()
                if "route" in key.lower() or "disposition" in key.lower()
            }
            if supplement is not None else None
        ),
        "execution_status": "not_run",
        "accepted": False,
        "route_acceptance": False,
        "authority": "expectation_metadata_only_not_a_route_result",
    }


def build_rows(inputs: Mapping[str, Any], repo_root: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    ledger = inputs["ledger"]
    supplements = inputs["supplements"]
    for index in range(1, COUNT + 1):
        base = ledger[index]
        path = source_path(base)
        shard_id = shard_id_for_index(index)
        supplement = supplements.get(index)
        live_sha = current_source_sha(repo_root, path)
        ledger_sha = base.get("source_sha256_current")
        supplement_sha = supplement.get("source_sha256_current") if supplement else None
        class_candidate = ledger_class(base)
        supplement_candidate = supplement_class(supplement) if supplement else None
        mismatch_reasons: list[str] = []
        if live_sha is None:
            mismatch_reasons.append("live_source_missing_or_unsafe")
        if not isinstance(ledger_sha, str) or ledger_sha != live_sha:
            mismatch_reasons.append("live_source_sha_differs_from_r1_ledger")
        if supplement is not None and supplement_sha != live_sha:
            mismatch_reasons.append("live_source_sha_differs_from_direct_supplement")
        if supplement is not None and supplement_candidate != class_candidate:
            mismatch_reasons.append("supplement_candidate_class_differs_from_orthogonal_ledger")
        if supplement is not None:
            for claim in source_hash_claims(supplement):
                if claim["sha256"] != live_sha:
                    mismatch_reasons.append("embedded_source_hash_claim_differs_from_live_source")
                    break
            if source_hash_flag_conflicts(supplement):
                mismatch_reasons.append("source_hash_recheck_flag_reports_mismatch_or_drift")

        prior_hash_status = get_nested(base, "source_hash_provenance.status")
        prior_unresolved = prior_hash_status != "all_current_bindings_match"
        if prior_unresolved:
            mismatch_reasons.append("r1_prior_source_hash_provenance_unresolved_retained")
        source_unresolved = bool(mismatch_reasons) or class_candidate == "unresolved"
        effective_class = "unresolved" if source_unresolved else class_candidate
        contract_status = extract_contracts(shard_id, supplement)
        if source_unresolved:
            for record in contract_status["records"]:
                if record["category"] != "existing_test_contract":
                    record["effective_status"] = "disabled_while_source_semantic_class_unresolved"
        else:
            for record in contract_status["records"]:
                if record["category"] != "existing_test_contract":
                    record["effective_status"] = "candidate_only_not_accepted"
        behavior_review = unresolved_source_contract_behavior(contract_status, source_unresolved)
        blockers = []
        if supplement is None:
            first, last, filename = SHARDS[shard_id]
            blockers.append(f"required direct-contract supplement missing: {filename} ({first}..{last})")
        if mismatch_reasons:
            blockers.extend(sorted(set(mismatch_reasons)))
        if contract_status["record_count"] == 0:
            blockers.append("direct test contract inventory is missing")
        else:
            blockers.append("direct test contract completeness and execution remain unproven")
        if behavior_review.get("raw_candidate_behavior_findings"):
            blockers.append("unresolved source contract contains raw behavior fields; they remain inactive")

        rows.append({
            "schema": "actor-census-source-bound-schema-v4-direct-contract-row-r2",
            "inventory_index": index,
            "path": path,
            "shard_id": shard_id,
            "source_semantic_class": {
                "value": effective_class,
                "ledger_candidate_value": class_candidate,
                "supplement_candidate_value": supplement_candidate,
                "candidate_match_status": (
                    "missing_supplement_candidate" if supplement is None
                    else "matches_orthogonal_ledger" if supplement_candidate == class_candidate
                    else "mismatch_unresolved"
                ),
                "prior_r1_class_status": get_nested(base, "source_semantic_class.status"),
                "candidate_only": True,
                "accepted": False,
            },
            "source_semantic_acceptance": False,
            "source_semantic_adjudication_evidence": {
                "status": "not_consumed_by_direct_contract_r2",
                "scope": "direct-test-contract supplement ingestion only",
                "source_semantic_decision": "none",
                "future_input_interface": {
                    "expected_external_filename_pattern": "actor-census-unresolved-independent-adjudication-<start>-<end>-20261113.jsonl",
                    "join_key": ["inventory_index", "source_path"],
                    "required_hash_bindings": ["live source SHA-256", "r1 ledger source SHA-256", "supplement source_sha256_current"],
                    "candidate_fields_are_evidence_only": True,
                    "semantic_acceptance": False,
                    "route_acceptance": False,
                },
            },
            "source_hash_validation": {
                "live_source_sha256": live_sha,
                "r1_ledger_source_sha256": ledger_sha,
                "direct_supplement_source_sha256": supplement_sha,
                "live_matches_r1_ledger": live_sha is not None and live_sha == ledger_sha,
                "live_matches_direct_supplement": supplement is None or live_sha is not None and live_sha == supplement_sha,
                "r1_prior_provenance_status": prior_hash_status,
                "r1_prior_unresolved_retained": prior_unresolved,
                "mismatch_reasons": sorted(set(mismatch_reasons)),
            },
            "default_route_disposition": route_metadata(base, supplement)["r1_ledger_claim"]["default_route_disposition"],
            "canonical_mir_only_input_rejection": route_metadata(base, supplement)["r1_ledger_claim"]["canonical_mir_only_input_rejection"],
            "route_metadata_only": route_metadata(base, supplement),
            "direct_test_contract_status": contract_status,
            "unresolved_source_contract_behavior_review": behavior_review,
            "execution_status": "not_run",
            "accepted": False,
            "accepted_as_route_pass": False,
            "route_acceptance": False,
            "runtime_or_bootstrap_authority": False,
            "runtime_or_bootstrap_observation_used_as_authority": False,
            "blockers": blockers,
            "r1_orthogonal_ledger_row_verbatim": base,
            "direct_contract_supplement_row_verbatim": supplement,
        })
    return rows


def package_status(shard_files: Mapping[int, Path]) -> tuple[str, list[str]]:
    missing = [
        f"shard-{shard_id:03d}:{SHARDS[shard_id][2]}:{SHARDS[shard_id][0]}-{SHARDS[shard_id][1]}"
        for shard_id in SHARDS if shard_id not in shard_files
    ]
    return ("complete_input_roster" if not missing else "blocked_missing_required_shards", missing)


def make_summary(rows: list[dict[str, Any]], inputs: Mapping[str, Any]) -> dict[str, Any]:
    roster_status, missing = package_status(inputs["shard_files"])
    classes = Counter(row["source_semantic_class"]["value"] for row in rows)
    candidates = Counter(row["source_semantic_class"]["ledger_candidate_value"] for row in rows)
    contracts = [
        record
        for row in rows
        for record in row["direct_test_contract_status"]["records"]
    ]
    candidate_mismatches = sum(
        row["source_semantic_class"]["candidate_match_status"] == "mismatch_unresolved"
        for row in rows
    )
    package_state = roster_status
    if roster_status == "complete_input_roster" and candidate_mismatches:
        package_state = "blocked_candidate_class_mismatch"
    candidate_mismatch_details = [
        {
            "inventory_index": row["inventory_index"],
            "path": row["path"],
            "r1_ledger_candidate": row["source_semantic_class"]["ledger_candidate_value"],
            "direct_supplement_candidate": row["source_semantic_class"]["supplement_candidate_value"],
            "effective_value": row["source_semantic_class"]["value"],
        }
        for row in rows
        if row["source_semantic_class"]["candidate_match_status"] == "mismatch_unresolved"
    ]
    lineage_unresolved_details = [
        {"inventory_index": row["inventory_index"], "path": row["path"], "reasons": row["source_hash_validation"]["mismatch_reasons"]}
        for row in rows
        if row["source_hash_validation"]["r1_prior_unresolved_retained"]
    ]
    return {
        "schema": "actor-census-direct-contract-consumer-summary-r2",
        "package_status": package_state,
        "direct_contract_inventory_status": "complete_1187_candidate_only" if len(inputs["supplements"]) == COUNT else "blocked_incomplete_supplement_inventory",
        "input_roster_status": roster_status,
        "purpose": "append-only direct-test-contract supplement ingestion",
        "source_semantic_acceptance": "none; r1 classes remain candidate-only",
        "source_semantic_adjudication": "not consumed; this package does not adjudicate source semantics",
        "route_acceptance": "none; no route result was executed or accepted",
        "execution_status": "not_run",
        "inventory_rows": len(rows),
        "unique_indices": len({row["inventory_index"] for row in rows}),
        "unique_source_paths": len({row["path"] for row in rows}),
        "expected_rows": COUNT,
        "required_shards": len(SHARDS),
        "present_shards": len(inputs["shard_files"]),
        "missing_shards": missing,
        "direct_supplement_rows": len(inputs["supplements"]),
        "live_sha_matches_r1_ledger_rows": sum(row["source_hash_validation"]["live_matches_r1_ledger"] for row in rows),
        "live_sha_matches_direct_supplement_rows": sum(row["source_hash_validation"]["live_matches_direct_supplement"] for row in rows),
        "prior_r1_unresolved_source_hash_rows_retained": sum(row["source_hash_validation"]["r1_prior_unresolved_retained"] for row in rows),
        "live_or_supplement_source_sha_binding_mismatch_rows": sum(not row["source_hash_validation"]["live_matches_r1_ledger"] or not row["source_hash_validation"]["live_matches_direct_supplement"] for row in rows),
        "source_lineage_or_candidate_mismatch_rows": sum(bool(row["source_hash_validation"]["mismatch_reasons"]) for row in rows),
        "candidate_class_match_rows": sum(row["source_semantic_class"]["candidate_match_status"] == "matches_orthogonal_ledger" for row in rows),
        "candidate_class_mismatch_rows_retained_unresolved": candidate_mismatches,
        "candidate_class_mismatch_details": candidate_mismatch_details,
        "r1_prior_lineage_mismatch_details_retained": lineage_unresolved_details,
        "candidate_class_reconciliation_status": (
            "blocked_by_candidate_mismatch" if any(row["source_semantic_class"]["candidate_match_status"] == "mismatch_unresolved" for row in rows)
            else "all_candidates_match_r1_ledger"
        ),
        "effective_source_semantic_class_counts": dict(sorted(classes.items())),
        "r1_ledger_candidate_class_counts": dict(sorted(candidates.items())),
        "direct_contract_records": len(contracts),
        "existing_test_contract_records": sum(record["category"] == "existing_test_contract" for record in contracts),
        "proposed_or_unimplemented_source_contract_records": sum(record["category"] == "proposed_or_unimplemented_source_contract" for record in contracts),
        "blocked_unresolved_source_contract_records": sum(record["category"] == "blocked_unresolved_source_contract" for record in contracts),
        "no_new_source_contract_records": sum(record["category"] == "no_new_source_contract_proposed" for record in contracts),
        "exact_source_sha256_pinned_contract_records": sum(record["pin_assessment"]["exact_source_sha256_pin"] for record in contracts),
        "unresolved_rows_with_raw_behavior_fields_disabled": sum(bool(row["unresolved_source_contract_behavior_review"].get("raw_candidate_behavior_findings")) for row in rows),
        "contract_completeness": "not_proven; records preserved and all executions not_run",
        "all_execution_status_not_run": all(row["execution_status"] == "not_run" for row in rows) and all(contract["execution_status"] == "not_run" for contract in contracts),
        "all_accepted_false": all(row["accepted"] is False for row in rows) and all(contract["accepted"] is False for contract in contracts),
        "all_source_semantic_acceptance_false": all(row["source_semantic_acceptance"] is False for row in rows),
        "all_route_acceptance_false": all(row["route_acceptance"] is False for row in rows) and all(contract["route_acceptance"] is False for contract in contracts),
        "all_runtime_or_bootstrap_authority_false": all(row["runtime_or_bootstrap_authority"] is False for row in rows),
        "blocker_rows": sum(bool(row["blockers"]) for row in rows),
    }


def validate_rows(rows: list[dict[str, Any]], expected_count: int = COUNT) -> None:
    if len(rows) != expected_count:
        raise ValueError(f"expected {expected_count} rows, found {len(rows)}")
    indices: set[int] = set()
    paths: set[str] = set()
    for row in rows:
        index = row.get("inventory_index")
        path = row.get("path")
        if not isinstance(index, int) or isinstance(index, bool) or index in indices:
            raise ValueError(f"invalid or duplicate inventory index: {index!r}")
        if not isinstance(path, str) or not path or path in paths:
            raise ValueError(f"invalid or duplicate source path: {path!r}")
        indices.add(index)
        paths.add(path)
        if row.get("execution_status") != "not_run" or row.get("accepted") is not False:
            raise ValueError(f"row execution/acceptance gate failed: {path}")
        if row.get("source_semantic_acceptance") is not False or row.get("route_acceptance") is not False:
            raise ValueError(f"semantic or route acceptance must remain false: {path}")
        if row.get("accepted_as_route_pass") is not False:
            raise ValueError(f"route pass must remain false: {path}")
        if row.get("runtime_or_bootstrap_authority") is not False:
            raise ValueError(f"runtime/bootstrap authority must remain false: {path}")
        if row["source_semantic_class"].get("accepted") is not False:
            raise ValueError(f"source class candidate cannot be accepted: {path}")
        if row["source_semantic_adjudication_evidence"].get("status") != "not_consumed_by_direct_contract_r2":
            raise ValueError(f"r2 incorrectly claims source-semantic adjudication: {path}")
        future_interface = row["source_semantic_adjudication_evidence"].get("future_input_interface", {})
        if future_interface.get("semantic_acceptance") is not False or future_interface.get("route_acceptance") is not False:
            raise ValueError(f"future adjudication input interface grants acceptance: {path}")
        if row.get("direct_contract_supplement_row_verbatim") is not None:
            validate_input_execution_authority(row["direct_contract_supplement_row_verbatim"])
        if row["route_metadata_only"].get("route_acceptance") is not False:
            raise ValueError(f"route metadata claims acceptance: {path}")
        for dimension in (row.get("default_route_disposition"), row.get("canonical_mir_only_input_rejection")):
            if isinstance(dimension, Mapping) and dimension.get("route_acceptance") is not False:
                raise ValueError(f"orthogonal route dimension claims acceptance: {path}")
        validation = row["source_hash_validation"]
        if validation["live_matches_r1_ledger"] is not True and row["source_semantic_class"]["value"] != "unresolved":
            raise ValueError(f"live/r1 source mismatch not unresolved: {path}")
        if validation["live_matches_direct_supplement"] is not True and row["direct_contract_supplement_row_verbatim"] is not None and row["source_semantic_class"]["value"] != "unresolved":
            raise ValueError(f"live/supplement source mismatch not unresolved: {path}")
        if row["source_semantic_class"]["candidate_match_status"] == "mismatch_unresolved" and row["source_semantic_class"]["value"] != "unresolved":
            raise ValueError(f"candidate class mismatch not unresolved: {path}")
        if validation["r1_prior_unresolved_retained"] and row["source_semantic_class"]["value"] != "unresolved":
            raise ValueError(f"prior r1 source mismatch was cleared: {path}")
        contract_status = row["direct_test_contract_status"]
        if contract_status.get("execution_status") != "not_run" or contract_status.get("accepted") is not False or contract_status.get("route_acceptance") is not False:
            raise ValueError(f"contract container acceptance/execution gate failed: {path}")
        for contract in contract_status["records"]:
            if contract.get("execution_status") != "not_run" or contract.get("accepted") is not False or contract.get("route_acceptance") is not False:
                raise ValueError(f"contract row acceptance/execution gate failed: {path}")
            if contract.get("effective_behavior") is not None or contract.get("effective_behavior_authority") != "none; raw contract is candidate evidence only":
                raise ValueError(f"contract record grants effective behavior authority: {path}")
            if row["source_semantic_class"]["value"] == "unresolved" and contract["category"] != "existing_test_contract" and contract.get("effective_status") != "disabled_while_source_semantic_class_unresolved":
                raise ValueError(f"unresolved source contract was not disabled: {path}")
        if row["source_semantic_class"]["value"] == "unresolved":
            review = row["unresolved_source_contract_behavior_review"]
            if review.get("effective_behavior") is not None or review.get("effective_behavior_authority") not in (None, "none_while_unresolved"):
                raise ValueError(f"unresolved source row has effective behavior: {path}")
    if indices != set(range(1, expected_count + 1)):
        raise ValueError("inventory indices are not the contiguous 1..N roster")


def validate_package(output: Path) -> None:
    rows = read_jsonl(output / LEDGER_NAME)
    validate_rows(rows)
    summary = json.loads((output / "summary.json").read_text(encoding="utf-8"))
    if summary.get("route_acceptance") != "none; no route result was executed or accepted":
        raise ValueError("summary fails to deny route acceptance")
    if summary.get("source_semantic_acceptance") != "none; r1 classes remain candidate-only":
        raise ValueError("summary fails to separate source-semantic acceptance")
    if summary.get("input_roster_status") == "complete_input_roster" and summary.get("present_shards") != 9:
        raise ValueError("complete status requires all nine direct-contract shards")
    if summary.get("input_roster_status") == "blocked_missing_required_shards" and not summary.get("missing_shards"):
        raise ValueError("blocked status has no missing-shard blocker")
    if summary.get("candidate_class_mismatch_rows_retained_unresolved", 0) and summary.get("package_status") != "blocked_candidate_class_mismatch":
        raise ValueError("candidate-class mismatch must keep the package blocked")
    for key in ("all_execution_status_not_run", "all_accepted_false", "all_source_semantic_acceptance_false", "all_route_acceptance_false", "all_runtime_or_bootstrap_authority_false"):
        if summary.get(key) is not True:
            raise ValueError(f"summary gate is not true: {key}")


def write_package(output: Path, rows: list[dict[str, Any]], summary: dict[str, Any], inputs: Mapping[str, Any]) -> None:
    if output.exists():
        raise FileExistsError(f"refusing to overwrite existing evidence package: {output}")
    output.mkdir(parents=True)
    ledger_path = output / LEDGER_NAME
    with ledger_path.open("w", encoding="utf-8", newline="\n") as stream:
        for row in rows:
            stream.write(json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n")
    (output / "summary.json").write_text(canonical_json(summary), encoding="utf-8")
    (output / "SUMMARY.md").write_text(
        "# Actor census direct-contract supplement consumer r2\n\n"
        f"Package status: **{summary['package_status']}**. Direct-contract inventory: **{summary['direct_contract_inventory_status']}**. Scope: append-only direct-test-contract ingestion. "
        "R1 source-semantic classes remain candidate-only; this package performs no source-semantic adjudication. "
        "No route was accepted and no Mimi/Cargo/project test was run. Every row and contract remains not_run and accepted=false.\n\n"
        f"Rows: {summary['inventory_rows']} / {summary['expected_rows']}\n\n"
        f"Direct-contract supplement rows: {summary['direct_supplement_rows']} / {summary['expected_rows']}\n\n"
        "Missing required shards:\n\n"
        + ("\n".join(f"- {item}" for item in summary["missing_shards"]) if summary["missing_shards"] else "- none")
        + "\n\n## Contract input shape crosswalk\n\n"
        "- shard 001: one proposed direct-check contract plus a separate related-existing-tests list.\n"
        "- shard 002: one source-check contract plus a separate existing-direct-tests list.\n"
        "- shard 003: existing direct-test list plus separate proposed and blocked source contracts.\n"
        "- shard 004: object-shaped candidate class; existing static tests, source-semantic question, and optional no-new/proposed record are separate.\n"
        "- shard 005: source-semantic contract and existing direct-test list are separate.\n"
        "- shard 006: one mixed direct_test_contracts list; implementation_status splits existing, proposed_unimplemented, and blocked_unresolved entries without changing raw records.\n"
        "- shard 007: existing tests, proposed source contract, and blocked contract are separate.\n"
        "- shard 008: proposed direct-test contract and existing-test list are separate.\n"
        "- shard 009: direct_contract wrapper separates semantic_check from existing_contracts.\n\n"
        "Source-semantic acceptance: none. Route acceptance: none. Runtime/bootstrap observations have no authority. "
        "Raw proposals attached to unresolved rows remain preserved but are disabled as effective behavior. "
        "The future unresolved-adjudication input interface is recorded separately and is not consumed by this direct-contract package.\n",
        encoding="utf-8",
    )
    manifest = {
        "schema": "actor-census-direct-contract-consumer-manifest-r2",
        "r1_ledger": {"path": str(R1_LEDGER), "sha256": sha256_file(R1_LEDGER), "bytes": R1_LEDGER.stat().st_size},
        "present_supplement_shards": [
            {"shard_id": shard_id, "path": str(path), "sha256": sha256_file(path), "bytes": path.stat().st_size}
            for shard_id, path in sorted(inputs["shard_files"].items())
        ],
        "missing_shards": summary["missing_shards"],
        "future_unresolved_adjudication_input_interface": {
            "expected_external_filename_pattern": "actor-census-unresolved-independent-adjudication-<start>-<end>-20261113.jsonl",
            "required_row_key": ["inventory_index", "source_path"],
            "required_source_binding": ["source_sha256_current == live source SHA-256 == r1 ledger SHA-256"],
            "merge_destination": "source_semantic_adjudication_evidence only",
            "cannot_change_candidate_class_or_accept_source_semantics_or_route": True,
            "status": "not consumed by direct-contract r2",
        },
        "builder": {"path": str(Path(__file__).resolve()), "sha256": sha256_file(Path(__file__).resolve()), "bytes": Path(__file__).stat().st_size},
        "python_unit_tests": {"path": str(Path(__file__).with_name("test_consume_actor_direct_contract_supplements.py").resolve()), "sha256": sha256_file(Path(__file__).with_name("test_consume_actor_direct_contract_supplements.py"))},
        "python_runtime": {"implementation": sys.implementation.name, "version": sys.version.split()[0]},
        "repo_root": str(REPO_ROOT),
    }
    (output / "input-manifest.json").write_text(canonical_json(manifest), encoding="utf-8")
    (output / "command.txt").write_text(
        "python3 scripts/consume_actor_direct_contract_supplements.py --output " + str(output) + "\n",
        encoding="utf-8",
    )
    (output / "exit-code.txt").write_text("0\n" if summary["package_status"] == "complete_input_roster" else "2\n", encoding="utf-8")
    checksum_lines = []
    for path in sorted(item for item in output.iterdir() if item.is_file() and item.name != "SHA256SUMS"):
        checksum_lines.append(f"{sha256_file(path)}  {path.name}")
    (output / "SHA256SUMS").write_text("\n".join(checksum_lines) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--repo-root", type=Path, default=REPO_ROOT)
    parser.add_argument("--validate-package", type=Path)
    args = parser.parse_args(argv)
    if args.validate_package is not None:
        validate_package(args.validate_package)
        return 0
    inputs = load_inputs()
    rows = build_rows(inputs, args.repo_root)
    validate_rows(rows)
    summary = make_summary(rows, inputs)
    write_package(args.output, rows, summary, inputs)
    validate_package(args.output)
    print(json.dumps({"output": str(args.output), "package_status": summary["package_status"], "missing_shards": summary["missing_shards"], "rows": len(rows)}, ensure_ascii=False, sort_keys=True))
    return 0 if summary["package_status"] == "complete_input_roster" else 2


if __name__ == "__main__":
    raise SystemExit(main())
