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
import platform
import re
import sys
import tempfile
from collections import Counter
from pathlib import Path
from pathlib import PurePosixPath
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
CLASS_METADATA_SENTINELS = {"not_separately_adjudicated"}
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
    "acceptance",
    "accepted",
    "accepted_as_route_pass",
    "route_acceptance",
    "semantic_acceptance",
    "source_semantic_acceptance",
    "source_semantic_contract_acceptance",
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
NONEXECUTION_STATUS_FIELDS = {
    "used_as_semantic_authority",
    "runtime_or_test_observation_used_as_semantic_authority",
    "runtime_or_bootstrap_observation_used_as_authority",
    "runtime_or_bootstrap_observation_used_as_semantic_authority",
    "bootstrap_or_runtime_observation_used_as_semantic_authority",
    "runtime_or_bootstrap_authority",
}
SOURCE_HASH_STATUS_FIELDS = {
    "source_sha256_matches_frozen_ledger",
    "source_sha256_matches_frozen_locator_records",
}
SOURCE_BINDING_CONTEXTS = (
    "direct_contract",
    "direct_check_contract",
    "proposed_direct_contract",
    "source_binding",
    "source_identity",
    "source_identity_check",
    "source_hash_recheck",
)
NON_PRIMARY_SOURCE_CONTEXTS = (
    "non_normative_source_context",
    "normative_basis",
    "diagnostic_catalog_context_not_decisive",
    "docs_spec_scope_check",
    "test",
    "tool",
    "existing_test",
)


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
    repo_root: Path | None = None,
) -> dict[int, dict[str, Any]]:
    by_index: dict[int, dict[str, Any]] = {}
    by_path: dict[str, int] = {}
    by_inode: dict[tuple[int, int], tuple[int, str]] = {}
    for row in rows:
        index = row_index(row)
        path = row_path(row)
        candidate = PurePosixPath(path)
        if candidate.is_absolute() or ".." in candidate.parts:
            raise ValueError(f"unsafe {label} source path {path!r}")
        canonical_path = candidate.as_posix()
        if canonical_path in {"", "."}:
            raise ValueError(f"empty {label} source path {path!r}")
        if index in by_index:
            raise ValueError(f"duplicate {label} index {index}")
        if canonical_path in by_path:
            raise ValueError(
                f"duplicate {label} source path {path!r} at indices "
                f"{by_path[canonical_path]} and {index}"
            )
        if repo_root is not None:
            source_file = safe_source_file(repo_root, path)
            stat = source_file.stat()
            inode = (stat.st_dev, stat.st_ino)
            if inode in by_inode:
                prior_index, prior_path = by_inode[inode]
                raise ValueError(
                    f"duplicate {label} physical source file {path!r} and "
                    f"{prior_path!r} at indices {prior_index} and {index}"
                )
            by_inode[inode] = (index, path)
        by_index[index] = row
        by_path[canonical_path] = index
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
        if raw_value is not None:
            raise ValueError(
                f"invalid class claim at {source}.{field}: {raw_value!r}"
            )
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
    if value in CLASS_METADATA_SENTINELS:
        claims.append(
            {
                "source": source,
                "field": field,
                "value": value,
                "axis": "class_metadata_only",
                "semantic_candidate": False,
                "raw_value_verbatim": raw_value,
            }
        )
        return
    if value not in ALLOWED_SOURCE_CLASSES:
        raise ValueError(
            f"unknown source-semantic class claim at {source}.{field}: {value!r}"
        )
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
            name = str(key).lower()
            context = f"{location}.{name}".lower()
            if name in SOURCE_HASH_STATUS_FIELDS:
                continue
            if "test" in context or "normative_basis" in context or "file_sha256" in name:
                continue
            looks_source_bound = _is_authoritative_source_hash_field(name, context)
            if not looks_source_bound:
                continue
            if not _is_hash(value):
                raise ValueError(
                    f"adjudication row {row_index(row)} has malformed authoritative "
                    f"source hash at {location}.{key}: {value!r}"
                )
            claim = {"field_path": f"{location}.{key}", "sha256": value}
            if any(token in name for token in ("first_review", "review_start", "original")):
                historical.append(claim)
            else:
                current.append(claim)
    return current, historical


