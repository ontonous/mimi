#!/usr/bin/env python3
"""Append-only consumer for the independent Actor census adjudication shards.

This tool consumes the frozen r2 source-bound ledger and all nine independent
adjudication supplements. It never executes Mimi, Cargo, tests, or builds.
Source classification remains a candidate field; route disposition and direct
contract inventory are kept on separate axes and never accepted here.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import tempfile
from collections import Counter
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_INPUT_ROOT = Path(
    "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census"
)
R2_RELATIVE_PATH = Path(
    "actor-census-direct-contract-consumer-20261113-r2"
) / "source-bound-schema-v4-direct-contract-ledger-1187.jsonl"
ADJUDICATION_FILENAMES = (
    "actor-census-unresolved-independent-adjudication-001-0132-20261113.jsonl",
    "actor-census-unresolved-independent-adjudication-0133-0264-20261113.jsonl",
    "actor-census-unresolved-independent-adjudication-0265-0396-20261113.jsonl",
    "actor-census-unresolved-independent-adjudication-0397-0528-20261113.jsonl",
    "actor-census-unresolved-independent-adjudication-0529-0660-20261113.jsonl",
    "actor-census-unresolved-independent-adjudication-0661-0792-20261113.jsonl",
    "actor-census-unresolved-independent-adjudication-0793-0924-20261113.jsonl",
    "actor-census-unresolved-independent-adjudication-0925-1056-20261113.jsonl",
    "actor-census-unresolved-independent-adjudication-1057-1187-20261113.jsonl",
)
ADJUDICATION_SHARD_RANGES = {
    ADJUDICATION_FILENAMES[0]: (1, 132),
    ADJUDICATION_FILENAMES[1]: (133, 264),
    ADJUDICATION_FILENAMES[2]: (265, 396),
    ADJUDICATION_FILENAMES[3]: (397, 528),
    ADJUDICATION_FILENAMES[4]: (529, 660),
    ADJUDICATION_FILENAMES[5]: (661, 792),
    ADJUDICATION_FILENAMES[6]: (793, 924),
    ADJUDICATION_FILENAMES[7]: (925, 1056),
    ADJUDICATION_FILENAMES[8]: (1057, 1187),
}
COUNT = 1187
UNRESOLVED = "unresolved"
ROUTE_ONLY_CLASS = "expected_build_rejection"
ALLOWED_SOURCE_CLASSES = {
    "required_compile",
    "expected_source_diagnostic",
    "interpreter_only",
    "known_gap",
    "unmapped",
    "environment_failure",
    UNRESOLVED,
}
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
ADJUDICATION_CLASS_FIELDS = (
    "independent_candidate_class",
    "candidate_semantic_class",
    "candidate_class",
    "independent_source_semantic_candidate_class",
    "candidate_source_semantic_class",
)
DIRECT_CONTRACT_FIELDS = (
    "direct_contract",
    "direct_check_contract",
    "proposed_direct_contract",
    "direct_test_contracts",
    "source_identity_check",
)
ACCEPTANCE_FIELDS = {
    "accepted",
    "accepted_as_route_pass",
    "route_acceptance",
    "source_semantic_acceptance",
}
EXECUTION_FIELDS = {
    "execution_status",
    "mimi_or_cargo_or_test_or_build_or_bootstrap_executed",
    "mimi_or_cargo_or_test_or_build_executed",
    "mimi_cargo_test_build_executed",
    "test_or_build_or_mimi_executed",
    "bootstrap_or_runtime_observation_used",
    "runtime_or_test_observation_used_as_semantic_authority",
    "bootstrap_or_runtime_observation_used_as_authority",
}
NOT_RUN_VALUES = {"not_run", "not_executed", "not_run_not_accepted"}


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def canonical_json(value: Any, *, indent: int | None = None) -> str:
    if indent is None:
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return json.dumps(value, ensure_ascii=False, sort_keys=True, indent=indent) + "\n"


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"input is unavailable or not a regular file: {path}")
    result: list[dict[str, Any]] = []
    with path.open("rb") as stream:
        for line_number, raw in enumerate(stream, 1):
            if not raw.strip():
                continue
            try:
                value = json.loads(raw.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise ValueError(f"{path}:{line_number}: invalid JSONL: {error}") from error
            if not isinstance(value, dict):
                raise ValueError(f"{path}:{line_number}: row must be a JSON object")
            result.append(value)
    return result


def row_index(row: Mapping[str, Any]) -> int:
    value = row.get("inventory_index", row.get("index"))
    if not isinstance(value, int) or isinstance(value, bool) or value < 1:
        raise ValueError(f"invalid inventory index: {value!r}")
    return value


def row_path(row: Mapping[str, Any]) -> str:
    value = row.get("path") or row.get("source_path")
    if not isinstance(value, str) or not value:
        raise ValueError("evidence row has no source path")
    return value


def unique_index_and_path(
    rows: Iterable[dict[str, Any]],
    label: str,
    *,
    expected_count: int | None = None,
) -> dict[int, dict[str, Any]]:
    by_index: dict[int, dict[str, Any]] = {}
    by_path: dict[str, int] = {}
    for row in rows:
        index = row_index(row)
        path = row_path(row)
        if index in by_index:
            raise ValueError(f"duplicate {label} index {index}")
        if path in by_path:
            raise ValueError(
                f"duplicate {label} source path {path!r} at indices "
                f"{by_path[path]} and {index}"
            )
        by_index[index] = row
        by_path[path] = index
    if expected_count is not None:
        expected = set(range(1, expected_count + 1))
        missing = sorted(expected - set(by_index))
        extra = sorted(set(by_index) - expected)
        if missing or extra:
            raise ValueError(
                f"{label} roster mismatch: count={len(by_index)}, "
                f"missing={missing[:12]}, extra={extra[:12]}"
            )
    return by_index


def safe_source_file(repo_root: Path, relative_path: str) -> Path:
    candidate = Path(relative_path)
    if candidate.is_absolute() or ".." in candidate.parts:
        raise ValueError(f"source path is not repository-relative: {relative_path!r}")
    try:
        resolved = (repo_root / candidate).resolve(strict=True)
        resolved.relative_to(repo_root.resolve(strict=True))
    except (OSError, ValueError) as error:
        raise ValueError(f"source path is missing or escapes repository: {relative_path}") from error
    if not resolved.is_file() or (repo_root / candidate).is_symlink():
        raise ValueError(f"source path is not a regular non-symlink file: {relative_path}")
    return resolved


def current_source_sha(repo_root: Path, relative_path: str) -> str:
    return sha256_file(safe_source_file(repo_root, relative_path))


def walk_mappings(value: Any, location: str = "$") -> Iterable[tuple[str, Mapping[str, Any]]]:
    if isinstance(value, Mapping):
        yield location, value
        for key, child in value.items():
            yield from walk_mappings(child, f"{location}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk_mappings(child, f"{location}[{index}]")


def class_value(value: Any) -> str | None:
    if isinstance(value, Mapping):
        nested = value.get("value")
        return nested if isinstance(nested, str) else None
    return value if isinstance(value, str) else None


def add_class_claim(
    claims: list[dict[str, Any]],
    *,
    source: str,
    field: str,
    raw_value: Any,
) -> None:
    value = class_value(raw_value)
    if value is None:
        return
    if value == ROUTE_ONLY_CLASS:
        claims.append(
            {
                "source": source,
                "field": field,
                "value": value,
                "axis": "route_expectation_only",
                "semantic_candidate": False,
                "raw_value_verbatim": raw_value,
            }
        )
        return
    if value not in ALLOWED_SOURCE_CLASSES:
        claims.append(
            {
                "source": source,
                "field": field,
                "value": value,
                "axis": "unrecognized_claim_preserved",
                "semantic_candidate": False,
                "raw_value_verbatim": raw_value,
            }
        )
        return
    claims.append(
        {
            "source": source,
            "field": field,
            "value": value,
            "axis": "source_semantic",
            "semantic_candidate": True,
            "raw_value_verbatim": raw_value,
        }
    )


def r2_class_claims(r2_row: Mapping[str, Any]) -> list[dict[str, Any]]:
    claims: list[dict[str, Any]] = []
    r1 = r2_row.get("r1_orthogonal_ledger_row_verbatim", {})
    if not isinstance(r1, Mapping):
        raise ValueError("r2 row is missing its full r1 ledger object")
    add_class_claim(
        claims,
        source="r1_orthogonal_ledger",
        field="source_semantic_class",
        raw_value=r1.get("source_semantic_class"),
    )
    raw_claims = r1.get("source_semantic_class_claims", [])
    if not isinstance(raw_claims, list):
        raise ValueError("r1 source_semantic_class_claims must be a list")
    for index, claim in enumerate(raw_claims):
        if isinstance(claim, Mapping):
            add_class_claim(
                claims,
                source=str(claim.get("source", "r1_source_semantic_class_claims")),
                field=f"source_semantic_class_claims[{index}].{claim.get('field', 'value')}",
                raw_value=claim.get("value"),
            )
    r1_evidence = r1.get("source_evidence_verbatim", {})
    if isinstance(r1_evidence, Mapping):
        repaired = r1_evidence.get("repaired_independent_shard")
        if isinstance(repaired, Mapping):
            add_class_claim(
                claims,
                source="repaired_independent_shard_full_row",
                field="expected_class",
                raw_value=repaired.get("expected_class"),
            )
        supplement_65 = r1_evidence.get("independent_65_row_adjudication_supplement")
        if isinstance(supplement_65, Mapping):
            for field in (
                "final_source_semantic_class",
                "independent_revised_source_semantic_class",
                "source_semantic_class",
            ):
                if field in supplement_65:
                    add_class_claim(
                        claims,
                        source="independent_65_row_adjudication_supplement",
                        field=field,
                        raw_value=supplement_65[field],
                    )
            ruling = supplement_65.get("independent_ruling")
            if isinstance(ruling, Mapping):
                add_class_claim(
                    claims,
                    source="independent_65_row_adjudication_supplement",
                    field="independent_ruling.source_semantic_class",
                    raw_value=ruling.get("source_semantic_class"),
                )
    raw_r2_semantic = r2_row.get("source_semantic_class", {})
    if isinstance(raw_r2_semantic, Mapping):
        for field in ("value", "ledger_candidate_value", "supplement_candidate_value"):
            add_class_claim(
                claims,
                source="r2_source_semantic_class",
                field=field,
                raw_value=raw_r2_semantic.get(field),
            )
    direct = r2_row.get("direct_contract_supplement_row_verbatim", {})
    if isinstance(direct, Mapping):
        add_class_claim(
            claims,
            source="r2_direct_contract_supplement",
            field="candidate_semantic_class",
            raw_value=direct.get("candidate_semantic_class"),
        )
    return claims


def adjudication_class_claims(row: Mapping[str, Any], index: int) -> list[dict[str, Any]]:
    claims: list[dict[str, Any]] = []
    for field in ADJUDICATION_CLASS_FIELDS:
        if field in row:
            add_class_claim(
                claims,
                source=f"unresolved_independent_adjudication_{index}",
                field=field,
                raw_value=row[field],
            )
    if not claims:
        raise ValueError(
            f"adjudication row {index} has no independent source-semantic class field"
        )
    if any(claim["axis"] == "route_expectation_only" for claim in claims):
        raise ValueError(
            f"adjudication row {index} puts a route-only disposition in a semantic class field"
        )
    return claims


def normative_basis_present(row: Mapping[str, Any]) -> bool:
    basis = row.get("normative_basis")
    if isinstance(basis, str):
        return bool(basis.strip())
    if isinstance(basis, (list, dict)):
        return bool(basis)
    return False


def _is_hash(value: Any) -> bool:
    return isinstance(value, str) and SHA256_RE.fullmatch(value) is not None


def adjudication_hash_claims(
    row: Mapping[str, Any],
) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    """Return current source pins and historical pins as separate evidence."""
    current: list[dict[str, str]] = []
    historical: list[dict[str, str]] = []
    for location, mapping in walk_mappings(row):
        for key, value in mapping.items():
            if not isinstance(value, str) or not _is_hash(value):
                continue
            name = str(key).lower()
            context = f"{location}.{name}".lower()
            if "test" in context or "normative_basis" in context or "file_sha256" in name:
                continue
            looks_source_bound = (
                name.startswith("source_sha256")
                or name in {"expected_sha256", "sha256"}
                and any(token in context for token in ("source_identity", "source_binding", "source_hash_recheck"))
            )
            if not looks_source_bound:
                continue
            claim = {"field_path": f"{location}.{key}", "sha256": value}
            if any(token in name for token in ("first_review", "review_start", "original")):
                historical.append(claim)
            else:
                current.append(claim)
    return current, historical


def r2_expected_hashes(row: Mapping[str, Any]) -> list[dict[str, str]]:
    validation = row.get("source_hash_validation")
    if not isinstance(validation, Mapping):
        raise ValueError(f"r2 row {row_index(row)} lacks source_hash_validation")
    result: list[dict[str, str]] = []
    for field in (
        "live_source_sha256",
        "r1_ledger_source_sha256",
        "direct_supplement_source_sha256",
    ):
        value = validation.get(field)
        if not _is_hash(value):
            raise ValueError(
                f"r2 row {row_index(row)} has missing or invalid {field}; source hash is unreviewed"
            )
        result.append({"field": f"source_hash_validation.{field}", "sha256": value})
    r1 = row.get("r1_orthogonal_ledger_row_verbatim", {})
    if isinstance(r1, Mapping):
        value = r1.get("source_sha256_current")
        if not _is_hash(value):
            raise ValueError(f"r2 row {row_index(row)} has no frozen r1 source SHA")
        result.append({"field": "r1_orthogonal_ledger_row_verbatim.source_sha256_current", "sha256": value})
    direct = row.get("direct_contract_supplement_row_verbatim", {})
    if isinstance(direct, Mapping):
        value = direct.get("source_sha256_current")
        if not _is_hash(value):
            raise ValueError(f"r2 row {row_index(row)} has no frozen direct supplement source SHA")
        result.append(
            {
                "field": "direct_contract_supplement_row_verbatim.source_sha256_current",
                "sha256": value,
            }
        )
    return result


def validate_input_nonexecution(value: Any, label: str, location: str = "$") -> None:
    if isinstance(value, Mapping):
        for key, child in value.items():
            name = str(key).lower()
            child_location = f"{location}.{key}"
            if name in ACCEPTANCE_FIELDS and child is not False:
                raise ValueError(f"{label}{child_location} must be false")
            if name in EXECUTION_FIELDS:
                if isinstance(child, bool) and child is not False:
                    raise ValueError(f"{label}{child_location} reports execution/authority")
                if isinstance(child, str) and name == "execution_status" and child not in NOT_RUN_VALUES:
                    raise ValueError(f"{label}{child_location} is not not_run: {child!r}")
            if name in {
                "runtime_or_test_observation_used_as_semantic_authority",
                "runtime_or_bootstrap_observation_used_as_authority",
            } and child is not False:
                raise ValueError(f"{label}{child_location} must be false")
            validate_input_nonexecution(child, label, child_location)
    elif isinstance(value, list):
        for index, child in enumerate(value):
            validate_input_nonexecution(child, label, f"{location}[{index}]")


def validate_candidate_claims(row: Mapping[str, Any], index: int) -> list[dict[str, Any]]:
    claims = adjudication_class_claims(row, index)
    values = {claim["value"] for claim in claims if claim["axis"] == "source_semantic"}
    if not values:
        raise ValueError(f"adjudication row {index} has no recognized source-semantic class")
    return claims


def validate_adjudication_source_provenance(
    row: Mapping[str, Any],
    repo_root: Path,
) -> None:
    """Fail closed when an adjudication row binds a different Mimi source."""
    expected = row_path(row)
    expected_file = safe_source_file(repo_root, expected)
    for location, mapping in walk_mappings(row):
        for key, value in mapping.items():
            if not isinstance(value, str):
                continue
            name = str(key).lower()
            path = f"{location}.{name}".lower()
            if ".tool." in path or ".test" in path or "normative_basis" in path:
                continue
            source_path_field = name in {"source_path", "source_path_absolute"}
            contract_input_path = name == "path" and any(
                token in path for token in (".input.", ".source_binding.", ".source_identity_check.")
            )
            if not source_path_field and not contract_input_path:
                continue
            claimed = Path(value)
            try:
                if claimed.is_absolute():
                    resolved = claimed.resolve(strict=True)
                else:
                    resolved = (repo_root / claimed).resolve(strict=True)
                resolved.relative_to(repo_root.resolve(strict=True))
            except (OSError, ValueError) as error:
                raise ValueError(
                    f"adjudication row {row_index(row)} has an unsafe source-path claim "
                    f"at {location}.{key}: {value!r}"
                ) from error
            if resolved != expected_file:
                raise ValueError(
                    f"adjudication row {row_index(row)} source-path provenance mismatch "
                    f"at {location}.{key}: {value!r} does not bind {expected!r}"
                )


def load_inputs(
    input_root: Path = DEFAULT_INPUT_ROOT,
    repo_root: Path = REPO_ROOT,
    *,
    expected_count: int = COUNT,
) -> dict[str, Any]:
    r2_path = input_root / R2_RELATIVE_PATH
    r2_rows = read_jsonl(r2_path)
    r2_by_index = unique_index_and_path(
        r2_rows, "r2 ledger", expected_count=expected_count
    )
    unresolved = {
        index
        for index, row in r2_by_index.items()
        if class_value(row.get("source_semantic_class")) == UNRESOLVED
    }
    if not unresolved:
        raise ValueError("r2 ledger has no unresolved source-semantic roster")
    adjudication_rows: list[dict[str, Any]] = []
    shard_files: list[Path] = []
    for filename in ADJUDICATION_FILENAMES:
        path = input_root / filename
        shard = read_jsonl(path)
        shard_files.append(path)
        start_index, end_index = ADJUDICATION_SHARD_RANGES[filename]
        shard_indices: set[int] = set()
        shard_paths: set[str] = set()
        for row in shard:
            index = row_index(row)
            source = row_path(row)
            if not start_index <= index <= end_index:
                raise ValueError(
                    f"{path.name}: index {index} is outside its frozen range "
                    f"{start_index}-{end_index}"
                )
            if index in shard_indices:
                raise ValueError(f"{path.name}: duplicate index {index}")
            if source in shard_paths:
                raise ValueError(f"{path.name}: duplicate source path {source!r}")
            shard_indices.add(index)
            shard_paths.add(source)
            adjudication_rows.append(row)
    adjudication_by_index = unique_index_and_path(adjudication_rows, "adjudication")
    if set(adjudication_by_index) != unresolved:
        missing = sorted(unresolved - set(adjudication_by_index))
        extra = sorted(set(adjudication_by_index) - unresolved)
        raise ValueError(
            "adjudication roster must exactly match the r2 unresolved subset: "
            f"expected={len(unresolved)}, got={len(adjudication_by_index)}, "
            f"missing={missing[:16]}, extra={extra[:16]}"
        )
    for index, row in adjudication_by_index.items():
        r2_row = r2_by_index[index]
        if row_path(row) != row_path(r2_row):
            raise ValueError(
                f"adjudication path mismatch at {index}: "
                f"{row_path(row)!r} != {row_path(r2_row)!r}"
            )
        validate_input_nonexecution(row, f"adjudication[{index}]")
        claims = validate_candidate_claims(row, index)
        current_hashes, _ = adjudication_hash_claims(row)
        if not current_hashes or not any(
            claim["field_path"].endswith(".source_sha256_current")
            or claim["field_path"].endswith(".source_sha256_final_recheck")
            or claim["field_path"].endswith(".source_sha256_rechecked_after_review")
            or claim["field_path"].endswith(".source_sha256_after_review")
            or claim["field_path"].endswith(".source_sha256_review_end")
            for claim in current_hashes
        ):
            raise ValueError(
                f"adjudication row {index} has no current/rechecked source hash; unreviewed hash"
            )
        if any(claim["value"] != UNRESOLVED for claim in claims) and not normative_basis_present(row):
            # The row remains unresolved at merge time, but a concrete candidate
            # without a normative basis is not a reviewable classification input.
            raise ValueError(
                f"adjudication row {index} has a concrete source class without normative_basis"
            )
        validate_adjudication_source_provenance(row, repo_root)
    for row in r2_rows:
        validate_input_nonexecution(row, f"r2[{row_index(row)}]")
        r2_expected_hashes(row)
        safe_source_file(repo_root, row_path(row))
    return {
        "r2_path": r2_path,
        "r2_rows": r2_rows,
        "r2_by_index": r2_by_index,
        "unresolved_indices": unresolved,
        "adjudication_rows": adjudication_rows,
        "adjudication_by_index": adjudication_by_index,
        "shard_files": shard_files,
        "input_root": input_root,
        "repo_root": repo_root,
        "expected_count": expected_count,
    }


def _source_claim_values(claims: Sequence[Mapping[str, Any]]) -> set[str]:
    return {
        str(claim["value"])
        for claim in claims
        if claim.get("axis") == "source_semantic"
        and isinstance(claim.get("value"), str)
        and claim["value"] != UNRESOLVED
    }


def _source_hash_review_flags(row: Mapping[str, Any]) -> list[str]:
    reasons: list[str] = []
    for location, mapping in walk_mappings(row):
        for key, value in mapping.items():
            name = str(key).lower()
            if not isinstance(value, bool):
                continue
            context = f"{location}.{name}".lower()
            if ("drift" in name or "mismatch" in name) and value is True:
                reasons.append(f"{context}=true")
            if ("stable" in name or "match" in name or "rechecked" in name) and value is False:
                if any(token in context for token in ("source_hash", "source_sha", "source_identity")):
                    reasons.append(f"{context}=false")
    return sorted(set(reasons))


def _source_hash_state(
    r2_row: Mapping[str, Any],
    adjudication_row: Mapping[str, Any] | None,
    repo_root: Path,
) -> dict[str, Any]:
    path = row_path(r2_row)
    live_sha = current_source_sha(repo_root, path)
    r2_hashes = r2_expected_hashes(r2_row)
    r2_matches = all(item["sha256"] == live_sha for item in r2_hashes)
    adjud_hashes: list[dict[str, str]] = []
    historical: list[dict[str, str]] = []
    adjud_matches: bool | None = None
    review_flags: list[str] = []
    if adjudication_row is not None:
        adjud_hashes, historical = adjudication_hash_claims(adjudication_row)
        if not adjud_hashes:
            raise ValueError(f"adjudication row {row_index(adjudication_row)} has no current source hash")
        adjud_matches = all(item["sha256"] == live_sha for item in adjud_hashes)
        review_flags = _source_hash_review_flags(adjudication_row)
        for item in historical:
            if item["sha256"] != live_sha:
                review_flags.append(
                    f"{item['field_path']} differs from current live source SHA"
                )
    validation = r2_row.get("source_hash_validation", {})
    r1_prior_unresolved = (
        isinstance(validation, Mapping)
        and validation.get("r1_prior_unresolved_retained") is True
    )
    reasons: list[str] = []
    if not r2_matches:
        reasons.append("live_source_differs_from_r2_frozen_source_hash")
    if adjud_matches is False:
        reasons.append("live_source_differs_from_adjudication_current_hash")
    if review_flags:
        reasons.append("source_hash_drift_or_recheck_flag_in_adjudication")
    if r1_prior_unresolved:
        reasons.append("r1_prior_unresolved_source_hash_provenance_retained")
    return {
        "live_source_sha256": live_sha,
        "r2_source_hash_claims": r2_hashes,
        "r2_hash_claims_match_live": r2_matches,
        "adjudication_current_source_hash_claims": adjud_hashes,
        "adjudication_current_hash_claims_match_live": adjud_matches,
        "adjudication_historical_source_hash_claims": historical,
        "source_hash_review_flags": sorted(set(review_flags)),
        "r1_prior_provenance_status": (
            validation.get("r1_prior_provenance_status")
            if isinstance(validation, Mapping)
            else None
        ),
        "r1_prior_unresolved_retained": r1_prior_unresolved,
        "hash_drift_reasons": sorted(set(reasons)),
        "status": "all_source_hashes_match_current" if not reasons else "source_hash_drift_or_prior_mismatch",
    }


def _raw_route_expectation_claims(row: Mapping[str, Any] | None) -> list[dict[str, Any]]:
    if row is None:
        return []
    result: list[dict[str, Any]] = []
    for key, value in row.items():
        if "route_expectation" in str(key).lower() or str(key).lower() in {
            "default_route",
            "explicit_mir_route",
        }:
            result.append({"field": str(key), "raw_value_verbatim": value})
    return result


def _r2_route_expectation_claims(r2_row: Mapping[str, Any]) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    for field in ("route_metadata_only",):
        if field in r2_row:
            result.append(
                {
                    "source": "r2_route_metadata",
                    "field": field,
                    "raw_value_verbatim": r2_row[field],
                }
            )
    direct = r2_row.get("direct_contract_supplement_row_verbatim")
    if isinstance(direct, Mapping):
        for field in ("route_expectation", "route_expectation_metadata_only"):
            if field in direct:
                result.append(
                    {
                        "source": "r2_direct_contract_supplement",
                        "field": field,
                        "raw_value_verbatim": direct[field],
                    }
                )
    r1 = r2_row.get("r1_orthogonal_ledger_row_verbatim")
    if isinstance(r1, Mapping):
        evidence = r1.get("source_evidence_verbatim")
        if isinstance(evidence, Mapping):
            route16 = evidence.get("shard_004_route_reclassification_supplement")
            if route16 is not None:
                result.append(
                    {
                        "source": "shard_004_route_reclassification_supplement",
                        "field": "full_route_reclassification_row",
                        "raw_value_verbatim": route16,
                    }
                )
    return result


def _status_value(raw: Any, keys: set[str]) -> list[str]:
    result: list[str] = []
    for _, mapping in walk_mappings(raw):
        for key, value in mapping.items():
            if str(key).lower() in keys and isinstance(value, str):
                result.append(value.lower())
    return result


def _contract_lifecycle(raw: Any, *, existing_reference: bool = False) -> str:
    if existing_reference:
        return "existing"
    if isinstance(raw, Mapping) and raw.get("kind") == "source-byte-identity-only":
        return "source_identity_check"
    statuses = _status_value(
        raw,
        {
            "status",
            "contract_status",
            "implementation_status",
            "oracle_status",
        },
    )
    if any("blocked" in value or "unresolved" in value or "undecidable" in value for value in statuses):
        return "blocked"
    if any(value in {"existing", "implemented", "present"} for value in statuses):
        return "existing"
    if any("proposed" in value or "unimplemented" in value for value in statuses):
        return "proposed"
    return "unclassified"


def _contract_pin_assessment(raw: Any, expected_path: str, live_sha: str) -> dict[str, Any]:
    evidence: list[dict[str, str]] = []
    for location, mapping in walk_mappings(raw):
        path = mapping.get("source_path") or mapping.get("path") or mapping.get("exact_path")
        digest = (
            mapping.get("source_sha256")
            or mapping.get("source_sha256_current")
            or mapping.get("expected_sha256")
            or mapping.get("sha256")
        )
        if isinstance(path, str) and isinstance(digest, str) and SHA256_RE.fullmatch(digest):
            if path == expected_path and digest == live_sha:
                evidence.append({"field_path": location, "source_path": path, "sha256": digest})
    return {
        "exact_source_path_and_sha256_pin": bool(evidence),
        "status": "exact_live_source_pin_present" if evidence else "exact_live_source_pin_not_proven",
        "evidence": evidence,
    }


def _normalize_contract_record(
    *,
    source: str,
    source_field: str,
    raw: Any,
    path: str,
    live_sha: str,
    existing_reference: bool = False,
    r2_record_verbatim: Any = None,
) -> dict[str, Any]:
    lifecycle = _contract_lifecycle(raw, existing_reference=existing_reference)
    return {
        "source": source,
        "source_field": source_field,
        "lifecycle_class": lifecycle,
        "execution_status": "not_run",
        "accepted": False,
        "route_acceptance": False,
        "completeness_status": (
            r2_record_verbatim.get("completeness_status", "not_assessed")
            if isinstance(r2_record_verbatim, Mapping)
            else "not_assessed"
        ),
        "pin_assessment": _contract_pin_assessment(raw, path, live_sha),
        "raw_contract_verbatim": raw,
        "r2_normalized_record_verbatim": r2_record_verbatim,
    }


def _direct_contract_records(
    r2_row: Mapping[str, Any],
    adjudication_row: Mapping[str, Any] | None,
    live_sha: str,
) -> dict[str, Any]:
    path = row_path(r2_row)
    records: list[dict[str, Any]] = []
    r2_status = r2_row.get("direct_test_contract_status", {})
    if isinstance(r2_status, Mapping):
        raw_r2_records = r2_status.get("records", [])
        if not isinstance(raw_r2_records, list):
            raise ValueError(f"r2 row {row_index(r2_row)} direct contract records are malformed")
        for ordinal, record in enumerate(raw_r2_records, 1):
            if not isinstance(record, Mapping):
                raise ValueError(f"r2 row {row_index(r2_row)} has non-object contract record")
            if record.get("accepted") is not False or record.get("route_acceptance") is not False:
                raise ValueError(f"r2 row {row_index(r2_row)} direct contract is not unaccepted")
            execution = record.get("execution_status", "not_run")
            if execution not in NOT_RUN_VALUES:
                raise ValueError(f"r2 row {row_index(r2_row)} contract was executed: {execution!r}")
            raw_contract = record.get("raw_contract_verbatim")
            r2_categories = {
                "existing_test_contract": "existing",
                "proposed_or_unimplemented_source_contract": "proposed",
                "blocked_unresolved_source_contract": "blocked",
                "no_new_source_contract_proposed": "no_new_contract_proposed",
            }
            lifecycle = r2_categories.get(
                str(record.get("category")),
                _contract_lifecycle(raw_contract),
            )
            records.append(
                {
                    "source": "r2_direct_contract_ledger",
                    "source_field": str(record.get("source_field", f"records[{ordinal - 1}]")),
                    "lifecycle_class": lifecycle,
                    "execution_status": "not_run",
                    "accepted": False,
                    "route_acceptance": False,
                    "completeness_status": record.get("completeness_status", "not_assessed"),
                    "pin_assessment": record.get(
                        "pin_assessment",
                        _contract_pin_assessment(raw_contract, path, live_sha),
                    ),
                    "raw_contract_verbatim": raw_contract,
                    "r2_normalized_record_verbatim": record,
                }
            )
    if adjudication_row is not None:
        for field in DIRECT_CONTRACT_FIELDS:
            if field not in adjudication_row:
                continue
            raw = adjudication_row[field]
            if field == "source_identity_check":
                source_field = field
                lifecycle = _contract_lifecycle(raw)
            else:
                source_field = field
                lifecycle = _contract_lifecycle(raw)
            records.append(
                _normalize_contract_record(
                    source="unresolved_independent_adjudication",
                    source_field=source_field,
                    raw=raw,
                    path=path,
                    live_sha=live_sha,
                )
            )
        for field in ("related_existing_test_contracts", "existing_test_contracts"):
            raw_existing = adjudication_row.get(field)
            if raw_existing is None:
                continue
            entries = raw_existing if isinstance(raw_existing, list) else [raw_existing]
            for ordinal, raw in enumerate(entries, 1):
                records.append(
                    _normalize_contract_record(
                        source="unresolved_independent_adjudication",
                        source_field=f"{field}[{ordinal - 1}]",
                        raw=raw,
                        path=path,
                        live_sha=live_sha,
                        existing_reference=True,
                    )
                )
    return {
        "status": "append_only_contract_inventory_not_run",
        "execution_status": "not_run",
        "accepted": False,
        "route_acceptance": False,
        "records": records,
        "counts": dict(Counter(record["lifecycle_class"] for record in records)),
    }


def _effective_source_class(
    r2_row: Mapping[str, Any],
    adjudication_row: Mapping[str, Any] | None,
    r2_claims: list[dict[str, Any]],
    adjudication_claims: list[dict[str, Any]],
    hash_state: Mapping[str, Any],
) -> dict[str, Any]:
    raw_r2 = r2_row.get("source_semantic_class")
    r2_value = class_value(raw_r2)
    if r2_value not in ALLOWED_SOURCE_CLASSES:
        raise ValueError(f"r2 row {row_index(r2_row)} has invalid source class: {r2_value!r}")
    candidate_values = {
        str(claim["value"])
        for claim in adjudication_claims
        if claim.get("axis") == "source_semantic"
    }
    concrete_prior = _source_claim_values(r2_claims)
    candidate_disagreement = len(candidate_values) > 1
    prior_disagreement = len(concrete_prior) > 1
    candidate = next(iter(candidate_values)) if len(candidate_values) == 1 else UNRESOLVED
    prior_candidate_conflict = (
        candidate != UNRESOLVED
        and bool(concrete_prior)
        and concrete_prior != {candidate}
    )
    conflicts: list[dict[str, Any]] = []
    if candidate_disagreement:
        conflicts.append(
            {
                "kind": "independent_adjudication_candidate_fields_disagree",
                "values": sorted(candidate_values),
            }
        )
    if prior_disagreement:
        conflicts.append(
            {
                "kind": "prior_source_semantic_claims_disagree",
                "values": sorted(concrete_prior),
            }
        )
    if prior_candidate_conflict:
        conflicts.append(
            {
                "kind": "independent_candidate_conflicts_with_prior_class_claims",
                "candidate": candidate,
                "prior_values": sorted(concrete_prior),
            }
        )
    hash_blocked = bool(hash_state["hash_drift_reasons"])
    basis_present = adjudication_row is not None and normative_basis_present(adjudication_row)
    update_allowed = (
        adjudication_row is not None
        and candidate != UNRESOLVED
        and len(candidate_values) == 1
        and basis_present
        and not conflicts
        and not hash_blocked
    )
    if adjudication_row is None:
        effective = r2_value
        status = "r2_candidate_retained_without_unresolved_adjudication"
        if hash_blocked:
            effective = UNRESOLVED
            status = "source_hash_drift_forces_unresolved"
        elif conflicts:
            effective = UNRESOLVED
            status = "source_class_claim_conflict_forces_unresolved"
    elif update_allowed:
        effective = candidate
        status = "independent_candidate_reconciled_candidate_only"
    else:
        effective = UNRESOLVED
        if hash_blocked:
            status = "source_hash_drift_forces_unresolved"
        elif conflicts:
            status = "source_class_claim_conflict_forces_unresolved"
        elif not basis_present:
            status = "missing_normative_basis_forces_unresolved"
        else:
            status = "independent_candidate_unresolved"
    return {
        "value": effective,
        "status": status,
        "candidate_only": True,
        "accepted": False,
        "r2_value_verbatim": raw_r2,
        "r2_source_semantic_claims_verbatim": r2_claims,
        "independent_candidate_claims_verbatim": adjudication_claims,
        "independent_candidate_values": sorted(candidate_values),
        "prior_concrete_source_semantic_values": sorted(concrete_prior),
        "class_claim_conflicts": conflicts,
        "normative_basis_present": basis_present,
        "independent_candidate_value": candidate,
        "update_gate_passed": update_allowed,
    }


def merge_rows(
    r2_rows: Sequence[dict[str, Any]],
    adjudication_rows: Sequence[dict[str, Any]],
    repo_root: Path = REPO_ROOT,
    *,
    expected_count: int | None = COUNT,
) -> list[dict[str, Any]]:
    r2_by_index = unique_index_and_path(
        r2_rows, "r2 ledger", expected_count=expected_count
    )
    unresolved_indices = {
        index
        for index, row in r2_by_index.items()
        if class_value(row.get("source_semantic_class")) == UNRESOLVED
    }
    adjudication_by_index = unique_index_and_path(adjudication_rows, "adjudication")
    if set(adjudication_by_index) != unresolved_indices:
        missing = sorted(unresolved_indices - set(adjudication_by_index))
        extra = sorted(set(adjudication_by_index) - unresolved_indices)
        raise ValueError(
            "adjudication index roster differs from unresolved r2 roster: "
            f"missing={missing[:16]}, extra={extra[:16]}"
        )
    result: list[dict[str, Any]] = []
    for index in sorted(r2_by_index):
        r2_row = r2_by_index[index]
        path = row_path(r2_row)
        adj_row = adjudication_by_index.get(index)
        if adj_row is not None and row_path(adj_row) != path:
            raise ValueError(
                f"adjudication path mismatch at {index}: {row_path(adj_row)!r} != {path!r}"
            )
        r2_claims = r2_class_claims(r2_row)
        adj_claims = (
            validate_candidate_claims(adj_row, index) if adj_row is not None else []
        )
        hash_state = _source_hash_state(r2_row, adj_row, repo_root)
        sem_class = _effective_source_class(
            r2_row, adj_row, r2_claims, adj_claims, hash_state
        )
        route_claims = _r2_route_expectation_claims(r2_row)
        route_claims.extend(_raw_route_expectation_claims(adj_row))
        r1 = r2_row.get("r1_orthogonal_ledger_row_verbatim", {})
        if isinstance(r1, Mapping):
            raw_claims = r1.get("source_semantic_class_claims", [])
            if isinstance(raw_claims, list):
                for claim in raw_claims:
                    if isinstance(claim, Mapping) and claim.get("value") == ROUTE_ONLY_CLASS:
                        route_claims.append(
                            {
                                "field": "r1.source_semantic_class_claims.route_only",
                                "raw_value_verbatim": claim,
                            }
                        )
        direct_status = _direct_contract_records(
            r2_row, adj_row, hash_state["live_source_sha256"]
        )
        row = {
            "schema": "actor-census-source-bound-schema-v4-adjudication-r3",
            "inventory_index": index,
            "path": path,
            "r2_full_row_verbatim": r2_row,
            "independent_adjudication_row_verbatim": adj_row,
            "source_semantic_class": sem_class,
            "source_semantic_acceptance": False,
            "default_route_disposition": r2_row.get("default_route_disposition"),
            "canonical_mir_only_input_rejection": r2_row.get(
                "canonical_mir_only_input_rejection"
            ),
            "route_expectation_claims_verbatim": route_claims,
            "route_acceptance": False,
            "direct_test_contract_status": direct_status,
            "execution_status": "not_run",
            "accepted": False,
            "accepted_as_route_pass": False,
            "runtime_or_bootstrap_authority": False,
            "source_hash_validation": hash_state,
            "source_hash_review_status": (
                "source_hash_reconciled"
                if not hash_state["hash_drift_reasons"]
                else "unresolved_source_hash_drift_or_prior_mismatch"
            ),
            "blockers": sorted(
                set(
                    (
                        ["source_semantic_class_claim_conflict"]
                        if sem_class["class_claim_conflicts"]
                        else []
                    )
                    + (
                        ["source_hash_drift_or_prior_mismatch"]
                        if hash_state["hash_drift_reasons"]
                        else []
                    )
                    + (
                        ["independent_candidate_normative_basis_missing"]
                        if adj_row is not None and not sem_class["normative_basis_present"]
                        else []
                    )
                )
            ),
        }
        result.append(row)
    validate_rows(
        result,
        expected_count=expected_count if expected_count is not None else None,
    )
    return result


def _assert_false_fields(value: Any, label: str, location: str = "$") -> None:
    if isinstance(value, Mapping):
        for key, child in value.items():
            name = str(key).lower()
            path = f"{location}.{key}"
            if name in ACCEPTANCE_FIELDS and child is not False:
                raise ValueError(f"{label}{path} must be false")
            if name == "execution_status" and child not in NOT_RUN_VALUES:
                raise ValueError(f"{label}{path} must remain not_run")
            if name in EXECUTION_FIELDS and isinstance(child, bool) and child is not False:
                raise ValueError(f"{label}{path} claims execution or runtime authority")
            if name in {
                "runtime_or_test_observation_used_as_semantic_authority",
                "runtime_or_bootstrap_observation_used_as_authority",
            } and child is not False:
                raise ValueError(f"{label}{path} must be false")
            _assert_false_fields(child, label, path)
    elif isinstance(value, list):
        for index, child in enumerate(value):
            _assert_false_fields(child, label, f"{location}[{index}]")


def validate_rows(
    rows: Sequence[dict[str, Any]], *, expected_count: int | None = COUNT
) -> None:
    by_index = unique_index_and_path(
        list(rows), "r3 ledger", expected_count=expected_count
    )
    for index, row in by_index.items():
        if row.get("schema") != "actor-census-source-bound-schema-v4-adjudication-r3":
            raise ValueError(f"r3 row {index} has unexpected schema")
        if row.get("source_semantic_acceptance") is not False:
            raise ValueError(f"r3 row {index} source-semantic acceptance must be false")
        if row.get("route_acceptance") is not False:
            raise ValueError(f"r3 row {index} route acceptance must be false")
        if row.get("execution_status") != "not_run":
            raise ValueError(f"r3 row {index} execution status must be not_run")
        if row.get("accepted") is not False or row.get("accepted_as_route_pass") is not False:
            raise ValueError(f"r3 row {index} cannot be accepted")
        if row.get("runtime_or_bootstrap_authority") is not False:
            raise ValueError(f"r3 row {index} cannot claim runtime/bootstrap authority")
        semantic = row.get("source_semantic_class")
        if not isinstance(semantic, Mapping):
            raise ValueError(f"r3 row {index} lacks source_semantic_class")
        if semantic.get("value") not in ALLOWED_SOURCE_CLASSES:
            raise ValueError(f"r3 row {index} contains route/unknown value in source class")
        if semantic.get("accepted") is not False or semantic.get("candidate_only") is not True:
            raise ValueError(f"r3 row {index} source class must remain candidate-only")
        if semantic.get("class_claim_conflicts") and semantic.get("value") != UNRESOLVED:
            raise ValueError(f"r3 row {index} resolves a source-class conflict")
        hash_state = row.get("source_hash_validation")
        if not isinstance(hash_state, Mapping) or not _is_hash(hash_state.get("live_source_sha256")):
            raise ValueError(f"r3 row {index} has no live source SHA binding")
        if hash_state.get("hash_drift_reasons") and semantic.get("value") != UNRESOLVED:
            raise ValueError(f"r3 row {index} promotes a source class despite hash drift")
        direct = row.get("direct_test_contract_status")
        if not isinstance(direct, Mapping):
            raise ValueError(f"r3 row {index} has no direct contract inventory")
        if direct.get("execution_status") != "not_run" or direct.get("accepted") is not False:
            raise ValueError(f"r3 row {index} direct contract inventory must be not_run/unaccepted")
        if direct.get("route_acceptance") is not False:
            raise ValueError(f"r3 row {index} direct contracts cannot accept a route")
        for record in direct.get("records", []):
            if not isinstance(record, Mapping):
                raise ValueError(f"r3 row {index} has malformed direct contract record")
            if record.get("execution_status") != "not_run" or record.get("accepted") is not False:
                raise ValueError(f"r3 row {index} has executed/accepted direct contract")
            if record.get("route_acceptance") is not False:
                raise ValueError(f"r3 row {index} direct contract route acceptance must be false")
        _assert_false_fields(row, f"r3[{index}]")
        if row_path(row) != row_path(row["r2_full_row_verbatim"]):
            raise ValueError(f"r3 row {index} changed the r2 source path")
        if row["r2_full_row_verbatim"].get("inventory_index") != index:
            raise ValueError(f"r3 row {index} changed the r2 inventory index")
        for claim in semantic.get("independent_candidate_claims_verbatim", []):
            if claim.get("value") == ROUTE_ONLY_CLASS:
                raise ValueError(f"r3 row {index} mixes route rejection into source semantics")
        if row.get("default_route_disposition") != row["r2_full_row_verbatim"].get(
            "default_route_disposition"
        ):
            raise ValueError(f"r3 row {index} rewrote default-route evidence")
        if row.get("canonical_mir_only_input_rejection") != row[
            "r2_full_row_verbatim"
        ].get("canonical_mir_only_input_rejection"):
            raise ValueError(f"r3 row {index} rewrote canonical MIR-only route evidence")


def finalization_blockers(rows: Sequence[dict[str, Any]]) -> list[dict[str, Any]]:
    blockers: list[dict[str, Any]] = []
    for row in rows:
        state = row["source_hash_validation"]
        if state.get("hash_drift_reasons"):
            blockers.append(
                {
                    "inventory_index": row["inventory_index"],
                    "path": row["path"],
                    "reasons": state["hash_drift_reasons"],
                }
            )
    return blockers


def make_summary(rows: Sequence[dict[str, Any]], inputs: Mapping[str, Any]) -> dict[str, Any]:
    source_classes = Counter(row["source_semantic_class"]["value"] for row in rows)
    semantic_status = Counter(row["source_semantic_class"]["status"] for row in rows)
    contract_lifecycle: Counter[str] = Counter()
    contract_sources: Counter[str] = Counter()
    for row in rows:
        for contract in row["direct_test_contract_status"]["records"]:
            contract_lifecycle[contract["lifecycle_class"]] += 1
            contract_sources[contract["source"]] += 1
    return {
        "schema": "actor-census-source-bound-schema-v4-adjudication-r3-summary",
        "package_status": (
            "blocked_source_hash_drift"
            if finalization_blockers(rows)
            else "candidate_inventory_reconciled_not_accepted"
        ),
        "input_roster_status": "complete_r2_1187_plus_nine_adjudication_shards",
        "source_row_count": len(rows),
        "r2_unresolved_row_count": len(inputs["unresolved_indices"]),
        "adjudication_row_count": len(inputs["adjudication_by_index"]),
        "adjudication_shard_count": len(inputs["shard_files"]),
        "source_semantic_class_candidate_counts": dict(sorted(source_classes.items())),
        "source_semantic_candidate_status_counts": dict(sorted(semantic_status.items())),
        "source_class_conflict_rows": sum(
            bool(row["source_semantic_class"]["class_claim_conflicts"]) for row in rows
        ),
        "source_hash_drift_rows": len(finalization_blockers(rows)),
        "r1_prior_unresolved_hash_provenance_rows": sum(
            row["source_hash_validation"]["r1_prior_unresolved_retained"] for row in rows
        ),
        "direct_contract_record_count": sum(
            len(row["direct_test_contract_status"]["records"]) for row in rows
        ),
        "direct_contract_lifecycle_counts": dict(sorted(contract_lifecycle.items())),
        "direct_contract_source_counts": dict(sorted(contract_sources.items())),
        "direct_contract_execution_status": "all not_run",
        "source_semantic_acceptance": False,
        "route_acceptance": False,
        "runtime_or_bootstrap_authority": False,
        "source_taxonomy_reconciliation_only": True,
        "statement": (
            "This ledger reconciles source-semantic candidate claims only. "
            "It records no route acceptance, source-semantic acceptance, or execution."
        ),
    }


def input_manifest(inputs: Mapping[str, Any]) -> dict[str, Any]:
    files = [inputs["r2_path"], *inputs["shard_files"]]
    return {
        "r2_file": {
            "path": str(inputs["r2_path"]),
            "sha256": sha256_file(inputs["r2_path"]),
            "row_count": len(inputs["r2_rows"]),
        },
        "adjudication_shards": [
            {
                "path": str(path),
                "sha256": sha256_file(path),
                "row_count": len(read_jsonl(path)),
            }
            for path in inputs["shard_files"]
        ],
        "source_file_count": len(inputs["r2_rows"]),
        "input_file_count": len(files),
    }


def write_package(
    output_dir: Path,
    rows: Sequence[dict[str, Any]],
    summary: Mapping[str, Any],
    manifest: Mapping[str, Any],
) -> None:
    validate_rows(rows, expected_count=len(rows))
    blockers = finalization_blockers(rows)
    if blockers:
        raise ValueError(
            "r3 finalization is blocked by source-hash drift or unresolved prior provenance: "
            + canonical_json(blockers)
        )
    target = output_dir.resolve()
    repo = REPO_ROOT.resolve()
    if target == repo or repo in target.parents:
        raise ValueError("evidence package output must remain outside the git repository")
    if target.exists():
        raise FileExistsError(f"refusing to overwrite an existing evidence package: {target}")
    target.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f".{target.name}.staging-", dir=target.parent) as staging:
        stage = Path(staging)
        ledger = stage / "source-bound-schema-v4-adjudicated-ledger-1187.jsonl"
        with ledger.open("w", encoding="utf-8", newline="\n") as stream:
            for row in rows:
                stream.write(canonical_json(row) + "\n")
        summary_path = stage / "SUMMARY.json"
        summary_path.write_text(canonical_json(summary, indent=2), encoding="utf-8")
        manifest_path = stage / "INPUTS.json"
        manifest_path.write_text(canonical_json(manifest, indent=2), encoding="utf-8")
        sums = {
            ledger.name: sha256_file(ledger),
            summary_path.name: sha256_file(summary_path),
            manifest_path.name: sha256_file(manifest_path),
        }
        (stage / "SHA256SUMS.json").write_text(
            canonical_json(sums, indent=2), encoding="utf-8"
        )
        os.replace(stage, target)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-root", type=Path, default=DEFAULT_INPUT_ROOT)
    parser.add_argument("--repo-root", type=Path, default=REPO_ROOT)
    parser.add_argument(
        "--output-dir",
        type=Path,
        required=True,
        help="new external evidence directory; existing directories are never overwritten",
    )
    args = parser.parse_args(argv)
    inputs = load_inputs(args.input_root, args.repo_root)
    rows = merge_rows(
        inputs["r2_rows"],
        inputs["adjudication_rows"],
        args.repo_root,
        expected_count=inputs["expected_count"],
    )
    summary = make_summary(rows, inputs)
    write_package(args.output_dir, rows, summary, input_manifest(inputs))
    print(canonical_json({"output_dir": str(args.output_dir.resolve()), "summary": summary}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
