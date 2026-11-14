#!/usr/bin/env python3
"""Build a current, source-hash-bound schema-v4 expectation ledger.

This preparer consumes the independent semantic authority package (not route
bootstrap observations), recomputes the live route-candidate inventory, and
keeps every changed or unresolved source as a blocking row. It does not run
Mimi, Cargo, or tests. The output schema-v1 JSON document is the input to
``render_independent_route_authorities.py``; the JSONL ledger is the audit
record with per-source semantic, direct-test, and blocker contracts.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
from typing import Any, Mapping
import stdlib_matrix_artifacts


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_AUTHORITY_PACKAGE = Path(
    "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/gates/"
    "schema-v4-current-authority-package-20261112-luna/schema-v4-authority-1187.jsonl"
)
TOOL_DIAGNOSTIC_INPUT = "projects/mimi-lint/src/test_bad.mimi"
TOOL_DIAGNOSTIC_SOURCE = "projects/mimi-lint/src/main.mimi"
COLLECTIONS_CORE_PREFIX = "tests/stdlib_matrix_generated/"
MAP_POSITIVE_TARGET = "tests/fixtures/mir_ordinary_map_owned_any_v2_direct_tuple_get_l1.mimi"
MATRIX_GENERATOR_PATH = "scripts/gen-stdlib-matrix.py"
MATRIX_RUNNER_PATH = "scripts/run-stdlib-matrix.sh"
MAP_CONTRACT_PATH = "devdocs/v0.41/map-any-contract-0.1.11.md"
ALLOWED_CLASSES = {
    "required_compile",
    "expected_build_rejection",
    "expected_source_diagnostic",
    "interpreter_only",
    "known_gap",
    "unmapped",
    "environment_failure",
    "unresolved",
}


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def load_module(path: Path) -> Any:
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"source module is unavailable or not regular: {path}")
    spec = importlib.util.spec_from_file_location("actor_schema_v4_census_collector", path)
    if spec is None or spec.loader is None:
        raise ValueError(f"cannot load route census collector: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def stable_contract_id(kind: str, path: str, source_sha256: str) -> str:
    identity = f"{kind}\0{path}\0{source_sha256}".encode("utf-8")
    return f"{kind}-{sha256_bytes(identity)[:20]}"


def load_authority_rows(path: Path) -> dict[str, Mapping[str, Any]]:
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"independent source authority JSONL is unavailable: {path}")
    rows: dict[str, Mapping[str, Any]] = {}
    with path.open("rb") as stream:
        for line_number, raw in enumerate(stream, 1):
            if not raw.strip():
                continue
            try:
                row = json.loads(raw.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise ValueError(f"{path}:{line_number}: invalid JSONL row: {error}") from error
            if not isinstance(row, Mapping) or not isinstance(row.get("path"), str):
                raise ValueError(f"{path}:{line_number}: expected a source path row")
            source_path = str(row["path"])
            if source_path in rows:
                raise ValueError(f"duplicate independent authority path: {source_path}")
            rows[source_path] = row
    return rows


def validate_prior_provenance(
    prior: Mapping[str, Any], source_path: str, current_source_sha256: str
) -> dict[str, Any]:
    """Fail closed when independent class/reason/hash provenance conflicts.

    The authority package intentionally preserves claims from multiple review
    sources. This builder must not silently prefer its top-level class while
    copying a reason from a different nested record. A disagreement therefore
    becomes an unresolved row with the complete source claims retained.
    """
    basis = prior.get("normative_or_user_target_basis")
    basis = basis if isinstance(basis, Mapping) else {}
    expected = prior.get("expected_class")
    claims = basis.get("prior_class_source_claims")
    issues: list[str] = []
    normalized_claims: list[dict[str, Any]] = []
    claimed_classes: set[str] = set()

    if not isinstance(expected, str) or expected not in ALLOWED_CLASSES:
        issues.append("authority package expected_class is missing or unsupported")
    if not isinstance(claims, list) or not claims:
        issues.append("authority package has no prior_class_source_claims list")
    else:
        for index, claim in enumerate(claims):
            if not isinstance(claim, Mapping):
                issues.append(f"prior class source claim {index} is not an object")
                continue
            claim_class = claim.get("class")
            source_id = claim.get("source_id")
            field = claim.get("field")
            normalized_claims.append({
                "source_id": source_id,
                "class": claim_class,
                "field": field,
            })
            if not isinstance(source_id, str) or not source_id.strip():
                issues.append(f"prior class source claim {index} has no source_id")
            if not isinstance(claim_class, str) or claim_class not in ALLOWED_CLASSES:
                issues.append(f"prior class source claim {index} has an unsupported class")
            else:
                claimed_classes.add(claim_class)
            if not isinstance(field, str) or not field.strip():
                issues.append(f"prior class source claim {index} has no field provenance")

    if len(claimed_classes) > 1:
        issues.append(
            "prior class source claims disagree: " + ", ".join(sorted(claimed_classes))
        )
    prior_ruling_class = basis.get("prior_ruling_class")
    if prior_ruling_class is not None:
        if not isinstance(prior_ruling_class, str) or prior_ruling_class not in ALLOWED_CLASSES:
            issues.append("prior_ruling_class is unsupported")
        elif claimed_classes and prior_ruling_class not in claimed_classes:
            issues.append("prior_ruling_class is absent from prior class source claims")
    if (
        isinstance(expected, str)
        and expected not in {"unresolved"}
        and claimed_classes
        and claimed_classes != {expected}
    ):
        issues.append(
            f"top-level expected_class {expected!r} disagrees with prior class source claims "
            f"{sorted(claimed_classes)!r}"
        )

    prior_independent_sha = prior.get("prior_independent_sha256")
    nested_prior_sha = basis.get("prior_sha256")
    is_new = prior_independent_sha is None
    if is_new:
        new_source_issues: list[str] = []
        if claims is not None:
            if not isinstance(claims, list):
                new_source_issues.append("unbound new authority row has malformed prior class claims")
            else:
                for index, claim in enumerate(claims):
                    if not isinstance(claim, Mapping):
                        new_source_issues.append(f"prior class source claim {index} is not an object")
                        continue
                    if (
                        not isinstance(claim.get("class"), str)
                        or claim.get("class") not in ALLOWED_CLASSES
                        or not isinstance(claim.get("source_id"), str)
                        or not isinstance(claim.get("field"), str)
                    ):
                        new_source_issues.append(f"prior class source claim {index} is malformed or unsupported")
                if len(claimed_classes) > 1:
                    new_source_issues.append(
                        "unbound new authority row has conflicting prior class claims: "
                        + ", ".join(sorted(claimed_classes))
                    )
        return {
            "status": "current_source_contract_conflict_fail_closed" if new_source_issues else "new_source_requires_independent_current_contract",
            "issues": new_source_issues,
            "source_path": source_path,
            "expected_class_claimed": prior.get("expected_class"),
            "prior_claimed_classes": sorted(claimed_classes),
            "prior_class_source_claims": normalized_claims,
            "reason_source_key": "current-source-contract",
            "reason_bound_source_sha256": None,
            "prior_independent_sha256": None,
            "nested_prior_sha256": nested_prior_sha,
            "current_source_sha256": current_source_sha256,
            "is_new_authority_row": True,
        }
    else:
        if (
            not isinstance(prior_independent_sha, str)
            or len(prior_independent_sha) != 64
            or any(character not in "0123456789abcdef" for character in prior_independent_sha)
        ):
            issues.append("prior_independent_sha256 is missing or malformed")
        if not isinstance(nested_prior_sha, str) or nested_prior_sha != prior_independent_sha:
            issues.append("nested prior_sha256 does not match prior_independent_sha256")
        authority_snapshot_sha = prior.get("current_sha256")
        if (
            not isinstance(authority_snapshot_sha, str)
            or len(authority_snapshot_sha) != 64
            or any(character not in "0123456789abcdef" for character in authority_snapshot_sha)
        ):
            issues.append("authority package current_sha256 is missing or malformed")
        snapshot_matches_prior = authority_snapshot_sha == prior_independent_sha
        if basis.get("current_source_sha256_matches_prior") is not snapshot_matches_prior:
            issues.append("nested current_source_sha256_matches_prior contradicts the recorded hashes")

    reason_key = "prior_independent_reason"
    reason = basis.get(reason_key)
    if not isinstance(reason, str) or not reason.strip():
        issues.append(f"{reason_key} is missing or empty")

    return {
        "status": "consistent" if not issues else "conflict_fail_closed",
        "issues": issues,
        "expected_class_claimed": expected,
        "prior_claimed_classes": sorted(claimed_classes),
        "prior_class_source_claims": normalized_claims,
        "reason_source_key": reason_key,
        "reason_bound_source_sha256": prior_independent_sha,
        "source_path": source_path,
        "prior_independent_sha256": prior_independent_sha,
        "nested_prior_sha256": nested_prior_sha,
        "current_source_sha256": current_source_sha256,
        "is_new_authority_row": is_new,
    }


def orthogonal_status_fields(
    expectation_class: str,
    direct_contracts: list[dict[str, Any]],
    tool_contract: Mapping[str, Any] | None,
) -> dict[str, Any]:
    """Record semantic, route, contract, and execution dimensions separately."""
    if expectation_class == "expected_build_rejection":
        source_semantic_class: dict[str, Any] = {
            "value": "not_separately_adjudicated",
            "status": "build-rejection-source-semantics-not-split-in-current-authority",
        }
    else:
        source_semantic_class = {
            "value": expectation_class,
            "status": "source-authority-candidate" if expectation_class != "unresolved" else "unresolved",
        }
    if direct_contracts:
        contract_status = "inventory_only_not_route_authority"
    elif tool_contract is not None:
        contract_status = "pending_exact_tool_execution"
    else:
        contract_status = "no_direct_contract_inventory"
    return {
        "source_semantic_class": source_semantic_class,
        "default_route_disposition": {
            "candidate_from_authority_class": expectation_class,
            "status": "not_executed",
        },
        "canonical_mir_only_input_rejection": {
            "status": "not_separately_adjudicated",
            "contract_id": None,
            "execution_status": "not_run",
        },
        "direct_contract_status": {
            "status": contract_status,
            "count": len(direct_contracts),
            "candidate_statuses": [str(item.get("status")) for item in direct_contracts],
        },
        "execution_status": "not_run",
    }


def direct_contract_inventory(
    source_path: str,
    row: Mapping[str, Any],
    root: Path,
) -> list[dict[str, Any]]:
    raw_contracts = row.get("exact_direct_rust_contracts")
    if not isinstance(raw_contracts, list):
        return []
    result: list[dict[str, Any]] = []
    for raw in raw_contracts:
        if not isinstance(raw, Mapping):
            continue
        test_id = raw.get("test_id")
        test_path = raw.get("test_path")
        if not isinstance(test_id, str) or not isinstance(test_path, str):
            continue
        candidate = root / test_path
        current_hash = sha256_file(candidate) if candidate.is_file() and not candidate.is_symlink() else None
        recorded_hash = raw.get("test_source_sha256_current")
        exact_source_pin = (
            isinstance(raw.get("source_hash_binding"), Mapping)
            and raw["source_hash_binding"].get("test_pins_source_sha256") is True
        )
        active = raw.get("test_function_exists_current") is True and raw.get("ignored_static_marker") is False
        blockers: list[str] = []
        if current_hash is None:
            blockers.append("referenced Rust test source is missing or symlinked")
        elif recorded_hash != current_hash:
            blockers.append("referenced Rust test source hash is stale against the current worktree")
        if not active:
            blockers.append("test reference is not confirmed active and non-ignored")
        if not exact_source_pin:
            blockers.append("test reference does not pin this Mimi source SHA-256")
        result.append({
            "contract_id": stable_contract_id("rust-test", source_path + "\0" + test_id, str(row.get("current_sha256", ""))),
            "test_id": test_id,
            "test_path": test_path,
            "test_source_sha256_recorded": recorded_hash,
            "test_source_sha256_current": current_hash,
            "binding_kind": raw.get("binding_kind"),
            "ignored_static_marker": raw.get("ignored_static_marker"),
            "test_function_exists_current": raw.get("test_function_exists_current"),
            "pins_exact_mimi_source_sha256": exact_source_pin,
            "status": "candidate_inventory_only" if blockers else "candidate_needs_route_recognizer",
            "assertion_evidence": raw.get("assertion_evidence"),
            "blockers": blockers or ["strict schema-v4 active route recognizer and execution have not been established"],
        })
    return result


def classify_new_source(
    path: str,
    source_text: str,
    root: Path,
    matrix_outputs: Mapping[str, str],
    matrix_artifact: Mapping[str, Any],
) -> tuple[str, str, dict[str, Any]]:
    if path == MAP_POSITIVE_TARGET:
        contract_path = root / MAP_CONTRACT_PATH
        required_fragments = ("map_new()", "map_set(", "map_get(", "unpack_i32(")
        exact_shape = all(fragment in source_text for fragment in required_fragments)
        if contract_path.is_file() and not contract_path.is_symlink() and exact_shape:
            return (
                "required_compile",
                "Explicit 0.1.11 positive Map/Any implementation target: map_new -> map_set -> map_get -> unpack_i32; the current fixture has the declared direct tuple-get shape and is bound to the current Map/Any contract document.",
                {
                    "kind": "explicit_0_1_11_target",
                    "source_path": path,
                    "source_sha256": sha256_file(root / path),
                    "normative_contract_path": MAP_CONTRACT_PATH,
                    "normative_contract_sha256": sha256_file(contract_path),
                    "required_source_shape": list(required_fragments),
                },
            )
        return (
            "unresolved",
            "The new Map/Any fixture does not match the expected direct tuple-get source shape or its normative contract document is unavailable; no route class is inferred.",
            {
                "kind": "explicit_target_shape_unconfirmed",
                "source_path": path,
                "required_source_shape": list(required_fragments),
                "normative_contract_path": MAP_CONTRACT_PATH,
            },
        )
    if path.startswith(stdlib_matrix_artifacts.SOURCE_PREFIX):
        entry = matrix_artifact.get("files", {}).get(path)
        relative = path[len(stdlib_matrix_artifacts.SOURCE_PREFIX):]
        expected_text = matrix_outputs.get(relative)
        if (
            isinstance(entry, Mapping)
            and expected_text is not None
            and expected_text == source_text
            and entry.get("sha256") == sha256_bytes(source_text.encode("utf-8"))
        ):
            generator_path = root / MATRIX_GENERATOR_PATH
            runner_path = root / MATRIX_RUNNER_PATH
            return (
                "required_compile",
                "Generated stdlib matrix source exactly matches the verified current generator and stdlib inputs. The parity runner records test intent only; route success remains unverified until its exact source authority executes.",
                {
                    "kind": "generated_matrix_contract",
                    "source_path": path,
                    "source_sha256": sha256_bytes(source_text.encode("utf-8")),
                    "matrix_manifest_path": str(matrix_artifact["manifest_path"]),
                    "matrix_manifest_sha256": matrix_artifact["manifest_sha256"],
                    "generator_path": MATRIX_GENERATOR_PATH,
                    "generator_sha256": matrix_artifact["generator_sha256"],
                    "stdlib_inputs_sha256": matrix_artifact["stdlib_inputs_sha256"],
                    "runner_path": MATRIX_RUNNER_PATH,
                    "runner_sha256": sha256_file(runner_path),
                    "generator_output_sha256": entry["sha256"],
                    "expected_generator_output_matches_source": True,
                },
            )
    return (
        "unresolved",
        "New source path has no matching independently adjudicated semantic row or recognized exact generator contract; do not infer a class from route observations.",
        {
            "kind": "no_independent_current_source_contract",
            "source_path": path,
            "source_sha256": sha256_file(root / path),
        },
    )


def build(
    root: Path,
    authority_package: Path,
    stdlib_matrix_generated_root: Path,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    root = root.resolve(strict=True)
    collector_path = root / "scripts" / "full_route_census.py"
    builder_path = root / "scripts" / "build_actor_schema_v4_census.py"
    collector = load_module(collector_path)
    matrix_generator = load_module(root / MATRIX_GENERATOR_PATH)
    matrix_artifact = stdlib_matrix_artifacts.verify(root, stdlib_matrix_generated_root)
    matrix_outputs = matrix_generator.render_sources()
    current_sources = collector.enumerate_sources(
        root, stdlib_matrix_generated_root=stdlib_matrix_generated_root
    )
    current_by_path = {str(row["path"]): dict(row) for row in current_sources}
    if len(current_by_path) != len(current_sources):
        raise ValueError("current source roster contains duplicate paths")
    authority_rows = load_authority_rows(authority_package)
    current_paths = set(current_by_path)
    old_paths = set(authority_rows)
    missing_from_authority = sorted(current_paths - old_paths)
    stale_outside_current = sorted(old_paths - current_paths)
    if stale_outside_current:
        raise ValueError(f"independent authority package has paths outside current roster: {stale_outside_current[:10]}")

    current_count = len(current_sources)
    if current_count != 1187:
        raise ValueError(f"current source roster changed: expected reviewed current count 1187, got {current_count}")

    input_rows: dict[str, Any] = {}
    audit_rows: list[dict[str, Any]] = []
    class_counts: dict[str, int] = {}
    source_hash_mismatches = 0
    authority_snapshot_hash_mismatches = 0
    stale_semantic_rows = 0
    new_rows: list[str] = []
    authority_snapshot_mismatch_paths: list[str] = []
    provenance_conflict_paths: list[str] = []
    provenance_issue_counts: dict[str, int] = {}
    tool_source = root / TOOL_DIAGNOSTIC_SOURCE
    tool_source_sha = sha256_file(tool_source) if tool_source.is_file() and not tool_source.is_symlink() else None

    for index, source in enumerate(current_sources, 1):
        path = str(source["path"])
        source_sha = str(source["sha256"])
        prior = authority_rows.get(path)
        new = prior is None or not isinstance(prior.get("prior_independent_sha256"), str)
        if new:
            new_rows.append(path)
            physical_source = Path(str(source.get("external_source_path") or (root / path)))
            text = physical_source.read_text(encoding="utf-8")
            expectation_class, reason, authority_basis = classify_new_source(
                path, text, root, matrix_outputs, matrix_artifact
            )
            prior_sha = None
            semantic_status = "independently_classified_new_source"
            prior_contracts: list[dict[str, Any]] = []
            blockers: list[str] = []
            provenance_check = (
                validate_prior_provenance(prior, path, source_sha)
                if prior is not None
                else {
                    "status": "new_source_requires_independent_current_contract",
                    "issues": [],
                    "expected_class_claimed": None,
                    "prior_claimed_classes": [],
                    "prior_class_source_claims": [],
                    "reason_source_key": "current-source-contract",
                    "reason_source_sha256": None,
                    "prior_independent_sha256": None,
                    "nested_prior_sha256": None,
                    "current_source_sha256": source_sha,
                    "is_new_authority_row": True,
                }
            )
            provenance_basis = {
                "kind": "new_source_current_contract",
                "source_path": path,
                "source_sha256": source_sha,
                "provenance_validation": provenance_check,
            }
            if provenance_check["issues"]:
                expectation_class = "unresolved"
                semantic_status = "current_source_contract_provenance_conflict_unresolved"
                reason = (
                    "The newly recognized source contract is not used to override conflicting or malformed prior class claims: "
                    + "; ".join(provenance_check["issues"])
                )
                authority_basis = provenance_basis
                blockers = list(provenance_check["issues"])
        else:
            provenance_check = validate_prior_provenance(prior, path, source_sha)
            provenance_issues = list(provenance_check["issues"])
            prior_sha_value = prior.get("prior_independent_sha256")
            prior_sha = prior_sha_value if isinstance(prior_sha_value, str) else None
            authority_snapshot_sha_value = prior.get("current_sha256")
            authority_snapshot_sha = (
                authority_snapshot_sha_value
                if isinstance(authority_snapshot_sha_value, str)
                else None
            )
            authority_snapshot_matches = authority_snapshot_sha == source_sha
            if not authority_snapshot_matches:
                authority_snapshot_hash_mismatches += 1
                authority_snapshot_mismatch_paths.append(path)
            basis_doc = prior.get("normative_or_user_target_basis")
            basis_doc = basis_doc if isinstance(basis_doc, Mapping) else {}
            base_reason = basis_doc.get("prior_independent_reason")
            if not isinstance(base_reason, str) or not base_reason.strip():
                base_reason = "Prior authority reason is missing; this row remains unresolved pending current-byte review."
            prior_semantics_stale = prior_sha is not None and prior_sha != source_sha
            if prior_semantics_stale or not authority_snapshot_matches or provenance_issues:
                if prior_semantics_stale:
                    source_hash_mismatches += 1
                    stale_semantic_rows += 1
                expectation_class = "unresolved"
                semantic_status = (
                    "source_class_reason_provenance_conflict_unresolved"
                    if provenance_issues
                    else "stale_source_hash_unresolved"
                )
                stale_causes: list[str] = []
                if prior_semantics_stale:
                    stale_causes.append(
                        f"prior independent semantic bytes {prior_sha} -> {source_sha}"
                    )
                if not authority_snapshot_matches:
                    stale_causes.append(
                        f"authority-package current snapshot {authority_snapshot_sha} -> {source_sha}"
                    )
                if provenance_issues:
                    stale_causes.extend(provenance_issues)
                reason = (
                    "Independent class/reason/source provenance is stale or inconsistent ("
                    + "; ".join(stale_causes)
                    + "); the prior class remains provenance only and this current row stays unresolved until exact-byte re-review."
                )
                authority_basis = {
                    "kind": "stale_prior_semantic_ruling",
                    "prior_authority_basis": dict(basis_doc),
                    "provenance_validation": provenance_check,
                    "prior_independent_sha256": prior_sha,
                    "authority_package_current_sha256": authority_snapshot_sha,
                    "authority_package_current_sha256_matches": authority_snapshot_matches,
                    "current_source_sha256": source_sha,
                    "prior_expected_class": prior.get("expected_class"),
                    "prior_reason": base_reason,
                }
                blockers = ["stale source bytes invalidate the prior semantic ruling; exact current-byte adjudication is required"]
                if provenance_issues:
                    blockers.extend(provenance_issues)
            else:
                expectation_class = str(prior.get("expected_class", "unresolved"))
                semantic_status = str(prior.get("semantic_class_status") or "reused_hash_matched_independent_ruling")
                reason = base_reason
                authority_basis = {
                    **dict(basis_doc),
                    "provenance_validation": provenance_check,
                }
                blockers = [str(item) for item in prior.get("authority_blockers", []) if isinstance(item, str)] if isinstance(prior.get("authority_blockers"), list) else []
                if expectation_class in {"expected_build_rejection", "expected_source_diagnostic"}:
                    blockers.append("full exact route diagnostic/output/exit contract is not present in the independent row; preserve as blocker")
            prior_contracts = direct_contract_inventory(path, prior, root)
        if provenance_check["issues"]:
            provenance_conflict_paths.append(path)
            for issue in provenance_check["issues"]:
                provenance_issue_counts[issue] = provenance_issue_counts.get(issue, 0) + 1
        if path == TOOL_DIAGNOSTIC_INPUT:
            expectation_class = "unresolved"
            semantic_status = "tool_diagnostic_contract_unresolved"
            reason = (
                "This is input to mimi-lint, not a Mimi compiler rejection. Its tool promises diagnostics that the current lint implementation does not fully produce; "
                "the exact complete tool output, stable unused-variable diagnostic identity, current Mimi executable hash, and executed exit/output contract are not established."
            )
            authority_basis = {
                "kind": "tool_diagnostic_contract",
                "tool_source_path": TOOL_DIAGNOSTIC_SOURCE,
                "tool_source_sha256": tool_source_sha,
                "source_claims": ["W001", "W002", "W004", "W005", "unused-variable diagnostic"],
                "static_gap": "projects/mimi-lint/src/main.mimi advertises unused-variable detection but has no unused-variable scan; its W006 reports statement-count guidance, and its complexity scan sees only the statement list supplied by the parser. Exact current output remains unexecuted.",
            }
            blockers = [
                "no regular Mimi executable is present at target/debug/mimi or target/release/mimi, so no executable path/hash can be bound without a build",
                "no executed tool output bound to the current Mimi executable SHA-256",
                "no stable unused-variable diagnostic code/message is specified",
                "the tool's promised complete diagnostics are not all implemented in the current source",
            ]
        contract_id = stable_contract_id("schema-v4", path, source_sha)
        tool_contract = None
        if path == TOOL_DIAGNOSTIC_INPUT:
            tool_contract = {
                "schema": "mimi-tool-diagnostic-contract-v1",
                "contract_id": stable_contract_id("tool-diagnostic", path, source_sha),
                "status": "pending_exact_execution",
                "input": {"path": path, "source_sha256": source_sha},
                "command": {
                    "argv": ["<CURRENT_MIMI_EXECUTABLE>", "run", TOOL_DIAGNOSTIC_SOURCE, path],
                    "cwd": ".",
                    "command_status": "proposed_from_tool_usage_comment_not_executed",
                },
                "tool": {
                    "source_path": TOOL_DIAGNOSTIC_SOURCE,
                    "source_sha256": tool_source_sha,
                    "executable_path": None,
                    "executable_sha256": None,
                    "executable_discovery": {
                        "status": "no_candidate_found",
                        "checked_paths": ["target/debug/mimi", "target/release/mimi"],
                        "checked_without_execution": True,
                    },
                },
                "expected": {
                    "exit_code": None,
                    "stdout_utf8": None,
                    "stdout_sha256": None,
                    "stderr_utf8": None,
                    "stderr_sha256": None,
                    "full_output_captured": False,
                },
                "blockers": blockers,
            }
            contract_id = str(tool_contract["contract_id"])
        if expectation_class not in ALLOWED_CLASSES:
            raise ValueError(f"unsupported semantic class for {path}: {expectation_class}")
        class_counts[expectation_class] = class_counts.get(expectation_class, 0) + 1
        input_value: dict[str, Any] = {
            "class": expectation_class,
            "reason": reason,
            "source_sha256": source_sha,
            "contract_id": contract_id,
            "contract_kind": "tool_diagnostic" if tool_contract else "default_mimi_build_route",
            "blocker": "; ".join(dict.fromkeys(blockers)) if blockers else "Strict source-bound route authority has not yet been executed for this row.",
        }
        if tool_contract is not None:
            input_value["tool_diagnostic_contract"] = tool_contract
        dimensions = orthogonal_status_fields(expectation_class, prior_contracts, tool_contract)
        input_value.update(dimensions)
        input_rows[path] = input_value

        row_blockers = list(dict.fromkeys(blockers))
        if expectation_class == "required_compile":
            row_blockers.append("exact generated Rust route authority has not been executed against the current Mimi executable")
        if expectation_class == "unresolved":
            row_blockers.append("semantic class remains unresolved; row is not a successful route expectation")
        direct_contract_id = contract_id
        audit_rows.append({
            "inventory_index": index,
            "path": path,
            "source_sha256": source_sha,
            "target_kind": source.get("target_kind"),
            "semantic_class": expectation_class,
            "semantic_class_status": semantic_status,
            "reason": reason,
            "authority_basis": authority_basis,
            "contract_id": direct_contract_id,
            "contract_kind": "tool_diagnostic" if tool_contract else "default_mimi_build_route",
            "direct_rust_test_contracts": prior_contracts,
            "tool_diagnostic_contract": tool_contract,
            **dimensions,
            "authority_provenance_validation": (
                provenance_check if prior is not None else provenance_basis["provenance_validation"]
            ),
            "prior_semantic_source_sha256": prior_sha,
            "authority_package_current_source_sha256": (
                authority_snapshot_sha if prior is not None and not new else None
            ),
            "authority_package_current_source_hash_matches": (
                authority_snapshot_matches if prior is not None and not new else None
            ),
            "prior_semantic_class": prior.get("expected_class") if prior is not None else None,
            "prior_source_hash_matches": prior_sha == source_sha if prior_sha else False,
            "route_bootstrap_observation_used_for_class": False,
            "unresolved_blockers": list(dict.fromkeys(row_blockers)),
        })

    if set(input_rows) != current_paths or len(audit_rows) != current_count:
        raise ValueError("source-bound expectation manifest does not exactly cover the current roster")
    manifest = {
        "schema_version": 1,
        "manifest_kind": "independent-source-bound-schema-v4-expectations",
        "decision_authority": "source-bound semantic rulings, current source hash, normative intent, and exact tool/test contract inventory; route bootstrap observations are excluded",
        "builder": {
            "path": "scripts/build_actor_schema_v4_census.py",
            "sha256": sha256_file(builder_path),
            "provenance_policy": "contradictory class claims and inconsistent class/reason/hash provenance fail closed as unresolved",
        },
        "repository_root": str(root),
        "stdlib_matrix_generated_artifact": {
            key: value for key, value in matrix_artifact.items() if key != "files"
        },
        "source_roster": {
            "enumerator": "scripts/full_route_census.py::enumerate_sources",
            "enumerator_sha256": sha256_file(collector_path),
            "candidate_count": current_count,
            "paths_sha256": sha256_bytes("\0".join(str(row["path"]) for row in current_sources).encode("utf-8")),
            "source_path_sha256_sha256": sha256_bytes(canonical_bytes([
                {"path": row["path"], "sha256": row["sha256"]} for row in current_sources
            ])),
        },
        "prior_independent_authority_package": {
            "path": str(authority_package.resolve(strict=True)),
            "sha256": sha256_file(authority_package),
            "rows": len(authority_rows),
            "same_source_sha_rows_reused": current_count - stale_semantic_rows - len(new_rows),
            "stale_semantic_rows_forced_unresolved": stale_semantic_rows,
            "new_rows_independently_classified": len(new_rows),
            "current_snapshot_sha_mismatch_count": authority_snapshot_hash_mismatches,
            "current_snapshot_sha_mismatch_paths": sorted(authority_snapshot_mismatch_paths),
        },
        "denominator_change": {
            "prior_count": 1161,
            "current_count": current_count,
            "added_count": current_count - 1161,
            "added_path_families": {
                "collections_core_generated_matrix_probes": sum(
                    path.startswith(COLLECTIONS_CORE_PREFIX) and "collections_core" in Path(path).name
                    for path in new_rows
                ),
                "map_positive_fixture": sum(path == MAP_POSITIVE_TARGET for path in new_rows),
            },
            "new_paths": sorted(new_rows),
        },
        "class_counts": dict(sorted(class_counts.items())),
        "provenance_validation": {
            "status": "conflicts_fail_closed_as_unresolved",
            "conflicted_row_count": len(provenance_conflict_paths),
            "conflicted_paths": sorted(provenance_conflict_paths),
            "issue_counts": dict(sorted(provenance_issue_counts.items())),
        },
        "dimension_status_counts": {
            "default_route_disposition": {"not_executed": current_count},
            "canonical_mir_only_input_rejection": {"not_separately_adjudicated": current_count},
            "direct_contract_status": dict(sorted({
                status: sum(
                    1 for row in audit_rows
                    if row["direct_contract_status"]["status"] == status
                )
                for status in {row["direct_contract_status"]["status"] for row in audit_rows}
            }.items())),
            "execution_status": {"not_run": current_count},
        },
        "strict_route_authority": {
            "execution_performed": False,
            "accepted": False,
            "all_current_source_hashes_match": True,
            "reason": "This is a source-bound expectation/contract ledger only. Strict route, diagnostics, tool outputs, and test executions remain unverified.",
        },
        "inputs": input_rows,
    }
    return manifest, audit_rows


def write_outputs(
    root: Path,
    authority_package: Path,
    output_dir: Path,
    stdlib_matrix_generated_root: Path,
) -> dict[str, Any]:
    if output_dir.exists() or output_dir.is_symlink():
        raise ValueError(f"refusing to overwrite output directory: {output_dir}")
    manifest, rows = build(root, authority_package, stdlib_matrix_generated_root)
    output_dir.mkdir(parents=True)
    manifest_path = output_dir / "schema-v4-independent-expectations-v1.json"
    ledger_path = output_dir / "schema-v4-source-bound-ledger-1187.jsonl"
    summary_path = output_dir / "summary.json"
    manifest_path.write_bytes(canonical_bytes(manifest))
    ledger_path.write_bytes(b"".join(
        (json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
        for row in rows
    ))
    summary = {key: value for key, value in manifest.items() if key != "inputs"}
    summary["artifacts"] = {
        manifest_path.name: {"bytes": manifest_path.stat().st_size, "sha256": sha256_file(manifest_path)},
        ledger_path.name: {"bytes": ledger_path.stat().st_size, "sha256": sha256_file(ledger_path)},
    }
    summary_path.write_bytes(canonical_bytes(summary))
    checksum_path = output_dir / "SHA256SUMS"
    checksum_path.write_text(
        "".join(f"{sha256_file(output_dir / name)}  {name}\n" for name in (manifest_path.name, ledger_path.name, summary_path.name)),
        encoding="utf-8",
    )
    return summary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--authority-package", type=Path, default=DEFAULT_AUTHORITY_PACKAGE)
    parser.add_argument("--stdlib-matrix-generated-root", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    summary = write_outputs(
        args.root,
        args.authority_package,
        args.output_dir,
        args.stdlib_matrix_generated_root,
    )
    print(json.dumps({
        "status": "source_bound_expectations_generated_not_executed",
        "count": summary["source_roster"]["candidate_count"],
        "class_counts": summary["class_counts"],
        "output_dir": str(args.output_dir.resolve()),
        "all_route_bootstrap_observations_used_for_class": False,
    }, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