def _is_authoritative_source_hash_field(name: str, context: str) -> bool:
    if name.startswith("source_sha256"):
        return not any(token in name for token in ("matches_", "_matches", "_bound", "_verified"))
    if name in {"expected_sha256", "sha256", "sha256_current"}:
        return any(token in context for token in SOURCE_BINDING_CONTEXTS)
    return False


def validate_source_binding_schema(row: Mapping[str, Any]) -> None:
    """Reject unrecognized path/hash pairs in source authority structures."""
    known_path_fields = {"source_path", "source_path_absolute", "path", "exact_path"}
    known_hash_fields = {
        "source_sha256",
        "expected_sha256",
        "sha256",
        "sha256_current",
        *SOURCE_HASH_STATUS_FIELDS,
    }
    for location, mapping in walk_mappings(row):
        for key in SOURCE_HASH_STATUS_FIELDS:
            if key in mapping and not isinstance(mapping[key], bool):
                raise ValueError(
                    f"adjudication row {row_index(row)} has non-boolean hash status "
                    f"at {location}.{key}"
                )
        context = location.lower()
        if any(token in context for token in NON_PRIMARY_SOURCE_CONTEXTS):
            continue
        is_contextual_reference = "referenced_current_source_context" in context
        in_binding_context = any(token in context for token in SOURCE_BINDING_CONTEXTS)
        path_fields = [
            str(key).lower()
            for key, value in mapping.items()
            if isinstance(value, str)
            and not str(key).lower().startswith("test_")
            and ("path" in str(key).lower() or str(key).lower() in {"file", "source"})
        ]
        hash_fields = [
            str(key).lower()
            for key, value in mapping.items()
            if not str(key).lower().startswith("test_")
            and (
                "sha256" in str(key).lower()
                or str(key).lower() in {"sha256", "expected_sha256", "hash", "digest"}
            )
        ]
        has_source_path = any(field in {"source_path", "source_path_absolute"} for field in path_fields)
        if path_fields and hash_fields and not (
            in_binding_context or has_source_path or is_contextual_reference
        ):
            raise ValueError(
                f"adjudication row {row_index(row)} has an unbound path/hash pair at "
                f"{location}: paths={path_fields}, hashes={hash_fields}"
            )
        if not (in_binding_context or has_source_path or is_contextual_reference):
            continue
        if not path_fields and not hash_fields:
            continue
        unknown_paths = sorted(set(path_fields) - known_path_fields - {"file", "source"})
        unknown_hashes = sorted(
            field
            for field in set(hash_fields)
            if field not in known_hash_fields
            and not (
                field.startswith("source_sha256")
                and not any(
                    token in field
                    for token in ("matches_", "_matches", "_bound", "_verified")
                )
            )
        )
        if unknown_paths or unknown_hashes:
            raise ValueError(
                f"adjudication row {row_index(row)} has unknown source path/hash "
                f"schema at {location}: paths={unknown_paths}, hashes={unknown_hashes}"
            )
        if is_contextual_reference:
            digest = mapping.get("sha256_current")
            if not isinstance(mapping.get("path"), str) or not _is_hash(digest):
                raise ValueError(
                    f"adjudication row {row_index(row)} has malformed contextual "
                    f"source reference at {location}"
                )


def r2_expected_hashes(row: Mapping[str, Any]) -> list[dict[str, str]]:
    validation = row.get("source_hash_validation")
    if not isinstance(validation, Mapping):
        raise ValueError(f"r2 row {row_index(row)} lacks source_hash_validation")
    for field in ("live_matches_r1_ledger", "live_matches_direct_supplement"):
        if field in validation and not isinstance(validation[field], bool):
            raise ValueError(f"r2 row {row_index(row)} has non-boolean {field}")
    if "r1_prior_unresolved_retained" in validation and not isinstance(
        validation["r1_prior_unresolved_retained"], bool
    ):
        raise ValueError(
            f"r2 row {row_index(row)} has non-boolean r1_prior_unresolved_retained"
        )
    provenance_status = validation.get("r1_prior_provenance_status")
    if provenance_status not in {"all_current_bindings_match", "mismatch_unresolved"}:
        raise ValueError(
            f"r2 row {row_index(row)} has unknown r1 hash provenance status: "
            f"{provenance_status!r}"
        )
    mismatch_reasons = validation.get("mismatch_reasons", [])
    if not isinstance(mismatch_reasons, list) or any(
        not isinstance(reason, str) for reason in mismatch_reasons
    ):
        raise ValueError(f"r2 row {row_index(row)} has malformed hash mismatch reasons")
    known_mismatch_reasons = {
        "r1_prior_source_hash_provenance_unresolved_retained",
        "source_hash_recheck_flag_reports_mismatch_or_drift",
        "embedded_source_hash_claim_differs_from_live_source",
        "supplement_candidate_class_differs_from_orthogonal_ledger",
    }
    unknown_reasons = sorted(set(mismatch_reasons) - known_mismatch_reasons)
    if unknown_reasons:
        raise ValueError(
            f"r2 row {row_index(row)} has unknown prior mismatch reasons: {unknown_reasons}"
        )
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


def r2_hash_provenance_blockers(row: Mapping[str, Any]) -> list[str]:
    validation = row.get("source_hash_validation", {})
    if not isinstance(validation, Mapping):
        return ["missing_r2_source_hash_validation"]
    reasons = list(validation.get("mismatch_reasons", []))
    blockers = [
        f"r2_prior_hash_provenance:{reason}"
        for reason in reasons
        if reason != "supplement_candidate_class_differs_from_orthogonal_ledger"
    ]
    if validation.get("r1_prior_provenance_status") == "mismatch_unresolved":
        blockers.append("r1_prior_hash_provenance_status_mismatch_unresolved")
    return sorted(set(blockers))


def _validate_control_field(name: str, child: Any, label: str, location: str) -> None:
    if name in ACCEPTANCE_FIELDS:
        if not isinstance(child, bool) or child is not False:
            raise ValueError(f"{label}{location} must be the boolean false")
        return
    if name == "execution_status":
        if not isinstance(child, str) or child not in NOT_RUN_VALUES:
            raise ValueError(f"{label}{location} must remain not_run")
        return
    if name == "not_executed":
        if not isinstance(child, bool) or child is not True:
            raise ValueError(f"{label}{location} must be boolean true")
        return
    if name == "semantic_authority":
        if isinstance(child, bool):
            if child is not False:
                raise ValueError(f"{label}{location} must be boolean false")
            return
        if isinstance(child, str) and (
            "direct_test_contracts" in location or "raw_contract_verbatim" in location
        ):
            return
        raise ValueError(f"{label}{location} has an invalid semantic_authority shape")
    execution_flag = (
        name in EXECUTION_FIELDS or name.endswith("_executed") or name == "executed"
    )
    authority_flag = name in NONEXECUTION_STATUS_FIELDS or (
        ("runtime" in name or "bootstrap" in name)
        and ("authority" in name or name.endswith("_used"))
    )
    if execution_flag or authority_flag:
        if not isinstance(child, bool) or child is not False:
            raise ValueError(f"{label}{location} must be boolean false; execution/authority is forbidden")


def validate_input_nonexecution(value: Any, label: str, location: str = "$") -> None:
    if isinstance(value, Mapping):
        for key, child in value.items():
            name = str(key).lower()
            child_location = f"{location}.{key}"
            _validate_control_field(name, child, label, child_location)
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
    validate_source_binding_schema(row)
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
                token in path
                for token in (
                    ".input.",
                    ".source_binding.",
                    ".source_identity_check.",
                    ".source_hash_recheck.",
                )
            )
            contextual_reference = "referenced_current_source_context" in path
            if not source_path_field and not contract_input_path:
                if not contextual_reference or name != "path":
                    continue
            if contextual_reference and name == "path":
                claimed = Path(value)
                try:
                    resolved = (
                        claimed.resolve(strict=True)
                        if claimed.is_absolute()
                        else (repo_root / claimed).resolve(strict=True)
                    )
                    resolved.relative_to(repo_root.resolve(strict=True))
                except (OSError, ValueError) as error:
                    raise ValueError(
                        f"adjudication row {row_index(row)} has an unsafe contextual "
                        f"source-path claim at {location}.{key}: {value!r}"
                    ) from error
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
        r2_rows, "r2 ledger", expected_count=expected_count, repo_root=repo_root
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
    adjudication_by_index = unique_index_and_path(
        adjudication_rows, "adjudication", repo_root=repo_root
    )
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
        _source_hash_review_flags(row)
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
        _source_hash_review_flags(row)
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
            context = f"{location}.{name}".lower()
            source_hash_context = any(
                token in context for token in ("source_hash", "source_sha", "source_identity")
            )
            is_hash_review_flag = (
                name in SOURCE_HASH_STATUS_FIELDS
                or any(token in name for token in ("drift", "mismatch", "stable", "match"))
                and source_hash_context
                and not name.endswith(("_reason", "_reasons", "_status", "_details", "_description"))
            )
            if is_hash_review_flag and not isinstance(value, bool):
                raise ValueError(
                    f"adjudication row {row_index(row)} has non-boolean source hash "
                    f"review flag at {context}"
                )
            if not isinstance(value, bool):
                continue
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
    review_flags: list[str] = _source_hash_review_flags(r2_row)
    if adjudication_row is not None:
        adjud_hashes, historical = adjudication_hash_claims(adjudication_row)
        if not adjud_hashes:
            raise ValueError(f"adjudication row {row_index(adjudication_row)} has no current source hash")
        adjud_matches = all(item["sha256"] == live_sha for item in adjud_hashes)
        review_flags.extend(_source_hash_review_flags(adjudication_row))
        for item in historical:
            if item["sha256"] != live_sha:
                review_flags.append(
                    f"{item['field_path']} differs from current live source SHA"
                )
    validation = r2_row.get("source_hash_validation", {})
    r1_prior_unresolved = (
        isinstance(validation, Mapping)
        and (
            validation.get("r1_prior_unresolved_retained") is True
            or validation.get("r1_prior_provenance_status") == "mismatch_unresolved"
        )
    )
    live_hash_mismatch_reasons: list[str] = []
    if not r2_matches:
        live_hash_mismatch_reasons.append("live_source_differs_from_r2_frozen_source_hash")
    if adjud_matches is False:
        live_hash_mismatch_reasons.append(
            "live_source_differs_from_adjudication_current_source_hash"
        )
    provenance_blockers: list[str] = []
    provenance_blockers.extend(r2_hash_provenance_blockers(r2_row))
    if review_flags:
        provenance_blockers.append("source_hash_review_flag_or_historical_pin_mismatch")
    if r1_prior_unresolved:
        provenance_blockers.append("r1_prior_unresolved_source_hash_provenance_retained")
    blocked = bool(live_hash_mismatch_reasons or provenance_blockers)
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
        "live_source_hash_mismatch_reasons": sorted(set(live_hash_mismatch_reasons)),
        "source_hash_provenance_blockers": sorted(set(provenance_blockers)),
        "status": "all_source_hashes_and_provenance_match"
        if not blocked
        else "source_hash_or_provenance_blocked",
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
    live_hash_blocked = bool(hash_state["live_source_hash_mismatch_reasons"])
    provenance_blocked = bool(hash_state["source_hash_provenance_blockers"])
    hash_blocked = live_hash_blocked or provenance_blocked
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
        if live_hash_blocked:
            effective = UNRESOLVED
            status = "live_source_hash_mismatch_forces_unresolved"
        elif provenance_blocked:
            effective = UNRESOLVED
            status = "source_hash_provenance_blocker_forces_unresolved"
        elif conflicts:
            effective = UNRESOLVED
            status = "source_class_claim_conflict_forces_unresolved"
    elif update_allowed:
        effective = candidate
        status = "independent_candidate_reconciled_candidate_only"
    else:
        effective = UNRESOLVED
        if live_hash_blocked:
            status = "live_source_hash_mismatch_forces_unresolved"
        elif provenance_blocked:
            status = "source_hash_provenance_blocker_forces_unresolved"
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
        r2_rows, "r2 ledger", expected_count=expected_count, repo_root=repo_root
    )
    unresolved_indices = {
        index
        for index, row in r2_by_index.items()
        if class_value(row.get("source_semantic_class")) == UNRESOLVED
    }
    adjudication_by_index = unique_index_and_path(
        adjudication_rows, "adjudication", repo_root=repo_root
    )
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
                "source_hash_and_provenance_reconciled"
                if not (
                    hash_state["live_source_hash_mismatch_reasons"]
                    or hash_state["source_hash_provenance_blockers"]
                )
                else "unresolved_source_hash_or_provenance_blocker"
            ),
            "blockers": sorted(
                set(
                    (
                        ["source_semantic_class_claim_conflict"]
                        if sem_class["class_claim_conflicts"]
                        else []
                    )
                    + (
                        ["live_source_hash_mismatch"]
                        if hash_state["live_source_hash_mismatch_reasons"]
                        else []
                    )
                    + (
                        ["source_hash_provenance_blocker"]
                        if hash_state["source_hash_provenance_blockers"]
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
        repo_root=repo_root,
    )
    return result


def _assert_false_fields(value: Any, label: str, location: str = "$") -> None:
    if isinstance(value, Mapping):
        for key, child in value.items():
            name = str(key).lower()
            path = f"{location}.{key}"
            _validate_control_field(name, child, label, path)
            _assert_false_fields(child, label, path)
    elif isinstance(value, list):
        for index, child in enumerate(value):
            _assert_false_fields(child, label, f"{location}[{index}]")


def _expected_class_conflicts(
    candidate_values: set[str], prior_values: set[str]
) -> list[dict[str, Any]]:
    candidate = next(iter(candidate_values)) if len(candidate_values) == 1 else UNRESOLVED
    conflicts: list[dict[str, Any]] = []
    if len(candidate_values) > 1:
        conflicts.append(
            {
                "kind": "independent_adjudication_candidate_fields_disagree",
                "values": sorted(candidate_values),
            }
        )
    if len(prior_values) > 1:
        conflicts.append(
            {
                "kind": "prior_source_semantic_claims_disagree",
                "values": sorted(prior_values),
            }
        )
    if candidate != UNRESOLVED and prior_values and prior_values != {candidate}:
        conflicts.append(
            {
                "kind": "independent_candidate_conflicts_with_prior_class_claims",
                "candidate": candidate,
                "prior_values": sorted(prior_values),
            }
        )
    return conflicts


def _validate_semantic_result(row: Mapping[str, Any], index: int) -> None:
    semantic = row["source_semantic_class"]
    candidate_claims = semantic.get("independent_candidate_claims_verbatim")
    prior_claims = semantic.get("r2_source_semantic_claims_verbatim")
    if not isinstance(candidate_claims, list) or not isinstance(prior_claims, list):
        raise ValueError(f"r3 row {index} lacks class-claim evidence lists")
    candidate_values: set[str] = set()
    for claim in candidate_claims:
        if not isinstance(claim, Mapping):
            raise ValueError(f"r3 row {index} has malformed independent class claim")
        value = claim.get("value")
        axis = claim.get("axis")
        if axis == "route_expectation_only":
            raise ValueError(f"r3 row {index} mixes route expectation into source class")
        if axis == "source_semantic":
            if value not in ALLOWED_SOURCE_CLASSES:
                raise ValueError(f"r3 row {index} has unknown independent source class")
            candidate_values.add(value)
        elif axis not in {"class_metadata_only"}:
            raise ValueError(f"r3 row {index} has unknown class-claim axis {axis!r}")
    prior_values: set[str] = set()
    for claim in prior_claims:
        if not isinstance(claim, Mapping):
            raise ValueError(f"r3 row {index} has malformed prior class claim")
        axis = claim.get("axis")
        value = claim.get("value")
        if axis == "source_semantic":
            if value not in ALLOWED_SOURCE_CLASSES:
                raise ValueError(f"r3 row {index} has unknown prior source class")
            if value != UNRESOLVED:
                prior_values.add(value)
        elif axis not in {"route_expectation_only", "class_metadata_only"}:
            raise ValueError(f"r3 row {index} has unknown prior class-claim axis {axis!r}")
    sorted_candidates = sorted(candidate_values)
    sorted_prior = sorted(prior_values)
    candidate = next(iter(candidate_values)) if len(candidate_values) == 1 else UNRESOLVED
    expected_conflicts = _expected_class_conflicts(candidate_values, prior_values)
    if semantic.get("independent_candidate_values") != sorted_candidates:
        raise ValueError(f"r3 row {index} candidate category does not match its claims")
    if semantic.get("independent_candidate_value") != candidate:
        raise ValueError(f"r3 row {index} candidate value does not match its claims")
    if semantic.get("prior_concrete_source_semantic_values") != sorted_prior:
        raise ValueError(f"r3 row {index} prior classes do not match their claims")
    if semantic.get("class_claim_conflicts") != expected_conflicts:
        raise ValueError(f"r3 row {index} conflict result does not match class claims")
    adjudication = row.get("independent_adjudication_row_verbatim")
    basis_present = isinstance(adjudication, Mapping) and normative_basis_present(adjudication)
    if semantic.get("normative_basis_present") is not basis_present:
        raise ValueError(f"r3 row {index} normative-basis status does not match source evidence")
    hash_state = row["source_hash_validation"]
    live_hash_blocked = bool(hash_state.get("live_source_hash_mismatch_reasons"))
    provenance_blocked = bool(hash_state.get("source_hash_provenance_blockers"))
    hash_blocked = live_hash_blocked or provenance_blocked
    update_allowed = bool(
        isinstance(adjudication, Mapping)
        and candidate != UNRESOLVED
        and len(candidate_values) == 1
        and basis_present
        and not expected_conflicts
        and not hash_blocked
    )
    if semantic.get("update_gate_passed") is not update_allowed:
        raise ValueError(f"r3 row {index} update gate does not match candidate/blockers")
    r2_row = row["r2_full_row_verbatim"]
    raw_r2_value = class_value(r2_row.get("source_semantic_class"))
    if update_allowed:
        expected_value = candidate
        expected_status = "independent_candidate_reconciled_candidate_only"
    elif adjudication is None:
        expected_value = raw_r2_value
        expected_status = "r2_candidate_retained_without_unresolved_adjudication"
        if live_hash_blocked:
            expected_value = UNRESOLVED
            expected_status = "live_source_hash_mismatch_forces_unresolved"
        elif provenance_blocked:
            expected_value = UNRESOLVED
            expected_status = "source_hash_provenance_blocker_forces_unresolved"
        elif expected_conflicts:
            expected_value = UNRESOLVED
            expected_status = "source_class_claim_conflict_forces_unresolved"
    else:
        expected_value = UNRESOLVED
        if live_hash_blocked:
            expected_status = "live_source_hash_mismatch_forces_unresolved"
        elif provenance_blocked:
            expected_status = "source_hash_provenance_blocker_forces_unresolved"
        elif expected_conflicts:
            expected_status = "source_class_claim_conflict_forces_unresolved"
        elif not basis_present:
            expected_status = "missing_normative_basis_forces_unresolved"
        else:
            expected_status = "independent_candidate_unresolved"
    if semantic.get("value") != expected_value or semantic.get("status") != expected_status:
        raise ValueError(
            f"r3 row {index} output class/status disagrees with candidate/conflict results"
        )
    expected_blockers = []
    if expected_conflicts:
        expected_blockers.append("source_semantic_class_claim_conflict")
    if live_hash_blocked:
        expected_blockers.append("live_source_hash_mismatch")
    if provenance_blocked:
        expected_blockers.append("source_hash_provenance_blocker")
    if isinstance(adjudication, Mapping) and not basis_present:
        expected_blockers.append("independent_candidate_normative_basis_missing")
    if row.get("blockers") != sorted(expected_blockers):
        raise ValueError(f"r3 row {index} blocker categories do not match observed evidence")
    expected_hash_review = (
        "source_hash_and_provenance_reconciled"
        if not hash_blocked
        else "unresolved_source_hash_or_provenance_blocker"
    )
    if row.get("source_hash_review_status") != expected_hash_review:
        raise ValueError(f"r3 row {index} hash-review category does not match hash evidence")


def _validate_hash_state(row: Mapping[str, Any], index: int) -> None:
    state = row.get("source_hash_validation")
    if not isinstance(state, Mapping) or not _is_hash(state.get("live_source_sha256")):
        raise ValueError(f"r3 row {index} has no live source SHA binding")
    r2_row = row.get("r2_full_row_verbatim")
    if not isinstance(r2_row, Mapping):
        raise ValueError(f"r3 row {index} lacks the original r2 row")
    expected_r2_hashes = r2_expected_hashes(r2_row)
    if state.get("r2_source_hash_claims") != expected_r2_hashes:
        raise ValueError(f"r3 row {index} altered the r2 source hash claims")
    live_sha = state["live_source_sha256"]
    r2_matches = all(claim["sha256"] == live_sha for claim in expected_r2_hashes)
    if state.get("r2_hash_claims_match_live") is not r2_matches:
        raise ValueError(f"r3 row {index} misreports r2/live hash agreement")
    adjudication = row.get("independent_adjudication_row_verbatim")
    review_flags = _source_hash_review_flags(r2_row)
    adjudication_hashes: list[dict[str, str]] = []
    historical: list[dict[str, str]] = []
    adjudication_matches: bool | None = None
    if adjudication is not None:
        if not isinstance(adjudication, Mapping):
            raise ValueError(f"r3 row {index} has malformed adjudication evidence")
        adjudication_hashes, historical = adjudication_hash_claims(adjudication)
        adjudication_matches = all(claim["sha256"] == live_sha for claim in adjudication_hashes)
        review_flags.extend(_source_hash_review_flags(adjudication))
        for claim in historical:
            if claim["sha256"] != live_sha:
                review_flags.append(f"{claim['field_path']} differs from current live source SHA")
    if state.get("adjudication_current_source_hash_claims") != adjudication_hashes:
        raise ValueError(f"r3 row {index} altered the adjudication current hash claims")
    if state.get("adjudication_current_hash_claims_match_live") is not adjudication_matches:
        raise ValueError(f"r3 row {index} misreports adjudication/live hash agreement")
    if state.get("adjudication_historical_source_hash_claims") != historical:
        raise ValueError(f"r3 row {index} altered the historical hash evidence")
    review_flags = sorted(set(review_flags))
    if state.get("source_hash_review_flags") != review_flags:
        raise ValueError(f"r3 row {index} altered source hash review flags")
    validation = r2_row.get("source_hash_validation", {})
    prior_unresolved = (
        isinstance(validation, Mapping)
        and (
            validation.get("r1_prior_unresolved_retained") is True
            or validation.get("r1_prior_provenance_status") == "mismatch_unresolved"
        )
    )
    if state.get("r1_prior_unresolved_retained") is not prior_unresolved:
        raise ValueError(f"r3 row {index} misreports prior provenance retention")
    live_reasons = []
    if not r2_matches:
        live_reasons.append("live_source_differs_from_r2_frozen_source_hash")
    if adjudication_matches is False:
        live_reasons.append("live_source_differs_from_adjudication_current_source_hash")
    provenance_reasons = r2_hash_provenance_blockers(r2_row)
    if review_flags:
        provenance_reasons.append("source_hash_review_flag_or_historical_pin_mismatch")
    if prior_unresolved:
        provenance_reasons.append("r1_prior_unresolved_source_hash_provenance_retained")
    live_reasons = sorted(set(live_reasons))
    provenance_reasons = sorted(set(provenance_reasons))
    if state.get("live_source_hash_mismatch_reasons") != live_reasons:
        raise ValueError(f"r3 row {index} live hash blocker classification is inconsistent")
    if state.get("source_hash_provenance_blockers") != provenance_reasons:
        raise ValueError(f"r3 row {index} provenance blocker classification is inconsistent")
    blocked = bool(live_reasons or provenance_reasons)
    expected_status = (
        "source_hash_or_provenance_blocked"
        if blocked
        else "all_source_hashes_and_provenance_match"
    )
    if state.get("status") != expected_status:
        raise ValueError(f"r3 row {index} hash validation status is inconsistent")


def validate_rows(
    rows: Sequence[dict[str, Any]],
    *,
    expected_count: int | None = COUNT,
    repo_root: Path | None = REPO_ROOT,
) -> None:
    by_index = unique_index_and_path(
        list(rows), "r3 ledger", expected_count=expected_count, repo_root=repo_root
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
        _validate_semantic_result(row, index)
        hash_state = row.get("source_hash_validation")
        _validate_hash_state(row, index)
        if any(
            hash_state.get(key)
            for key in (
                "live_source_hash_mismatch_reasons",
                "source_hash_provenance_blockers",
            )
        ) and semantic.get("value") != UNRESOLVED:
            raise ValueError(f"r3 row {index} promotes a source class despite hash/provenance blockers")
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
        live_reasons = state.get("live_source_hash_mismatch_reasons", [])
        provenance_reasons = state.get("source_hash_provenance_blockers", [])
        if live_reasons or provenance_reasons:
            blockers.append(
                {
                    "inventory_index": row["inventory_index"],
                    "path": row["path"],
                    "live_source_hash_mismatch_reasons": live_reasons,
                    "source_hash_provenance_blockers": provenance_reasons,
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
        "package_status": "blocked_source_hash_or_provenance"
        if finalization_blockers(rows)
        else "candidate_inventory_reconciled_not_accepted",
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
        "live_source_hash_mismatch_rows": sum(
            bool(row["source_hash_validation"]["live_source_hash_mismatch_reasons"])
            for row in rows
        ),
        "source_hash_provenance_blocker_rows": sum(
            bool(row["source_hash_validation"]["source_hash_provenance_blockers"])
            for row in rows
        ),
        "finalization_blocker_rows": len(finalization_blockers(rows)),
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
    merger_path = Path(__file__).resolve()
    return {
        "merger": {"path": str(merger_path), "sha256": sha256_file(merger_path)},
        "runtime": {
            "python_implementation": platform.python_implementation(),
            "python_version": platform.python_version(),
            "python_full_version": sys.version,
            "python_cache_tag": sys.implementation.cache_tag,
            "platform": platform.platform(),
        },
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


def recheck_live_source_hashes(rows: Sequence[dict[str, Any]], repo_root: Path) -> None:
    """Re-read all bound source files and reject stale in-memory evidence."""
    for row in rows:
        index = row_index(row)
        path = row_path(row)
        actual = current_source_sha(repo_root, path)
        state = row.get("source_hash_validation")
        if not isinstance(state, Mapping):
            raise ValueError(f"r3 row {index} has no source hash state to recheck")
        if state.get("live_source_sha256") != actual:
            raise ValueError(
                f"live source SHA changed before package write for {path}: "
                f"ledger={state.get('live_source_sha256')!r}, live={actual}"
            )
        for field in ("r2_source_hash_claims", "adjudication_current_source_hash_claims"):
            claims = state.get(field, [])
            if not isinstance(claims, list):
                raise ValueError(f"r3 row {index} has malformed {field}")
            for claim in claims:
                if not isinstance(claim, Mapping) or not _is_hash(claim.get("sha256")):
                    raise ValueError(f"r3 row {index} has invalid bound hash in {field}")
                if claim["sha256"] != actual:
                    raise ValueError(
                        f"live source SHA does not match {field} claim for {path}: "
                        f"{claim.get('field') or claim.get('field_path')}"
                    )


def write_package(
    output_dir: Path,
    rows: Sequence[dict[str, Any]],
    summary: Mapping[str, Any],
    manifest: Mapping[str, Any],
    repo_root: Path = REPO_ROOT,
) -> None:
    validate_rows(rows, expected_count=len(rows), repo_root=repo_root)
    recheck_live_source_hashes(rows, repo_root)
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
        recheck_live_source_hashes(rows, repo_root)
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
    write_package(
        args.output_dir,
        rows,
        summary,
        input_manifest(inputs),
        args.repo_root,
    )
    print(canonical_json({"output_dir": str(args.output_dir.resolve()), "summary": summary}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
