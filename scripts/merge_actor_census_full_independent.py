#!/usr/bin/env python3
"""Strict candidate merger for the nine independent Actor census shards.

The current locator inventory is projected to exactly ``(input_index, path,
sha256)`` from ``records[]``. No classification, route, test result, or runtime
observation from that inventory is copied. Full-shard source classes remain
unaccepted candidates; default and explicit ``--mir`` route expectations are
separate, unresolved axes. This program never invokes Mimi, Cargo, a test, or
a build.

The real evidence package is intentionally not produced by this source edit.
Use the CLI only after all nine shards and their referenced sources are frozen.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import stat
import sys
import tempfile
from collections import Counter
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Mapping, Sequence


COUNT = 1187
LOCATOR_IDENTITY_FIELDS = ("input_index", "path")
LOCATOR_PROVENANCE_HASH_FIELD = "sha256"
FROZEN_LOCATOR_INPUT = (
    "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/"
    "actor-schema-v4-source-bound-current-20261113-r1/inventory-current-r2.json"
)
FROZEN_LOCATOR_SHA256 = "4d7abb36829128b2c1016440443a0637a7f0aa35cdf7ac28eaf4fa703740af45"
SHARD_RANGES: tuple[tuple[int, int], ...] = (
    (1, 132),
    (133, 264),
    (265, 396),
    (397, 528),
    (529, 660),
    (661, 792),
    (793, 924),
    (925, 1056),
    (1057, 1187),
)
FROZEN_SHARD_INPUTS: tuple[tuple[str, str], ...] = (
    (
        "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/actor-census-full-independent-rereview-20261113-shard001/shard-001-0001-0132.jsonl",
        "a4688095600ebc1a73407d130a66d88e986d504253e101441795a1a75058467b",
    ),
    (
        "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/actor-census-full-independent-adjudication-0133-0264-20261113/actor-census-full-independent-adjudication-0133-0264-20261113.jsonl",
        "8e9b21ac91ba398d09f0cf362fcc2fe5072792fe3363cec2b40a19cf29a1a39f",
    ),
    (
        "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/actor-census-full-independent-source-review-0265-0396-20261113/shard-003-0265-0396.jsonl",
        "a5c65d88898edb65b94a61eacaaf18652337aaa2b007fb080d15c71cc04aea61",
    ),
    (
        "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/actor-census-full-independent-20261113/full-review-shard-004-0397-0528-r2-20261113.jsonl",
        "60117c1e19b102cca6fef6c28263394932dcd0f0de9d9eb216c9e70dfb278866",
    ),
    (
        "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/actor-census-full-independent-adjudication-0529-0660-20261113-r2.jsonl",
        "40e6c43942a388b100403dcb11a3aa7b63bcc57447666782877a89561e1623d9",
    ),
    (
        "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/actor-census-full-independent-rereview-0661-0792-20261113-b1/actor-census-full-independent-rereview-0661-0792.jsonl",
        "77ba1a33a1c4de34f7c6d25988a898e068d2b04ae225f14f48e70c3021312673",
    ),
    (
        "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/actor-census-full-independent-adjudication-0793-0924-20261113-v1.jsonl",
        "579e655a1250cc4dc433d7b801a9ffe9e52232b8bc6e6cd29530784bdd26a41f",
    ),
    (
        "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/actor-census-independent-full-20261113-r2-shard-008-0925-1056/shard-008-0925-1056.jsonl",
        "d7c81ac1cd70ca270322b37097328065104218c57f43facc2186591c8ef95021",
    ),
    (
        "/media/ontos-hpc/P7000Z2T/mimi-evidence/0.1.11-dev/census/actor-census-full-independent-adjudication-r2-20261113/shard-009-1057-1187.jsonl",
        "13f10b2f7e544f5d335da7feee967abaf538a68bf67d66cb93c9eb8dc443ef20",
    ),
)
SOURCE_CLASSES = frozenset(
    {
        "required_compile",
        "expected_build_rejection",
        "expected_source_diagnostic",
        "interpreter_only",
        "known_gap",
        "unmapped",
        "environment_failure",
        "unresolved",
    }
)
APPROVED_LOCATOR_REFRESHES: dict[int, dict[str, str]] = {
    240: {
        "path": "tests/fixtures/mir_map_root_default_verify.mimi",
        "r2_sha256": "eab37a00629ae84291fe22e0554a31aaf5db8340bc3f53816a61c21696e49d96",
        "live_sha256": "1482b17a537f56729eef49d378719b5c79039b53affaf982abc5ba00954175b6",
    },
    244: {
        "path": "tests/fixtures/mir_map_root_string_default_verify.mimi",
        "r2_sha256": "b0dcc1bfca4b1904be4a38ca625032cd9e255914da62730b309361d038d4ceed",
        "live_sha256": "c7a7b382a71cfde5174ae7a62f27f791ac517435a41969e71fca955697bc2f80",
    },
    719: {
        "path": "tests/real_world/actor_nested_func_capture.mimi",
        "r2_sha256": "58e39d7fc0c5ad0b7538f12c3d056ccd00d5e2f1d3bd5ae42c687a63c7fadec3",
        "live_sha256": "bfa4233b9a5007762e00f8f0fb10bd50814fb504a7fe481135d204cb2c800e4d",
    },
    720: {
        "path": "tests/real_world/actor_nested_func_shadow.mimi",
        "r2_sha256": "18c88f220b8311b32c514b40370b37931ae4bd640c7d38b7d548a17120c0e328",
        "live_sha256": "59a8b0b68ed0242414cb9c6cdd940a9a80d1f4ff8fc227bd94cecfe11f56fed8",
    },
}
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
ROW_KEYS = frozenset(
    {
        "accepted",
        "accepted_as_route_pass",
        "bootstrap_or_runtime_observation_used",
        "conflicts_and_limits",
        "direct_test_contracts",
        "direct_test_gaps",
        "expected_class",
        "inventory_index",
        "mimi_or_cargo_or_test_or_build_executed",
        "normative_basis",
        "semantic_basis",
        "source_path",
        "source_sha256_current",
        "status",
    }
)
REREVIEW_ROW_KEYS = frozenset(
    {
        "accepted",
        "accepted_as_route_pass",
        "bootstrap_or_runtime_observation_used",
        "candidate_semantic_class",
        "conflicts_and_limits",
        "default_route_expectation",
        "direct_test_contracts",
        "direct_test_gaps",
        "expected_class",
        "explicit_mir_route_expectation",
        "inventory_index",
        "mimi_or_cargo_or_test_or_build_executed",
        "normative_basis",
        "prior_semantic_snapshot_drift",
        "r2_locator_drift",
        "semantic_basis",
        "source_evidence_lines",
        "source_hash_differs_from_prior_semantic_snapshot",
        "source_hash_drift_during_review",
        "source_path",
        "source_sha256_at_review",
        "source_sha256_current",
        "source_sha256_prior_semantic_snapshot",
        "source_sha256_r2_locator",
        "status",
    }
)
FROZEN_SOURCE_SCHEMA_KEYS: tuple[tuple[str, frozenset[str]], ...] = (
    (
        "independent_rereview_001_v1",
        frozenset("""
            accepted accepted_as_route_pass bootstrap_or_runtime_observation_used candidate_class
            conflicts_and_gaps default_route_expectation direct_test_contract direct_test_gaps
            explicit_mir_route_expectation inventory_index mimi_or_cargo_or_tests_or_build_executed
            normative_basis semantic_basis source_hash_drift_from_roster_anchor source_hash_status
            source_line_evidence source_path source_sha256_live_at_review source_sha256_live_final_recheck
            source_sha256_roster_anchor status
        """.split()),
    ),
    (
        "independent_adjudication_002_v1",
        frozenset("""
            accepted accepted_as_route_pass bootstrap_or_runtime_observation_used candidate_semantic_class
            conflicts_and_limits direct_test_contract direct_test_gaps inventory_index
            mimi_or_cargo_or_test_or_build_executed normative_basis route_expectation semantic_basis
            source_absolute_path source_hash_drift source_line_anchors source_path
            source_sha256_current source_sha256_rechecked_after_review status unresolved_normative_question
        """.split()),
    ),
    (
        "independent_source_review_003_v1",
        frozenset("""
            accepted accepted_as_route_pass bootstrap_or_runtime_observation_used candidate_semantic_class
            conflicts_and_limits default_route_expectation direct_test_contract direct_test_gaps expected_class
            explicit_mir_route_expectation inventory_index mimi_or_cargo_or_test_or_build_executed
            normative_basis semantic_basis source_drift_during_review source_path source_sha256_current
            source_sha256_review_end source_sha256_review_start source_structure status
        """.split()),
    ),
    (
        "independent_full_review_004_r2",
        frozenset("""
            accepted accepted_as_route_pass bootstrap_or_runtime_observation_used classification_status
            conflicts_and_limits default_route_expectation direct_test_contract_summary direct_test_contracts
            direct_test_gaps expected_class explicit_mir_route_expectation inventory_index
            mimi_or_cargo_or_test_or_build_executed non_normative_source_comments normative_basis
            proposed_direct_source_contract semantic_basis source_anchor_lines source_hash_drift
            source_hash_rechecked_stable source_line_count source_path source_semantic_candidate_class
            source_sha256_after_review source_sha256_current status
        """.split()),
    ),
    (
        "independent_adjudication_005_r2",
        frozenset("""
            accepted accepted_as_route_pass bootstrap_or_runtime_observation_used conflicts_and_gaps
            direct_test_contract direct_test_gaps expected_class inventory_index
            mimi_or_cargo_or_test_or_build_executed normative_basis semantic_basis source_evidence
            source_hash_drift source_path source_sha256_current source_sha256_review_end status
        """.split()),
    ),
    (
        "independent_rereview_006_b1",
        REREVIEW_ROW_KEYS,
    ),
    (
        "independent_adjudication_007_v1",
        frozenset("""
            accepted accepted_as_route_pass blocked_question bootstrap_or_runtime_observation_used
            candidate_semantic_class direct_test_contract inventory_index
            mimi_or_cargo_or_test_or_build_executed normative_basis route_expectations schema semantic_reason
            source_evidence source_hash_drift source_hash_stable_during_review source_path
            source_sha256_current source_sha256_rechecked_before_finalization source_sha256_roster_anchor status
        """.split()),
    ),
    (
        "independent_adjudication_008_r2",
        frozenset("""
            accepted accepted_as_route_pass additional_gaps bootstrap_or_runtime_observation_used
            conflicts_and_gaps default_route_expectation direct_contracts direct_test_gaps
            existing_direct_test_contracts expected_class explicit_mir_route_expectation inventory_index
            mimi_or_cargo_or_test_or_build_executed normative_basis semantic_basis source_hash_drift
            source_path source_sha256_current source_sha256_review_end source_structure status
        """.split()),
    ),
    (
        "independent_adjudication_009_r2",
        frozenset("""
            accepted accepted_as_route_pass bootstrap_or_runtime_observation_used conflicts_and_limits
            default_route_expectation direct_test_contracts direct_test_gaps expected_class inventory_index
            mimi_or_cargo_or_test_or_build_executed mir_route_expectation normative_basis
            related_test_contract_sources semantic_basis source_evidence source_hash_drift source_path
            source_sha256_current source_sha256_recheck source_specific_normative_gaps status
        """.split()),
    ),
)
NORMATIVE_OBJECT_KEYS = frozenset(
    {
        frozenset({"claim", "file", "section"}),
        frozenset({"file", "line_reference", "relevance", "section"}),
        frozenset({"file", "lines", "section", "why"}),
        frozenset({"document", "line_ref", "rule", "section"}),
        frozenset({"file", "lines", "rule", "section"}),
        frozenset({"file", "line_refs", "rule", "section"}),
        frozenset({"file", "lines", "relevance", "section"}),
        frozenset({"claim_and_relevance", "file", "lines", "section", "sha256_current"}),
        frozenset({"file", "lines", "section", "sha256_current", "statement"}),
        frozenset({"file", "line_end", "line_start", "normative_text_summary", "section"}),
        frozenset({"reference", "rule"}),
        frozenset({"application", "authority", "line_end", "line_start", "path", "section", "sha256"}),
    }
)
REREVIEW_DIRECT_CONTRACT_KEYS = frozenset(
    {
        frozenset({"accepted", "blocker", "contract_status", "exact_command", "expected_oracle", "source_sha256", "source_under_test", "status"}),
        frozenset({"accepted", "contract_status", "exact_command", "expected_oracle", "source_sha256", "source_under_test", "status"}),
        frozenset({"assertion", "function", "kind", "line", "source_pin_status", "test_source_path", "test_source_sha256", "test_source_sha256_rechecked"}),
        frozenset({"assertion", "function_or_binding", "kind", "line", "source_pin_status", "test_source_path", "test_source_sha256", "test_source_sha256_rechecked"}),
    }
)
ROUTE_EXPECTATION_KEYS = frozenset(
    {"accepted_as_route_pass", "exact_command", "expectation_state", "expected_result", "normative_route_basis", "status"}
)
CONFLICT_OBJECT_KEYS = frozenset(
    {
        frozenset({"kind", "note"}),
        frozenset({"function_or_binding", "kind", "line", "note", "test_source_path"}),
        frozenset({"kind", "ledger_anchor_sha256", "live_source_sha256_at_review", "treatment"}),
        frozenset({"current_live_sha256", "kind", "prior_semantic_snapshot_sha256", "treatment"}),
        frozenset({"current_live_sha256", "kind", "r2_locator_sha256", "treatment"}),
    }
)
DIRECT_CONTRACT_KEYS = frozenset(
    {
        frozenset({"contract", "exact_source_pin_status", "file", "function", "line_refs", "sha256_current"}),
        frozenset({"contract", "exact_source_pin_status", "file", "function", "lines", "sha256"}),
        frozenset({"contract", "exact_source_sha256_pin", "file", "function", "lines", "sha256"}),
        frozenset({"contract", "file", "function", "line", "source_pin_status", "test_file_sha256"}),
        frozenset({"contract", "file", "function", "function_line", "line", "source_pin_status", "test_file_sha256"}),
        frozenset({"contract", "file", "file_sha256", "function_or_record", "line", "line_range", "source_pin_status"}),
        frozenset({"contract", "file", "file_sha256", "function_or_record", "line", "source_pin_status"}),
        frozenset({"contract", "file", "file_sha256", "function", "line", "source_pin_status"}),
        frozenset({"contract", "line_ref", "source_pin_status", "test_function", "test_path", "test_sha256"}),
        frozenset({"direct_contract", "line_refs", "source_pin_status", "test_file", "test_file_sha256", "test_function"}),
        frozenset({"direct_contract", "exact_source_pin_status", "file", "function", "lines", "sha256_current"}),
        frozenset({"authority_note", "fixture_reference_line", "function", "function_start_line", "source_pin_status", "test_contract_scope", "test_source", "test_source_sha256"}),
    }
)
FROZEN_DIRECT_CONTRACT_KEYS: dict[str, frozenset[frozenset[str]]] = {
    "independent_rereview_001_v1": frozenset({frozenset({
        "accepted", "exact_command_not_run", "exact_source_sha256_pin", "implementation_status",
        "oracle", "source_fixture_path", "source_fixture_sha256", "status",
    })}),
    "independent_adjudication_002_v1": frozenset({frozenset({
        "acceptance", "assertion_oracle", "command", "contract_status", "execution_status",
        "existing_test_source_refs", "fixture", "test_sources_used_as_language_authority",
    })}),
    "independent_source_review_003_v1": frozenset({frozenset({
        "acceptance", "contract_status", "exact_command", "existing_direct_test_references",
        "existing_test_reference_authority", "kind", "oracle", "source_locator", "source_sha256",
    })}),
    "independent_full_review_004_r2": frozenset({frozenset({
        "assertion_and_route_evidence", "execution_status", "fixture_case_evidence", "is_language_authority",
        "reference_line", "reference_text", "test_command_exact_candidate", "test_contract_status",
        "test_function", "test_function_line", "test_source_changed_since_initial_review",
        "test_source_hash_rechecked_stable", "test_source_path", "test_source_sha256_after_review",
        "test_source_sha256_current", "test_source_sha256_initial_snapshot",
    })}),
    "independent_adjudication_005_r2": frozenset({frozenset({
        "acceptance", "command", "execution_status", "oracle", "route_expectation", "source_path",
        "source_sha256", "status", "working_directory",
    })}),
    "independent_rereview_006_b1": REREVIEW_DIRECT_CONTRACT_KEYS,
    "independent_adjudication_007_v1": frozenset({frozenset({
        "accepted", "contract_source", "default_route", "execution_status", "explicit_mir_route", "note",
        "source_binding", "source_check", "status",
    })}),
    "independent_adjudication_008_r2": frozenset({
        frozenset({"accepted", "command", "expected_oracle", "kind", "not_run", "source_pin_sha256", "status"}),
        frozenset({"accepted", "blocked_reason", "command", "expected_oracle", "kind", "not_run", "source_pin_sha256", "status"}),
    }),
    "independent_adjudication_009_r2": frozenset({frozenset({
        "accepted", "command", "existing_direct_test_source", "implementation_status", "kind", "oracle",
        "source_pin_status", "source_sha256_pin", "status",
    })}),
}
FROZEN_RELATED_TEST_KEYS = frozenset({
    frozenset({"acceptance", "assertion", "direct_oracle", "function", "line", "mimi_test_runner_function", "status", "test_source_path", "test_source_sha256"}),
    frozenset({"direct_contract", "function_or_region", "path", "sha256_current", "source_pin_status", "used_as_language_authority"}),
})
FROZEN_ROUTE_KEYS: dict[str, frozenset[frozenset[str]]] = {
    "independent_rereview_001_v1": frozenset({frozenset({"accepted", "backend_build_expectation", "command", "normative_basis", "source_gate_expectation", "status"})}),
    "independent_adjudication_002_v1": frozenset({frozenset({"accepted", "command", "expected_source_outcome", "route_expectation", "route_selection_normatively_specified", "status"})}),
    "independent_source_review_003_v1": frozenset({frozenset({"documented_route", "expected_route", "reason", "source_specific_outcome"})}),
    "independent_full_review_004_r2": frozenset({frozenset({"candidate", "execution_status", "reason"})}),
    "independent_adjudication_005_r2": frozenset(),
    "independent_rereview_006_b1": frozenset({ROUTE_EXPECTATION_KEYS}),
    "independent_adjudication_007_v1": frozenset({frozenset({"accepted", "basis", "exact_command", "expected", "status"})}),
    "independent_adjudication_008_r2": frozenset({
        frozenset({"acceptance", "basis", "command", "expectation", "receipt_command", "status"}),
        frozenset({"acceptance", "basis", "command", "expectation", "known_shape", "receipt_command", "status"}),
        frozenset({"acceptance", "basis", "command", "conditional_route_class", "expectation", "status"}),
    }),
    "independent_adjudication_009_r2": frozenset({
        frozenset({"accepted_as_route_pass", "command", "execution_status", "expected_disposition_if_frontend_accepts", "route_basis", "route_reason", "status"}),
        frozenset({"accepted_as_route_pass", "command", "execution_status", "expected_disposition_if_frontend_accepts", "route_basis", "route_reason", "route_rejection_is_not_source_semantic_classification", "status"}),
    }),
}


class CensusError(ValueError):
    """Input or output violated the strict full-census contract."""


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def _pairs_no_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise CensusError(f"duplicate JSON object key: {key!r}")
        result[key] = value
    return result


def _reject_json_constant(value: str) -> Any:
    raise CensusError(f"non-JSON numeric constant: {value}")


def strict_json_loads(raw: bytes, label: str) -> Any:
    try:
        return json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_pairs_no_duplicates,
            parse_constant=_reject_json_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise CensusError(f"{label}: invalid UTF-8 JSON: {error}") from error


def canonical_repo_path(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise CensusError(f"{label}: expected a nonempty relative POSIX path")
    if "\\" in value or "\x00" in value:
        raise CensusError(f"{label}: backslash or NUL in path {value!r}")
    pure = PurePosixPath(value)
    canonical = pure.as_posix()
    if pure.is_absolute() or ".." in pure.parts or canonical in {"", "."}:
        raise CensusError(f"{label}: unsafe path {value!r}")
    if value != canonical:
        raise CensusError(f"{label}: noncanonical path alias {value!r} -> {canonical!r}")
    return canonical


@dataclass(frozen=True)
class Locator:
    index: int
    path: str
    sha256: str
    r2_sha256: str


@dataclass(frozen=True)
class ParsedShardRow:
    inventory_index: int
    source_path: str
    source_sha256_current: str
    source_class: str
    semantic_basis: str
    normative_basis: tuple[Any, ...]
    direct_test_contracts: tuple[Any, ...]
    direct_test_gaps: tuple[Any, ...]
    conflicts_and_limits: tuple[Any, ...]
    source_evidence_lines: tuple[dict[str, Any], ...]
    source_non_normative_comments: tuple[dict[str, Any], ...]
    route_expectations: Mapping[str, Mapping[str, Any]]
    review_schema: str
    source_hash_provenance: Mapping[str, Any]
    raw_row: Mapping[str, Any]


@dataclass
class BoundFileRegistry:
    repo_root: Path
    by_path: dict[str, dict[str, Any]]
    by_inode: dict[tuple[int, int], str]

    @classmethod
    def create(cls, repo_root: Path) -> "BoundFileRegistry":
        if repo_root.is_symlink() or not repo_root.is_dir():
            raise CensusError(f"repository root must be a non-symlink directory: {repo_root}")
        return cls(repo_root=repo_root.resolve(strict=True), by_path={}, by_inode={})

    def read(self, relative: str, role: str) -> tuple[bytes, str]:
        canonical = canonical_repo_path(relative, role)
        path = self.repo_root.joinpath(*PurePosixPath(canonical).parts)
        cursor = self.repo_root
        for component in PurePosixPath(canonical).parts:
            cursor = cursor / component
            try:
                mode = cursor.lstat().st_mode
            except FileNotFoundError as error:
                raise CensusError(f"{role}: missing referenced file {canonical!r}") from error
            if stat.S_ISLNK(mode):
                raise CensusError(f"{role}: symlink path is forbidden: {canonical!r}")
        if not path.is_file():
            raise CensusError(f"{role}: referenced path is not a regular file: {canonical!r}")
        info = path.stat()
        if not stat.S_ISREG(info.st_mode):
            raise CensusError(f"{role}: referenced path is not regular: {canonical!r}")
        inode = (info.st_dev, info.st_ino)
        prior = self.by_inode.get(inode)
        if prior is not None and prior != canonical:
            raise CensusError(f"{role}: path alias {canonical!r} refers to same file as {prior!r}")
        self.by_inode[inode] = canonical
        data = path.read_bytes()
        digest = sha256_bytes(data)
        record = self.by_path.setdefault(
            canonical,
            {"path": canonical, "sha256": digest, "roles": []},
        )
        if record["sha256"] != digest:
            raise CensusError(f"{role}: file changed while reading {canonical!r}")
        if role not in record["roles"]:
            record["roles"].append(role)
            record["roles"].sort()
        return data, digest

    def snapshots(self) -> dict[str, str]:
        result: dict[str, str] = {}
        for relative, record in sorted(self.by_path.items()):
            _, digest = self.read(relative, "snapshot_recheck")
            if digest != record["sha256"]:
                raise CensusError(f"bound file hash drift: {relative}")
            result[relative] = digest
        return result


def read_external_regular(path: Path, label: str) -> bytes:
    try:
        mode = path.lstat().st_mode
    except FileNotFoundError as error:
        raise CensusError(f"{label}: missing input {path}") from error
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
        raise CensusError(f"{label}: input must be a non-symlink regular file: {path}")
    return path.read_bytes()


def parse_jsonl(raw: bytes, label: str) -> list[dict[str, Any]]:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise CensusError(f"{label}: JSONL is not UTF-8") from error
    result: list[dict[str, Any]] = []
    for line_number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            raise CensusError(f"{label}:{line_number}: blank JSONL line")
        value = strict_json_loads(line.encode("utf-8"), f"{label}:{line_number}")
        if not isinstance(value, dict):
            raise CensusError(f"{label}:{line_number}: expected JSON object")
        result.append(value)
    if not result:
        raise CensusError(f"{label}: empty JSONL")
    return result


def _strict_int(value: Any, label: str, minimum: int = 1) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        raise CensusError(f"{label}: invalid integer {value!r}")
    return value


def _strict_sha(value: Any, label: str) -> str:
    if not isinstance(value, str) or not SHA256_RE.fullmatch(value):
        raise CensusError(f"{label}: expected lowercase SHA-256 hex")
    return value


def read_locator_roster(
    raw: bytes,
    registry: BoundFileRegistry,
    label: str = "current locator inventory",
    refresh_allowlist: Mapping[int, Mapping[str, str]] = APPROVED_LOCATOR_REFRESHES,
) -> tuple[list[Locator], list[dict[str, Any]]]:
    document = strict_json_loads(raw, label)
    if not isinstance(document, dict) or not isinstance(document.get("records"), list):
        raise CensusError(f"{label}: expected JSON object with records[]")
    rows = document["records"]
    locators: list[Locator] = []
    seen_indices: set[int] = set()
    seen_paths: set[str] = set()
    refreshes: list[dict[str, Any]] = []
    for offset, row in enumerate(rows, 1):
        if not isinstance(row, dict):
            raise CensusError(f"{label}: records[{offset - 1}] must be an object")
        index = _strict_int(row.get("input_index"), f"{label}:records[{offset - 1}].input_index")
        path = canonical_repo_path(row.get("path"), f"{label}:records[{offset - 1}].path")
        prior_digest = _strict_sha(row.get("sha256"), f"{label}:records[{offset - 1}].sha256")
        # Deliberately project only current inventory identity/hash fields.
        # Legacy expectations, semantic snapshots, route metadata, buildability,
        # and execution observations are not consulted or copied.
        if index in seen_indices:
            raise CensusError(f"{label}: duplicate inventory index {index}")
        if path in seen_paths:
            raise CensusError(f"{label}: duplicate/aliased source path {path!r}")
        seen_indices.add(index)
        seen_paths.add(path)
        _, live_digest = registry.read(path, "source_locator_live_refresh")
        if prior_digest != live_digest:
            approved = refresh_allowlist.get(index)
            actual_refresh = {"path": path, "r2_sha256": prior_digest, "live_sha256": live_digest}
            if approved is None or dict(approved) != actual_refresh:
                raise CensusError(
                    f"{label}: unapproved source hash drift at index {index}: "
                    f"path={path!r} r2={prior_digest} live={live_digest}"
                )
            refreshes.append({"inventory_index": index, **actual_refresh, "status": "authorized_live_refresh"})
        elif index in refresh_allowlist:
            # Approved stale locator rows are expected to remain stale until
            # that old inventory is refreshed; never silently ignore a changed
            # source state at any authorized row.
            approved = refresh_allowlist[index]
            if approved["path"] != path or approved["r2_sha256"] != prior_digest or approved["live_sha256"] != live_digest:
                raise CensusError(f"{label}: approved refresh record no longer matches index {index}")
        locators.append(Locator(index=index, path=path, sha256=live_digest, r2_sha256=prior_digest))
    if len(locators) != COUNT or seen_indices != set(range(1, COUNT + 1)):
        missing = sorted(set(range(1, COUNT + 1)) - seen_indices)
        extra = sorted(seen_indices - set(range(1, COUNT + 1)))
        raise CensusError(f"locator roster must be exact 1..{COUNT}; missing={missing[:8]} extra={extra[:8]}")
    return sorted(locators, key=lambda item: item.index), sorted(refreshes, key=lambda item: item["inventory_index"])


def _ignored_locator_field_stats(records: Sequence[Mapping[str, Any]]) -> tuple[dict[str, int], dict[str, int]]:
    """Count ignored r2 record fields without retaining or interpreting values."""
    occurrence_counts: Counter[str] = Counter()
    null_counts: Counter[str] = Counter()

    def visit(value: Any, prefix: str) -> None:
        if value is None:
            null_counts[prefix] += 1
            return
        if isinstance(value, dict):
            for key, child in value.items():
                child_path = f"{prefix}.{key}" if prefix else key
                occurrence_counts[child_path] += 1
                visit(child, child_path)
        elif isinstance(value, list):
            for child in value:
                if isinstance(child, (dict, list)):
                    visit(child, f"{prefix}[]")
                elif child is None:
                    null_counts[f"{prefix}[]"] += 1

    for row in records:
        for key, value in row.items():
            if key in {*LOCATOR_IDENTITY_FIELDS, LOCATOR_PROVENANCE_HASH_FIELD}:
                continue
            occurrence_counts[key] += 1
            visit(value, key)
    return dict(sorted(occurrence_counts.items())), dict(sorted(null_counts.items()))


def locator_projection_audit(raw: bytes) -> dict[str, Any]:
    """Return a value-free audit of the r2 fields excluded from locator identity."""
    document = strict_json_loads(raw, "r2 locator projection audit")
    if not isinstance(document, dict) or not isinstance(document.get("records"), list):
        raise CensusError("r2 locator projection audit: expected records[]")
    records = document["records"]
    if any(not isinstance(row, dict) for row in records):
        raise CensusError("r2 locator projection audit: every record must be an object")
    top_level_ignored = sorted(set(document) - {"records"})
    direct_ignored = sorted(
        {key for row in records for key in row}
        - {*LOCATOR_IDENTITY_FIELDS, LOCATOR_PROVENANCE_HASH_FIELD}
    )
    occurrence_counts, null_counts = _ignored_locator_field_stats(records)
    return {
        "schema": "actor-census-r2-locator-projection-audit-v1",
        "r2_input_sha256": sha256_bytes(raw),
        "record_count": len(records),
        "identity_fields": list(LOCATOR_IDENTITY_FIELDS),
        "r2_hash_field": LOCATOR_PROVENANCE_HASH_FIELD,
        "r2_hash_use": "provenance_only; current source hash is recomputed from live repository bytes",
        "live_hash_algorithm": "SHA-256 over current source bytes",
        "projection_rule": "Only records[].input_index and records[].path define identity; source SHA-256 is recomputed live. All other r2 fields are excluded from semantic classification, route expectation, test outcome, and acceptance.",
        "ignored_top_level_fields": top_level_ignored,
        "ignored_record_fields": direct_ignored,
        "ignored_record_field_occurrence_counts": occurrence_counts,
        "ignored_record_null_value_counts": null_counts,
        "expectation_semantics_consumed": False,
        "route_metadata_consumed": False,
        "status_or_execution_metadata_consumed": False,
        "runtime_or_bootstrap_observations_consumed": False,
    }


def _check_exact_keys(value: Mapping[str, Any], keys: frozenset[str], label: str) -> None:
    actual = frozenset(value)
    if actual != keys:
        unknown = sorted(actual - keys)
        missing = sorted(keys - actual)
        raise CensusError(f"{label}: schema keys mismatch; unknown={unknown}, missing={missing}")


def _frozen_schema_for_index(index: int) -> tuple[str, frozenset[str]]:
    for schema_index, ((start, end), schema) in enumerate(zip(SHARD_RANGES, FROZEN_SOURCE_SCHEMA_KEYS, strict=True), 1):
        if start <= index <= end:
            return schema
    raise CensusError(f"inventory index outside frozen shard ranges: {index}")


def _source_line_number(value: Any, label: str) -> int:
    if isinstance(value, int) and not isinstance(value, bool) and value >= 1:
        return value
    if isinstance(value, str):
        match = re.fullmatch(r"L?(\d+)", value)
        if match:
            return _strict_int(int(match.group(1)), label)
    raise CensusError(f"{label}: expected positive line number, got {value!r}")


def _collect_line_text_anchors(value: Any, label: str) -> list[dict[str, Any]]:
    anchors: list[dict[str, Any]] = []

    def visit(item: Any) -> None:
        if isinstance(item, dict):
            line_key = "line" if "line" in item else "line_ref" if "line_ref" in item else None
            if line_key is not None and isinstance(item.get("text"), str):
                anchors.append({
                    "line": _source_line_number(item[line_key], f"{label}: line anchor"),
                    "text": item["text"],
                })
            for child in item.values():
                visit(child)
        elif isinstance(item, list):
            for child in item:
                visit(child)

    visit(value)
    unique: list[dict[str, Any]] = []
    seen: set[tuple[int, str]] = set()
    for anchor in anchors:
        pair = (anchor["line"], anchor["text"])
        if pair not in seen:
            seen.add(pair)
            unique.append(anchor)
    return unique


def _route_command_tokens(command: Any, source_path: str, axis: str, label: str) -> str:
    if not isinstance(command, str) or not command:
        raise CensusError(f"{label}: route command must be a nonempty string")
    try:
        tokens = shlex.split(command, posix=True)
    except ValueError as error:
        raise CensusError(f"{label}: malformed route command") from error
    expected_path = source_path
    allowed = [
        ["mimi", "build", expected_path],
    ] if axis == "default" else [
        ["mimi", "build", expected_path, "--mir"],
        ["mimi", "build", "--mir", expected_path],
    ]
    if tokens not in allowed:
        raise CensusError(f"{label}: route command tokens are not an exact source-bound {axis} build command")
    return command


def _route_normative_items(route: Mapping[str, Any]) -> list[Any]:
    items: list[Any] = []
    for key in ("normative_route_basis", "normative_basis", "route_basis", "basis", "route_policy_basis", "documented_route"):
        if key not in route:
            continue
        value = route[key]
        if value is None:
            continue
        if isinstance(value, list):
            items.extend(value)
        elif isinstance(value, (dict, str)):
            items.append(value)
        else:
            raise CensusError(f"route field {key} must be an object, string, list, or null")
    return items


def _parse_route_contract(route: Mapping[str, Any], source_path: str, axis: str, label: str) -> dict[str, Any]:
    if not isinstance(route, dict):
        raise CensusError(f"{label}: route contract must be an object")
    command_value = route.get("exact_command", route.get("command", route.get("expected_route")))
    if command_value is None:
        command_value = f"mimi build {source_path}" + (" --mir" if axis == "explicit_mir" else "")
    command = _route_command_tokens(command_value, source_path, axis, label)
    for key in ("status", "execution_status", "review_status"):
        if key not in route:
            continue
        value = route[key]
        if not isinstance(value, str) or not (
            value == "not_run"
            or value.startswith("not_run_")
            or value.startswith("blocked_by_")
        ):
            raise CensusError(f"{label}: route status must remain not_run or explicitly blocked")
    for key in ("accepted", "acceptance", "accepted_as_route_pass"):
        if key in route and route[key] is not False:
            raise CensusError(f"{label}: route acceptance field {key} must be JSON false")
    state = route.get("expectation_state", "candidate_route_contract_recorded")
    if state not in {
        "blocked_unresolved",
        "source_error_preempts_route",
        "valid_source_route_unassessed",
        "candidate_route_contract_recorded",
    }:
        raise CensusError(f"{label}: unsupported route expectation_state {state!r}")
    return {
        "exact_command": command,
        "expectation_state": state,
        "expected_result": json.dumps(route, ensure_ascii=False, sort_keys=True, separators=(",", ":")),
        "status": "not_run",
        "accepted_as_route_pass": False,
        "normative_route_basis": _route_normative_items(route),
    }


def _frozen_route_contracts(row: Mapping[str, Any], source_path: str, schema_name: str, label: str) -> dict[str, Mapping[str, Any]]:
    raw_axes: dict[str, Any] = {}
    if schema_name == "independent_adjudication_002_v1":
        routes = row["route_expectation"]
        if not isinstance(routes, dict) or set(routes) != {"default", "explicit_mir"}:
            raise CensusError(f"{label}: route_expectation must have exact default/explicit_mir axes")
        raw_axes = {"default": routes["default"], "explicit_mir": routes["explicit_mir"]}
    elif schema_name == "independent_adjudication_007_v1":
        if row["route_expectations"] is None:
            raise CensusError(f"{label}: route_expectations cannot be null")
        routes = row["route_expectations"]
        if not isinstance(routes, dict) or set(routes) != {"default", "explicit_mir"}:
            raise CensusError(f"{label}: route_expectations must have exact default/explicit_mir axes")
        raw_axes = routes
    else:
        for axis, key in (
            ("default", "default_route_expectation"),
            ("explicit_mir", "explicit_mir_route_expectation"),
        ):
            if key in row:
                raw_axes[axis] = row[key]
        if "mir_route_expectation" in row:
            raw_axes["explicit_mir"] = row["mir_route_expectation"]
    parsed: dict[str, Mapping[str, Any]] = {}
    for axis, raw in raw_axes.items():
        if not isinstance(raw, dict):
            raise CensusError(f"{label}:{axis}: route contract must be an object")
        allowed_schemas = FROZEN_ROUTE_KEYS[schema_name]
        if frozenset(raw) not in allowed_schemas:
            raise CensusError(f"{label}:{axis}: unknown route schema {sorted(raw)}")
        parsed[axis] = _parse_route_contract(raw, source_path, axis, f"{label}:{axis}")
    return parsed


def _validate_frozen_contract_shape(value: Any, schema_name: str, label: str) -> list[Mapping[str, Any]]:
    if value is None:
        return []
    records = value if isinstance(value, list) else [value]
    allowed = FROZEN_DIRECT_CONTRACT_KEYS[schema_name]
    checked: list[Mapping[str, Any]] = []
    for offset, record in enumerate(records, 1):
        item_label = f"{label}[{offset}]"
        if not isinstance(record, dict) or frozenset(record) not in allowed:
            keys = sorted(record) if isinstance(record, dict) else type(record).__name__
            raise CensusError(f"{item_label}: unknown direct-test contract schema {keys}")
        checked.append(record)
    return checked


def _validate_unaccepted_nested(value: Any, label: str) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            child_label = f"{label}.{key}"
            if key in {"accepted", "acceptance", "accepted_as_route_pass"} and child is not False:
                raise CensusError(f"{child_label} must be JSON false")
            if key == "execution_status":
                if not isinstance(child, str) or not (
                    child == "not_run" or child.startswith("not_run_") or child.startswith("blocked_")
                ):
                    raise CensusError(f"{child_label} must remain not_run or explicitly blocked")
            elif key == "review_status" and child != "not_run":
                raise CensusError(f"{child_label} must remain not_run")
            elif key == "status":
                allowed_nested_statuses = {
                    "not_run", "decidable_candidate", "blocked_unresolved", "blocked_unresolved_normative_oracle",
                    "source_comment_non_normative_not_used_as_authority", "proposed_not_implemented",
                    "proposed_unimplemented", "existing_source_assertion_not_executed",
                }
                if not isinstance(child, str) or not (
                    child in allowed_nested_statuses or child.startswith("not_run_") or child.startswith("blocked_")
                ):
                    raise CensusError(f"{child_label} has an unknown candidate status")
            _validate_unaccepted_nested(child, child_label)
    elif isinstance(value, list):
        for offset, child in enumerate(value, 1):
            _validate_unaccepted_nested(child, f"{label}[{offset}]")


def _canonicalize_frozen_row(
    row: Mapping[str, Any],
    *,
    expected_index: int,
    repo_root: Path,
    label: str,
) -> ParsedShardRow:
    schema_name, schema_keys = _frozen_schema_for_index(expected_index)
    _check_exact_keys(row, schema_keys, f"{label}:{expected_index}")
    index = _strict_int(row.get("inventory_index"), f"{label}:{expected_index}: inventory_index")
    if index != expected_index:
        raise CensusError(f"{label}: row index {index} does not match expected roster position {expected_index}")
    if row.get("status") != "not_run":
        raise CensusError(f"{label}:{index}: status must be exactly 'not_run'")
    for key in ("accepted", "accepted_as_route_pass", "bootstrap_or_runtime_observation_used"):
        if row.get(key) is not False:
            raise CensusError(f"{label}:{index}: {key} must be JSON false")
    executed_key = (
        "mimi_or_cargo_or_tests_or_build_executed"
        if "mimi_or_cargo_or_tests_or_build_executed" in row
        else "mimi_or_cargo_or_test_or_build_executed"
    )
    if row[executed_key] is not False:
        raise CensusError(f"{label}:{index}: {executed_key} must be JSON false")

    source_path = canonical_repo_path(row.get("source_path"), f"{label}:{index}: source_path")
    if "source_absolute_path" in row:
        absolute = row["source_absolute_path"]
        expected_absolute = repo_root.joinpath(*PurePosixPath(source_path).parts).resolve(strict=True)
        if not isinstance(absolute, str) or not Path(absolute).is_absolute() or Path(absolute).resolve(strict=True) != expected_absolute:
            raise CensusError(f"{label}:{index}: source_absolute_path does not identify the projected source path")

    class_keys = [
        key for key in ("candidate_class", "candidate_semantic_class", "expected_class", "source_semantic_candidate_class")
        if key in row
    ]
    if not class_keys:
        raise CensusError(f"{label}:{index}: source row has no candidate semantic class")
    classes = [row[key] for key in class_keys]
    if any(not isinstance(value, str) or value not in SOURCE_CLASSES for value in classes):
        raise CensusError(f"{label}:{index}: unknown source class in {classes!r}")
    if len(set(classes)) != 1:
        raise CensusError(f"{label}:{index}: source semantic candidate/class fields conflict")

    current_hash_key = {
        "independent_rereview_001_v1": "source_sha256_live_final_recheck",
        "independent_adjudication_002_v1": "source_sha256_rechecked_after_review",
        "independent_source_review_003_v1": "source_sha256_review_end",
        "independent_full_review_004_r2": "source_sha256_after_review",
        "independent_adjudication_005_r2": "source_sha256_review_end",
        "independent_rereview_006_b1": "source_sha256_current",
        "independent_adjudication_007_v1": "source_sha256_rechecked_before_finalization",
        "independent_adjudication_008_r2": "source_sha256_review_end",
        "independent_adjudication_009_r2": "source_sha256_recheck",
    }[schema_name]
    current_sha = _strict_sha(row[current_hash_key], f"{label}:{index}:{current_hash_key}")
    source_hash_values: dict[str, str] = {}
    for key, value in row.items():
        if key.startswith("source_sha256_"):
            source_hash_values[key] = _strict_sha(value, f"{label}:{index}:{key}")
    current_hash_fields = {
        "independent_rereview_001_v1": ("source_sha256_live_at_review", "source_sha256_live_final_recheck", "source_sha256_roster_anchor"),
        "independent_adjudication_002_v1": ("source_sha256_current", "source_sha256_rechecked_after_review"),
        "independent_source_review_003_v1": ("source_sha256_current", "source_sha256_review_start", "source_sha256_review_end"),
        "independent_full_review_004_r2": ("source_sha256_current", "source_sha256_after_review"),
        "independent_adjudication_005_r2": ("source_sha256_current", "source_sha256_review_end"),
        "independent_rereview_006_b1": ("source_sha256_current", "source_sha256_at_review"),
        "independent_adjudication_007_v1": ("source_sha256_current", "source_sha256_rechecked_before_finalization", "source_sha256_roster_anchor"),
        "independent_adjudication_008_r2": ("source_sha256_current", "source_sha256_review_end"),
        "independent_adjudication_009_r2": ("source_sha256_current", "source_sha256_recheck"),
    }[schema_name]
    for key in current_hash_fields:
        if source_hash_values.get(key) != current_sha:
            raise CensusError(f"{label}:{index}: current source hash fields disagree at {key}")
    for key, value in row.items():
        if key in {"source_hash_drift", "source_hash_drift_from_roster_anchor", "source_drift_during_review", "source_hash_drift_during_review"}:
            if value is not False:
                raise CensusError(f"{label}:{index}: {key} must be false for a stable source review")
        elif key in {"source_hash_rechecked_stable", "source_hash_stable_during_review"} and value is not True:
            raise CensusError(f"{label}:{index}: {key} must be true")
    if "source_hash_status" in row and row["source_hash_status"] != "matched":
        raise CensusError(f"{label}:{index}: source_hash_status is not matched")
    for key in ("source_hash_differs_from_prior_semantic_snapshot", "prior_semantic_snapshot_drift", "r2_locator_drift"):
        if key in row and not isinstance(row[key], bool):
            raise CensusError(f"{label}:{index}: {key} must be boolean")
    prior_snapshot_sha = source_hash_values.get("source_sha256_prior_semantic_snapshot")
    if prior_snapshot_sha is not None:
        prior_drift = prior_snapshot_sha != current_sha
        if row.get("source_hash_differs_from_prior_semantic_snapshot") is not prior_drift:
            raise CensusError(f"{label}:{index}: prior semantic snapshot drift flag conflicts with hashes")
        if row.get("prior_semantic_snapshot_drift") is not prior_drift:
            raise CensusError(f"{label}:{index}: prior_semantic_snapshot_drift flag conflicts with hashes")
    r2_locator_sha = source_hash_values.get("source_sha256_r2_locator")
    if r2_locator_sha is not None and row.get("r2_locator_drift") is not (r2_locator_sha != current_sha):
        raise CensusError(f"{label}:{index}: r2 locator drift flag conflicts with hashes")

    semantic_key = "semantic_basis" if "semantic_basis" in row else "semantic_reason"
    semantic_basis = row[semantic_key]
    if not isinstance(semantic_basis, str) or not semantic_basis.strip():
        raise CensusError(f"{label}:{index}: semantic basis must be nonempty text")
    normative = row["normative_basis"]
    if not isinstance(normative, list):
        raise CensusError(f"{label}:{index}: normative_basis must be a list")
    for offset, item in enumerate(normative, 1):
        if isinstance(item, dict):
            keys = frozenset(item)
            if keys not in NORMATIVE_OBJECT_KEYS:
                raise CensusError(f"{label}:{index}: normative_basis[{offset}] has unknown schema {sorted(keys)}")
        elif not isinstance(item, str):
            raise CensusError(f"{label}:{index}: normative_basis[{offset}] must be text or a known citation object")

    direct_contracts: list[Any] = []
    for key in (
        "direct_test_contract", "direct_test_contracts", "direct_contracts",
        "existing_direct_test_contracts", "related_test_contract_sources",
    ):
        if key not in row:
            continue
        value = row[key]
        if key in {"existing_direct_test_contracts", "related_test_contract_sources"}:
            values = value if isinstance(value, list) else [value]
            for offset, contract in enumerate(values, 1):
                if not isinstance(contract, dict) or frozenset(contract) not in FROZEN_RELATED_TEST_KEYS:
                    raise CensusError(f"{label}:{index}:{key}[{offset}] has unknown direct-test reference schema")
                direct_contracts.append({"source_field": key, "contract": contract})
        else:
            direct_contracts.extend(
                {"source_field": key, "contract": item}
                for item in _validate_frozen_contract_shape(value, schema_name, f"{label}:{index}:{key}")
            )
    for key in ("direct_test_contract_summary", "proposed_direct_source_contract"):
        if key in row and not isinstance(row[key], dict):
            raise CensusError(f"{label}:{index}:{key} must be an object")
    for contract in direct_contracts:
        _validate_unaccepted_nested(contract, f"{label}:{index}:direct_contract")

    gaps: list[Any] = []
    for key in ("direct_test_gaps", "additional_gaps"):
        value = row.get(key, [])
        if not isinstance(value, list):
            raise CensusError(f"{label}:{index}: {key} must be a list")
        for offset, item in enumerate(value, 1):
            if isinstance(item, str):
                gaps.append(item)
            elif isinstance(item, dict) and set(item) == {"question", "acceptance"} and isinstance(item["question"], str) and item["acceptance"] is False:
                gaps.append(dict(item))
            else:
                raise CensusError(f"{label}:{index}: malformed {key}[{offset}]")
    for key in ("unresolved_normative_question", "blocked_question"):
        value = row.get(key)
        if value is not None:
            if not isinstance(value, str):
                raise CensusError(f"{label}:{index}: {key} must be string or null")
            if value and value not in gaps:
                gaps.append(value)

    conflicts: list[Any] = []
    for key in ("conflicts_and_gaps", "conflicts_and_limits", "source_specific_normative_gaps"):
        value = row.get(key, [])
        if not isinstance(value, list):
            raise CensusError(f"{label}:{index}: {key} must be a list")
        conflicts.extend(value)
    non_normative_comments: list[dict[str, Any]] = []
    if "non_normative_source_comments" in row:
        value = row["non_normative_source_comments"]
        if not isinstance(value, list):
            raise CensusError(f"{label}:{index}: non_normative_source_comments must be a list")
        for offset, comment in enumerate(value, 1):
            if not isinstance(comment, dict) or set(comment) != {"line", "text", "status"}:
                raise CensusError(f"{label}:{index}: malformed non-normative source comment {offset}")
            _strict_int(comment["line"], f"{label}:{index}: non-normative comment line")
            if not isinstance(comment["text"], str) or comment["status"] != "source_comment_non_normative_not_used_as_authority":
                raise CensusError(f"{label}:{index}: non-normative comment authority/status is invalid")
            non_normative_comments.append(dict(comment))
    if "source_line_count" in row:
        _strict_int(row["source_line_count"], f"{label}:{index}: source_line_count", minimum=0)

    evidence_value = None
    for key in ("source_line_evidence", "source_line_anchors", "source_evidence_lines", "source_anchor_lines", "source_evidence", "source_structure"):
        if key in row:
            evidence_value = row[key]
            break
    anchors = _collect_line_text_anchors(evidence_value, f"{label}:{index}: source evidence") if evidence_value is not None else []
    _validate_unaccepted_nested(row, f"{label}:{index}")
    route_expectations = _frozen_route_contracts(row, source_path, schema_name, f"{label}:{index}")
    provenance = {
        "review_schema": schema_name,
        "reported_source_hashes": source_hash_values,
        "source_hash_status": row.get("source_hash_status"),
        "source_hash_flags": {
            key: value for key, value in row.items()
            if key in {
                "source_hash_drift", "source_hash_drift_from_roster_anchor", "source_drift_during_review",
                "source_hash_drift_during_review", "source_hash_rechecked_stable", "source_hash_stable_during_review",
                "source_hash_differs_from_prior_semantic_snapshot", "prior_semantic_snapshot_drift", "r2_locator_drift",
            }
        },
        "prior_semantic_snapshot_sha256": row.get("source_sha256_prior_semantic_snapshot"),
        "r2_locator_sha256": row.get("source_sha256_r2_locator"),
    }
    return ParsedShardRow(
        inventory_index=index,
        source_path=source_path,
        source_sha256_current=current_sha,
        source_class=classes[0],
        semantic_basis=semantic_basis,
        normative_basis=tuple(normative),
        direct_test_contracts=tuple(direct_contracts),
        direct_test_gaps=tuple(gaps),
        conflicts_and_limits=tuple(conflicts),
        source_evidence_lines=tuple(anchors),
        source_non_normative_comments=tuple(non_normative_comments),
        route_expectations=route_expectations,
        review_schema=schema_name,
        source_hash_provenance=provenance,
        raw_row=row,
    )


def validate_source_row(row: Mapping[str, Any], shard_label: str) -> str:
    index = _strict_int(row.get("inventory_index"), f"{shard_label}: inventory_index")
    keys = frozenset(row)
    if keys == ROW_KEYS:
        variant = "basic"
    elif keys == REREVIEW_ROW_KEYS:
        variant = "rereview"
    else:
        expected = ROW_KEYS if "candidate_semantic_class" not in row else REREVIEW_ROW_KEYS
        _check_exact_keys(row, expected, f"{shard_label}:{index}")
        raise CensusError(f"{shard_label}:{index}: unsupported source-row schema")
    if not isinstance(row.get("source_path"), str):
        raise CensusError(f"{shard_label}:{index}: source_path must be a string")
    canonical_repo_path(row["source_path"], f"{shard_label}:{index}: source_path")
    _strict_sha(row.get("source_sha256_current"), f"{shard_label}:{index}: source_sha256_current")
    source_class = row.get("expected_class")
    if not isinstance(source_class, str) or source_class not in SOURCE_CLASSES:
        raise CensusError(f"{shard_label}:{index}: unknown source class {source_class!r}")
    if not isinstance(row.get("semantic_basis"), str) or not row["semantic_basis"].strip():
        raise CensusError(f"{shard_label}:{index}: semantic_basis must be a nonempty string")
    if row.get("status") != "not_run":
        raise CensusError(f"{shard_label}:{index}: status must be exactly 'not_run'")
    for key in (
        "accepted",
        "accepted_as_route_pass",
        "bootstrap_or_runtime_observation_used",
        "mimi_or_cargo_or_test_or_build_executed",
    ):
        if row.get(key) is not False:
            raise CensusError(f"{shard_label}:{index}: {key} must be the JSON boolean false")
    for key in ("conflicts_and_limits", "direct_test_gaps", "direct_test_contracts", "normative_basis"):
        if not isinstance(row.get(key), list):
            raise CensusError(f"{shard_label}:{index}: {key} must be a list")
    if variant == "basic" and any(not isinstance(item, str) for item in row["conflicts_and_limits"]):
        raise CensusError(f"{shard_label}:{index}: conflicts_and_limits must contain strings only")
    if variant == "rereview" and any(not isinstance(item, dict) for item in row["conflicts_and_limits"]):
        raise CensusError(f"{shard_label}:{index}: conflicts_and_limits must contain objects only")
    if any(not isinstance(item, str) for item in row["direct_test_gaps"]):
        raise CensusError(f"{shard_label}:{index}: direct_test_gaps must contain strings only")
    if variant == "rereview":
        if row["candidate_semantic_class"] != source_class:
            raise CensusError(f"{shard_label}:{index}: candidate_semantic_class conflicts with expected_class")
        review_sha = _strict_sha(row["source_sha256_at_review"], f"{shard_label}:{index}: source_sha256_at_review")
        prior_sha = _strict_sha(row["source_sha256_prior_semantic_snapshot"], f"{shard_label}:{index}: source_sha256_prior_semantic_snapshot")
        r2_sha = _strict_sha(row["source_sha256_r2_locator"], f"{shard_label}:{index}: source_sha256_r2_locator")
        current_sha = row["source_sha256_current"]
        if review_sha != current_sha or row["source_hash_drift_during_review"] is not False:
            raise CensusError(f"{shard_label}:{index}: review-time source hash is stale or drifted")
        prior_drift = prior_sha != current_sha
        if row["source_hash_differs_from_prior_semantic_snapshot"] is not prior_drift:
            raise CensusError(f"{shard_label}:{index}: prior semantic snapshot drift flag conflicts with hashes")
        if row["prior_semantic_snapshot_drift"] is not prior_drift:
            raise CensusError(f"{shard_label}:{index}: prior_semantic_snapshot_drift flag conflicts with hashes")
        if not isinstance(row["r2_locator_drift"], bool):
            raise CensusError(f"{shard_label}:{index}: r2_locator_drift must be boolean")
        if row["r2_locator_drift"] is not (r2_sha != current_sha):
            raise CensusError(f"{shard_label}:{index}: r2_locator_drift flag conflicts with hashes")
        for axis in ("default_route_expectation", "explicit_mir_route_expectation"):
            route = row[axis]
            if not isinstance(route, dict):
                raise CensusError(f"{shard_label}:{index}: {axis} must be an object")
            _check_exact_keys(route, ROUTE_EXPECTATION_KEYS, f"{shard_label}:{index}:{axis}")
            if route["status"] != "not_run" or route["accepted_as_route_pass"] is not False:
                raise CensusError(f"{shard_label}:{index}:{axis}: must remain not_run/unaccepted")
            if not isinstance(route["exact_command"], str) or not isinstance(route["expected_result"], str):
                raise CensusError(f"{shard_label}:{index}:{axis}: command/result must be strings")
            if route["expectation_state"] not in {
                "blocked_unresolved",
                "source_error_preempts_route",
                "valid_source_route_unassessed",
                "candidate_route_contract_recorded",
            }:
                raise CensusError(f"{shard_label}:{index}:{axis}: unsupported expectation_state")
            expected_command = f"mimi build {row['source_path']}"
            if axis == "explicit_mir_route_expectation":
                expected_command += " --mir"
            if route["exact_command"] != expected_command:
                raise CensusError(
                    f"{shard_label}:{index}:{axis}: exact command must be {expected_command!r}"
                )
            if not isinstance(route["normative_route_basis"], list):
                raise CensusError(f"{shard_label}:{index}:{axis}: normative_route_basis must be a list")
        for offset, item in enumerate(row["source_evidence_lines"], 1):
            if not isinstance(item, dict) or set(item) != {"line", "text"}:
                raise CensusError(f"{shard_label}:{index}: malformed source_evidence_lines[{offset}]")
            _strict_int(item["line"], f"{shard_label}:{index}: source evidence line")
            if not isinstance(item["text"], str):
                raise CensusError(f"{shard_label}:{index}: source evidence text must be a string")
        for offset, item in enumerate(row["conflicts_and_limits"], 1):
            if not isinstance(item, dict) or frozenset(item) not in CONFLICT_OBJECT_KEYS:
                raise CensusError(f"{shard_label}:{index}: unknown conflict schema at item {offset}")
            for key, value in item.items():
                if key == "line":
                    _strict_int(value, f"{shard_label}:{index}: conflicts_and_limits line")
                elif key.endswith("sha256") or key in {"live_source_sha256_at_review", "current_live_sha256"}:
                    _strict_sha(value, f"{shard_label}:{index}: conflicts_and_limits {key}")
                elif not isinstance(value, str):
                    raise CensusError(f"{shard_label}:{index}: conflicts_and_limits {key} must be string")
            if item["kind"] == "r2_locator_drift":
                if item.get("r2_locator_sha256") != r2_sha or item.get("current_live_sha256") != current_sha:
                    raise CensusError(f"{shard_label}:{index}: r2 drift conflict record disagrees with row hashes")
            if item["kind"] == "prior_semantic_snapshot_drift":
                if item.get("prior_semantic_snapshot_sha256") != prior_sha or item.get("current_live_sha256") != current_sha:
                    raise CensusError(f"{shard_label}:{index}: historical snapshot conflict disagrees with row hashes")
        for offset, contract in enumerate(row["direct_test_contracts"], 1):
            if not isinstance(contract, dict) or frozenset(contract) not in REREVIEW_DIRECT_CONTRACT_KEYS:
                raise CensusError(f"{shard_label}:{index}: unknown re-review direct-test schema at item {offset}")
            if "source_under_test" in contract:
                if contract["status"] != "not_run" or contract["accepted"] is not False:
                    raise CensusError(f"{shard_label}:{index}: source check contract must be not_run/unaccepted")
                if contract["source_under_test"] != row["source_path"] or contract["source_sha256"] != current_sha:
                    raise CensusError(f"{shard_label}:{index}: direct source contract identity/hash mismatch")
                if not isinstance(contract["exact_command"], str):
                    raise CensusError(f"{shard_label}:{index}: source check command must be a string")
                if contract["expected_oracle"] is not None and not isinstance(contract["expected_oracle"], str):
                    raise CensusError(f"{shard_label}:{index}: expected_oracle must be null or string")
                if "blocker" in contract and not isinstance(contract["blocker"], str):
                    raise CensusError(f"{shard_label}:{index}: source contract blocker must be string")
            else:
                _strict_int(contract["line"], f"{shard_label}:{index}: test contract line")
                _strict_sha(contract["test_source_sha256"], f"{shard_label}:{index}: test_source_sha256")
                if contract["test_source_sha256_rechecked"] != contract["test_source_sha256"]:
                    raise CensusError(f"{shard_label}:{index}: test source recheck conflicts with recorded test hash")
                if not all(isinstance(contract[key], str) for key in ("test_source_path", "function" if "function" in contract else "function_or_binding", "assertion", "source_pin_status")):
                    raise CensusError(f"{shard_label}:{index}: malformed existing test contract strings")
    return variant


def _normalize_normative_item(item: Any, registry: BoundFileRegistry, label: str) -> dict[str, Any]:
    raw_text: str
    file_refs: list[str] = []
    reported_hashes: dict[str, str] = {}
    if isinstance(item, str):
        raw_text = item
        for match in re.finditer(r"(?:README\.md|(?:docs|devdocs|src|tests|scripts)/[^\s,;()]+?\.(?:md|mimi|rs|toml|json|py|sh|txt))", item):
            candidate = match.group(0).rstrip(".,:)")
            if candidate not in file_refs:
                file_refs.append(candidate)
    elif isinstance(item, dict):
        keys = frozenset(item)
        if keys not in NORMATIVE_OBJECT_KEYS:
            raise CensusError(f"{label}: unknown normative-basis object schema {sorted(keys)}")
        for key, value in item.items():
            if key in {"line_start", "line_end"}:
                _strict_int(value, f"{label}:{key}")
            elif not isinstance(value, str):
                raise CensusError(f"{label}:{key}: normative-basis field must be text or a valid line number")
        path_key = "file" if "file" in item else "document" if "document" in item else "path" if "path" in item else None
        if path_key is not None:
            file_refs = [canonical_repo_path(item[path_key], label)]
        elif "reference" in item:
            file_refs = _file_reference_paths(registry, item["reference"])
        else:
            raise CensusError(f"{label}: normative-basis object has no document locator")
        hash_key = "sha256_current" if "sha256_current" in item else "sha256" if "sha256" in item else None
        if hash_key is not None and file_refs:
            reported_hashes[file_refs[0]] = _strict_sha(item[hash_key], f"{label}:{hash_key}")
        raw_text = json.dumps(item, ensure_ascii=False, sort_keys=True)
    elif isinstance(item, list):
        if len(item) != 4 or not isinstance(item[0], str) or any(not isinstance(value, str) for value in item[1:]):
            raise CensusError(f"{label}: only four-string citation tuples are supported")
        file_refs = [canonical_repo_path(item[0], label)]
        raw_text = " | ".join(item)
    else:
        raise CensusError(f"{label}: unsupported normative-basis item type {type(item).__name__}")

    bindings = []
    for relative in file_refs:
        canonical = canonical_repo_path(relative, label)
        _, digest = registry.read(canonical, "normative_reference")
        reported = reported_hashes.get(canonical)
        if reported is not None and reported != digest:
            raise CensusError(f"{label}: normative reference hash drift for {canonical}: reported {reported}, current {digest}")
        bindings.append({"path": canonical, "sha256_current_at_merge": digest})
    return {
        "reference": raw_text,
        "current_file_bindings": bindings,
        "review_time_file_hash_present_in_shard": bool(reported_hashes),
        "authority": "candidate_reference_only",
    }


def _contract_path_and_hash(contract: Mapping[str, Any], label: str) -> tuple[str, str]:
    keys = frozenset(contract)
    if keys not in DIRECT_CONTRACT_KEYS:
        raise CensusError(f"{label}: unknown direct-test contract schema {sorted(keys)}")
    if "test_source" in contract:
        path_key, hash_key = "test_source", "test_source_sha256"
    elif "test_path" in contract:
        path_key, hash_key = "test_path", "test_sha256"
    elif "test_file" in contract:
        path_key, hash_key = "test_file", "test_file_sha256"
    elif "file" in contract:
        path_key = "file"
        hash_key = next(
            key
            for key in ("file_sha256", "test_file_sha256", "sha256_current", "sha256")
            if key in contract
        )
    else:
        raise CensusError(f"{label}: direct-test record has no file locator")
    path = canonical_repo_path(contract[path_key], f"{label}:{path_key}")
    digest = _strict_sha(contract[hash_key], f"{label}:{hash_key}")
    for key, value in contract.items():
        if key in {"line", "function_line", "function_start_line", "fixture_reference_line"}:
            _strict_int(value, f"{label}:{key}")
        elif key == "exact_source_sha256_pin" and not isinstance(value, bool):
            raise CensusError(f"{label}:{key} must be boolean")
        elif not isinstance(value, (str, int, bool)) or isinstance(value, float):
            raise CensusError(f"{label}:{key} has invalid scalar type")
    return path, digest


def _file_reference_paths(registry: BoundFileRegistry, text: str) -> list[str]:
    paths: list[str] = []
    for match in re.finditer(r"(?:README\.md|(?:docs|devdocs|src|tests|scripts)/[^\s,;()]+?\.(?:md|mimi|rs|toml|json|py|sh|txt))", text):
        value = match.group(0).rstrip(".,:)")
        canonical = canonical_repo_path(value, "direct test prose locator")
        if canonical not in paths:
            paths.append(canonical)
    return paths


def _load_shard_rows(
    shard_paths: Sequence[Path],
    *,
    repo_root: Path | None = None,
    frozen_schema: bool = False,
) -> tuple[dict[int, ParsedShardRow | dict[str, Any]], dict[Path, bytes]]:
    if len(shard_paths) != len(SHARD_RANGES):
        raise CensusError(f"exactly {len(SHARD_RANGES)} full-review shard files are required")
    by_index: dict[int, dict[str, Any]] = {}
    snapshots: dict[Path, bytes] = {}
    seen_files: set[tuple[int, int]] = set()
    for expected_range, path in zip(SHARD_RANGES, shard_paths, strict=True):
        raw = read_external_regular(path, "full-review shard")
        stat_result = path.stat()
        inode = (stat_result.st_dev, stat_result.st_ino)
        if inode in seen_files:
            raise CensusError(f"duplicate/aliased shard file: {path}")
        seen_files.add(inode)
        snapshots[path] = raw
        rows = parse_jsonl(raw, str(path))
        expected_indices = set(range(expected_range[0], expected_range[1] + 1))
        shard_indices: set[int] = set()
        for row in rows:
            index = _strict_int(row.get("inventory_index"), f"{path}: inventory_index")
            if index not in expected_indices:
                raise CensusError(
                    f"{path}: row {index} is outside assigned range {expected_range[0]}..{expected_range[1]}"
                )
            if index in shard_indices or index in by_index:
                raise CensusError(f"duplicate roster index {index}")
            shard_indices.add(index)
            if frozen_schema:
                if repo_root is None:
                    raise CensusError("frozen schema loading requires repository root")
                by_index[index] = _canonicalize_frozen_row(
                    row,
                    expected_index=index,
                    repo_root=repo_root,
                    label=str(path),
                )
            else:
                validate_source_row(row, str(path))
                by_index[index] = row
        if shard_indices != expected_indices:
            missing = sorted(expected_indices - shard_indices)
            extra = sorted(shard_indices - expected_indices)
            raise CensusError(f"{path}: shard range mismatch; missing={missing[:8]}, extra={extra[:8]}")
    if set(by_index) != set(range(1, COUNT + 1)):
        raise CensusError("full-review shards do not cover exact roster 1..1187")
    return by_index, snapshots


def _stable_input_hash(path: Path, label: str) -> str:
    raw = read_external_regular(path, label)
    return sha256_bytes(raw)


DIRECT_PATH_KEYS = frozenset({
    "file", "path", "test_path", "test_file", "test_file_path", "test_source", "test_source_path",
    "test_source_ref", "source_fixture_path", "source_under_test", "source_locator", "source_path",
    "existing_direct_test_source", "existing_test_source", "fixture",
})
HISTORICAL_HASH_MARKERS = ("prior", "initial", "snapshot", "r2_locator", "roster_anchor", "ledger_anchor")


def _path_like_file(value: str) -> bool:
    return bool(value) and not value.startswith(("http://", "https://")) and (
        "/" in value or value.startswith(".") or bool(re.search(r"\.[A-Za-z0-9]{1,8}$", value))
    )


def _direct_hash_keys_for_path(path_key: str, hash_keys: Sequence[str], path_count: int) -> list[str]:
    aliases = {
        "test_source_path": ("test_source",),
        "test_path": ("test",),
        "test_file_path": ("test_file",),
        "source_fixture_path": ("source_fixture",),
        "source_under_test": ("source", "source_under_test"),
        "source_locator": ("source", "source_locator"),
        "source_path": ("source",),
        "existing_direct_test_source": ("existing_direct_test_source", "source"),
        "existing_test_source": ("existing_test_source", "test_source"),
        "fixture": ("fixture", "source_fixture", "source"),
        "test_source_ref": ("test_source",),
        "path": ("",),
        "file": ("file", "test_file"),
        "test_file": ("test_file", "test"),
        "test_source": ("test_source",),
        "test_path": ("test",),
    }
    prefixes = aliases.get(path_key, (path_key.removesuffix("_path"),))
    matched = []
    for key in hash_keys:
        if any(key.startswith(f"{prefix}_sha256") for prefix in prefixes if prefix):
            matched.append(key)
        elif not any(prefixes) and key in {"sha256", "sha256_current", "file_sha256"}:
            matched.append(key)
    if not matched and path_count == 1:
        matched = [key for key in hash_keys if not any(marker in key for marker in HISTORICAL_HASH_MARKERS)]
    return matched


def _bind_frozen_direct_contracts(
    contracts: Sequence[Any],
    *,
    source_path: str,
    source_sha256: str,
    registry: BoundFileRegistry,
    label: str,
) -> list[dict[str, Any]]:
    bound: list[dict[str, Any]] = []
    visited_strings: set[str] = set()
    current_owner: Any = None

    def append_binding(relative: str, digest: str, reported_hashes: Mapping[str, str], current_label: str) -> None:
        current_hashes = {
            key: value for key, value in reported_hashes.items()
            if not any(marker in key for marker in HISTORICAL_HASH_MARKERS)
        }
        reported_sha = next(iter(current_hashes.values()), None)
        bound.append({
            "locator_record": current_owner,
            "bound_test_source_path": relative,
            "reported_sha256": reported_sha,
            "reported_sha256_fields": dict(reported_hashes),
            "sha256_current_at_merge": digest,
            "hash_binding_status": "match" if reported_sha is not None else "no_review_hash_pin_locator_only",
            "authority": "source_identity_hash" if relative == source_path else "test_locator_only_not_semantic_authority",
            "executed": False,
            "accepted": False,
        })

    def visit(value: Any, current_label: str, *, allow_bare_path: bool = False) -> None:
        if isinstance(value, str):
            # A bare referenced file without a reported review hash remains a
            # locator-only gap, but its current bytes are still pinned.
            if allow_bare_path and _path_like_file(value) and value not in visited_strings:
                try:
                    relative = canonical_repo_path(value, current_label)
                    _, digest = registry.read(relative, "direct_test_contract_locator")
                except CensusError:
                    # Free text and commands are not path evidence. Canonical
                    # path-like values, however, must fail closed if invalid.
                    if "/" in value or value.startswith("."):
                        raise
                    return
                visited_strings.add(value)
                append_binding(relative, digest, {}, current_label)
        elif isinstance(value, list):
            for offset, child in enumerate(value, 1):
                visit(child, f"{current_label}[{offset}]", allow_bare_path=allow_bare_path)
        elif isinstance(value, dict):
            paths: list[tuple[str, str]] = []
            for key, child in value.items():
                if key in DIRECT_PATH_KEYS and isinstance(child, str) and _path_like_file(child):
                    paths.append((key, canonical_repo_path(child, f"{current_label}.{key}")))
            hash_values: dict[str, str] = {}
            for key, child in value.items():
                if key == "exact_source_sha256_pin":
                    if not isinstance(child, bool):
                        raise CensusError(f"{current_label}.{key} must be boolean")
                    continue
                if key.endswith("_status") and "sha256" in key:
                    if not isinstance(child, str):
                        raise CensusError(f"{current_label}.{key} must be text")
                    continue
                if "sha256" in key or key in {"source_pin_sha256", "test_sha256", "file_sha256"}:
                    hash_values[key] = _strict_sha(child, f"{current_label}.{key}")
            for key, relative in paths:
                _, digest = registry.read(relative, "direct_test_contract_locator")
                matching_hash_keys = _direct_hash_keys_for_path(key, list(hash_values), len(paths))
                reported_hashes = {name: hash_values[name] for name in matching_hash_keys}
                is_source = relative == source_path
                for hash_key, reported in reported_hashes.items():
                    historical = any(marker in hash_key for marker in HISTORICAL_HASH_MARKERS)
                    if not historical and reported != digest:
                        raise CensusError(
                            f"{current_label}: direct-test/source hash drift for {relative}: "
                            f"{hash_key} reports {reported}, current {digest}"
                        )
                append_binding(relative, digest, reported_hashes, current_label)
            if not paths:
                for hash_key, reported in hash_values.items():
                    if hash_key.startswith("source_") and "sha256" in hash_key and not any(
                        marker in hash_key for marker in HISTORICAL_HASH_MARKERS
                    ) and reported != source_sha256:
                        raise CensusError(
                            f"{current_label}: source pin {hash_key} conflicts with the live source hash"
                        )
            for key, child in value.items():
                if key not in DIRECT_PATH_KEYS:
                    visit(
                        child,
                        f"{current_label}.{key}",
                        allow_bare_path=key in {"existing_test_source_refs", "existing_direct_test_references", "test_source_refs", "test_sources"},
                    )
        elif value is not None and not isinstance(value, (bool, int)):
            raise CensusError(f"{current_label}: unsupported direct-test evidence value {type(value).__name__}")

    for offset, contract in enumerate(contracts, 1):
        current_owner = contract
        binding_count = len(bound)
        visit(contract, f"{label}[{offset}]")
        if len(bound) == binding_count:
            # Some proposed checks publish only a source SHA pin and command.
            # Bind that pin to the already selected current source locator.
            pin_hashes: dict[str, str] = {}
            def collect_source_pins(value: Any) -> None:
                if isinstance(value, dict):
                    for key, item in value.items():
                        if key.startswith("source_") and "sha256" in key:
                            pin_hashes[key] = _strict_sha(item, f"{label}[{offset}].{key}")
                        else:
                            collect_source_pins(item)
                elif isinstance(value, list):
                    for item in value:
                        collect_source_pins(item)
            collect_source_pins(contract)
            if pin_hashes:
                for key, reported in pin_hashes.items():
                    if not any(marker in key for marker in HISTORICAL_HASH_MARKERS) and reported != source_sha256:
                        raise CensusError(f"{label}[{offset}]: source SHA pin conflicts with current source")
                append_binding(source_path, source_sha256, pin_hashes, f"{label}[{offset}]")
            else:
                bound.append({
                    "locator_record": contract,
                    "bound_test_source_path": None,
                    "reported_sha256": None,
                    "reported_sha256_fields": {},
                    "sha256_current_at_merge": source_sha256,
                    "hash_binding_status": "no_file_locator_in_contract",
                    "authority": "proposed_contract_only_not_executed",
                    "executed": False,
                    "accepted": False,
                })
    return bound


def _normalize_parsed_row(
    index: int,
    shard_row: ParsedShardRow,
    locator: Locator,
    registry: BoundFileRegistry,
) -> dict[str, Any]:
    if shard_row.source_path != locator.path:
        raise CensusError(f"index {index}: shard source path differs from projected locator")
    if shard_row.source_sha256_current != locator.sha256:
        raise CensusError(f"index {index}: full-review source SHA-256 differs from current locator SHA-256")
    source_bytes, live_sha = registry.read(locator.path, "source")
    if live_sha != locator.sha256 or shard_row.source_sha256_current != live_sha:
        raise CensusError(f"index {index}: current source SHA-256 drift before merge")

    reported_r2_sha = shard_row.source_hash_provenance.get("r2_locator_sha256")
    if reported_r2_sha is not None and reported_r2_sha != locator.r2_sha256:
        raise CensusError(f"index {index}: shard r2 locator provenance differs from projected r2 locator hash")
    if index in APPROVED_LOCATOR_REFRESHES:
        approved = APPROVED_LOCATOR_REFRESHES[index]
        if locator.path != approved["path"] or locator.r2_sha256 != approved["r2_sha256"] or live_sha != approved["live_sha256"]:
            raise CensusError(f"index {index}: approved current-source refresh does not match frozen allowlist")

    for anchor in shard_row.source_evidence_lines:
        line_number = anchor["line"]
        source_lines = source_bytes.decode("utf-8").splitlines()
        if line_number > len(source_lines) or source_lines[line_number - 1].strip() != anchor["text"].strip():
            raise CensusError(f"index {index}: source evidence line {line_number} does not match current source bytes")
    line_count = len(source_bytes.decode("utf-8").splitlines())
    if "source_line_count" in shard_row.raw_row and shard_row.raw_row["source_line_count"] != line_count:
        raise CensusError(f"index {index}: source_line_count differs from current source bytes")

    normative = [
        _normalize_normative_item(item, registry, f"index {index}: normative_basis[{offset}]")
        for offset, item in enumerate(shard_row.normative_basis, 1)
    ]
    route_expectations: dict[str, Any] = {}
    for axis in ("default", "explicit_mir"):
        route = shard_row.route_expectations.get(axis)
        if route is None:
            route_expectations[axis] = {
                "expectation": "not_separately_adjudicated",
                "status": "not_run",
                "accepted_as_route_pass": False,
                "basis": "independent full-review shard has no route candidate for this axis; no route result inferred from source class.",
            }
            continue
        route_basis = [
            _normalize_normative_item(item, registry, f"index {index}: {axis} route basis[{offset}]")
            for offset, item in enumerate(route["normative_route_basis"], 1)
        ]
        route_expectations[axis] = {
            "expectation": route["expectation_state"],
            "expected_result": route["expected_result"],
            "exact_command": route["exact_command"],
            "status": "not_run",
            "accepted_as_route_pass": False,
            "normative_basis": route_basis,
        }
    direct = _bind_frozen_direct_contracts(
        shard_row.direct_test_contracts,
        source_path=locator.path,
        source_sha256=live_sha,
        registry=registry,
        label=f"index {index}: direct_test_contracts",
    )
    provenance = {
        "r2_locator_sha256_provenance_only": locator.r2_sha256,
        "r2_locator_drift_refreshed_to_live": locator.r2_sha256 != live_sha,
        "prior_semantic_snapshot_sha256_provenance_only": shard_row.source_hash_provenance.get("prior_semantic_snapshot_sha256"),
        "prior_semantic_snapshot_differs_from_live": (
            shard_row.source_hash_provenance["prior_semantic_snapshot_sha256"] != live_sha
            if shard_row.source_hash_provenance.get("prior_semantic_snapshot_sha256") is not None else None
        ),
        "source_hash_drift_during_review": False,
        "review_schema": shard_row.review_schema,
        "reported_source_hashes": dict(shard_row.source_hash_provenance.get("reported_source_hashes", {})),
        "source_hash_flags": dict(shard_row.source_hash_provenance.get("source_hash_flags", {})),
        "authority": "hash provenance only; never selects or overrides source class",
    }
    return {
        "schema": "actor-census-full-independent-merged-row-v1",
        "inventory_index": index,
        "source_path": locator.path,
        "source_sha256": live_sha,
        "source_semantic_class": {
            "value": shard_row.source_class,
            "authority": "full_independent_shard_candidate",
            "accepted": False,
            "semantic_basis": shard_row.semantic_basis,
            "normative_basis": normative,
            "hash_provenance": provenance,
            "source_evidence_lines": list(shard_row.source_evidence_lines),
        "source_non_normative_comments": list(shard_row.source_non_normative_comments),
        },
        "direct_test_contracts": direct,
        "direct_test_gaps": list(shard_row.direct_test_gaps),
        "conflicts_and_limits": list(shard_row.conflicts_and_limits),
        "route_expectations": {**route_expectations, "source_class_used_as_route_oracle": False},
        "status": "not_run",
        "accepted": False,
        "accepted_as_route_pass": False,
        "bootstrap_or_runtime_observation_used": False,
        "mimi_or_cargo_or_test_or_build_executed": False,
    }


def _normalize_row(
    index: int,
    shard_row: ParsedShardRow | Mapping[str, Any],
    locator: Locator,
    registry: BoundFileRegistry,
) -> dict[str, Any]:
    if isinstance(shard_row, ParsedShardRow):
        return _normalize_parsed_row(index, shard_row, locator, registry)
    if shard_row["source_path"] != locator.path:
        raise CensusError(
            f"index {index}: shard source path {shard_row['source_path']!r} differs from locator {locator.path!r}"
        )
    shard_sha = _strict_sha(shard_row["source_sha256_current"], f"index {index}: shard source hash")
    if shard_sha != locator.sha256:
        raise CensusError(f"index {index}: shard source hash conflicts with locator hash")
    _, live_sha = registry.read(locator.path, "source")
    if live_sha != locator.sha256:
        raise CensusError(f"index {index}: live source SHA-256 drift from locator/shard")

    extended = "candidate_semantic_class" in shard_row
    if extended and shard_row["source_sha256_r2_locator"] != locator.r2_sha256:
        raise CensusError(f"index {index}: re-review r2 locator provenance differs from locator inventory")
    normative = [
        _normalize_normative_item(item, registry, f"index {index}: normative_basis")
        for item in shard_row["normative_basis"]
    ]
    direct = []
    for offset, contract in enumerate(shard_row["direct_test_contracts"], 1):
        if not isinstance(contract, dict):
            raise CensusError(f"index {index}: direct_test_contracts[{offset}] must be an object")
        label = f"index {index}: direct_test_contracts[{offset}]"
        if extended and "source_under_test" in contract:
            direct.append({
                "locator_record": contract,
                "bound_test_source_path": None,
                "reported_sha256": contract["source_sha256"],
                "sha256_current_at_merge": live_sha,
                "hash_binding_status": "source_row_match",
                "authority": "proposed_source_check_contract_only_not_executed",
                "executed": False,
                "accepted": False,
            })
        elif extended:
            path = canonical_repo_path(contract["test_source_path"], f"{label}:test_source_path")
            reported = _strict_sha(contract["test_source_sha256"], f"{label}:test_source_sha256")
            _, current = registry.read(path, "direct_test_contract_locator")
            if current != reported:
                raise CensusError(
                    f"index {index}: direct-test source hash drift for {path}: reported {reported}, current {current}"
                )
            direct.append({
                "locator_record": contract,
                "bound_test_source_path": path,
                "reported_sha256": reported,
                "sha256_current_at_merge": current,
                "hash_binding_status": "match",
                "authority": "test_locator_only_not_semantic_authority",
                "executed": False,
                "accepted": False,
            })
        else:
            path, reported = _contract_path_and_hash(contract, label)
            _, current = registry.read(path, "direct_test_contract_locator")
            if current != reported:
                raise CensusError(
                    f"index {index}: direct-test source hash drift for {path}: reported {reported}, current {current}"
                )
            direct.append({
                "locator_record": contract,
                "bound_test_source_path": path,
                "reported_sha256": reported,
                "sha256_current_at_merge": current,
                "hash_binding_status": "match",
                "authority": "test_locator_only_not_semantic_authority",
                "executed": False,
                "accepted": False,
            })

    route_expectations = {}
    if extended:
        for output_axis, input_axis in (
            ("default", "default_route_expectation"),
            ("explicit_mir", "explicit_mir_route_expectation"),
        ):
            route = shard_row[input_axis]
            route_basis = [
                _normalize_normative_item(item, registry, f"index {index}: {input_axis}.normative_route_basis")
                for item in route["normative_route_basis"]
            ]
            route_expectations[output_axis] = {
                "expectation": route["expectation_state"],
                "expected_result": route["expected_result"],
                "exact_command": route["exact_command"],
                "status": "not_run",
                "accepted_as_route_pass": False,
                "normative_basis": route_basis,
            }
    else:
        for axis in ("default", "explicit_mir"):
            route_expectations[axis] = {
                "expectation": "not_separately_adjudicated",
                "status": "not_run",
                "accepted_as_route_pass": False,
                "basis": "full-review shard has no independent route expectation field; none inferred from source class or test observations.",
            }

    source_evidence_lines = []
    if extended:
        source_bytes, _ = registry.read(locator.path, "source_anchor_validation")
        source_lines = source_bytes.decode("utf-8").splitlines()
        for anchor in shard_row["source_evidence_lines"]:
            line_number = anchor["line"]
            if line_number > len(source_lines) or source_lines[line_number - 1].strip() != anchor["text"].strip():
                raise CensusError(f"index {index}: source evidence line {line_number} does not match current source bytes")
            source_evidence_lines.append({"line": line_number, "text": anchor["text"]})

    provenance = None
    if extended:
        provenance = {
            "r2_locator_sha256_provenance_only": locator.r2_sha256,
            "r2_locator_drift_refreshed_to_live": locator.r2_sha256 != live_sha,
            "prior_semantic_snapshot_sha256_provenance_only": shard_row["source_sha256_prior_semantic_snapshot"],
            "prior_semantic_snapshot_differs_from_live": shard_row["source_sha256_prior_semantic_snapshot"] != live_sha,
            "source_hash_drift_during_review": False,
            "authority": "hash provenance only; never selects or overrides source class",
        }

    return {
        "schema": "actor-census-full-independent-merged-row-v1",
        "inventory_index": index,
        "source_path": locator.path,
        "source_sha256": live_sha,
        "source_semantic_class": {
            "value": shard_row["candidate_semantic_class"] if extended else shard_row["expected_class"],
            "authority": "full_independent_shard_candidate",
            "accepted": False,
            "semantic_basis": shard_row["semantic_basis"],
            "normative_basis": normative,
            "hash_provenance": provenance,
            "source_evidence_lines": source_evidence_lines,
        "source_non_normative_comments": [],
        },
        "direct_test_contracts": direct,
        "direct_test_gaps": list(shard_row["direct_test_gaps"]),
        "conflicts_and_limits": list(shard_row["conflicts_and_limits"]),
        "route_expectations": {
            **route_expectations,
            "source_class_used_as_route_oracle": False,
        },
        "status": "not_run",
        "accepted": False,
        "accepted_as_route_pass": False,
        "bootstrap_or_runtime_observation_used": False,
        "mimi_or_cargo_or_test_or_build_executed": False,
    }


def _canonical_json_bytes(value: Any, *, indent: int = 2) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=indent) + "\n").encode("utf-8")


def _serialize_jsonl(rows: Sequence[Mapping[str, Any]]) -> bytes:
    return b"".join(
        (json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
        for row in rows
    )


def _now_utc() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def _manifest_input(path: Path, role: str, digest: str) -> dict[str, str]:
    return {"role": role, "path": str(path.resolve()), "sha256": digest}


def _row_class_counts(rows: Sequence[Mapping[str, Any]]) -> dict[str, int]:
    counts = Counter(row["source_semantic_class"]["value"] for row in rows)
    return dict(sorted(counts.items()))


def _route_expectation_counts(rows: Sequence[Mapping[str, Any]], axis: str) -> dict[str, int]:
    counts = Counter(row["route_expectations"][axis]["expectation"] for row in rows)
    return dict(sorted(counts.items()))


def _build_summary(rows: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    class_counts = _row_class_counts(rows)
    return {
        "schema": "actor-census-full-independent-summary-v1",
        "status": "candidate_only_not_accepted",
        "accepted": False,
        "accepted_as_route_pass": False,
        "row_count": len(rows),
        "inventory_index_min": rows[0]["inventory_index"],
        "inventory_index_max": rows[-1]["inventory_index"],
        "source_class_counts": class_counts,
        "source_class_total": sum(class_counts.values()),
        "default_route_expectation_counts": _route_expectation_counts(rows, "default"),
        "explicit_mir_route_expectation_counts": _route_expectation_counts(rows, "explicit_mir"),
        "execution_status_counts": {"not_run": len(rows)},
        "accepted_count": 0,
        "route_accepted_count": 0,
        "runtime_or_bootstrap_authority_count": 0,
        "mimi_cargo_test_build_executed_count": 0,
    }


def _build_readme(locator_audit: Mapping[str, Any], locator_refreshes: Sequence[Mapping[str, Any]]) -> bytes:
    refreshed_indices = [str(item["inventory_index"]) for item in locator_refreshes]
    refresh_note = ", ".join(refreshed_indices) if refreshed_indices else "none"
    return (
        "# Actor schema-v4 full independent census candidate\n\n"
        "This is a source-bound, unaccepted candidate merge of nine independent review shards. "
        "The r2 current inventory contributes only record index and path as identity; its recorded "
        "source hash is provenance, and each current source hash is recomputed from live repository bytes. "
        "All r2 semantic, route, execution, and acceptance fields are excluded from adjudication.\n\n"
        f"- Rows: {COUNT} (indices 1–{COUNT})\n"
        "- Every row: `status=not_run`, `accepted=false`, `accepted_as_route_pass=false`.\n"
        "- Default route and explicit `--mir` route expectations are separate axes; route results are not inferred from source class.\n"
        "- No Mimi, Cargo, test, or build was run by this merger.\n"
        "- Normative and direct-test source SHA-256 values are checked against current bytes whenever a review shard reports a pin; unpinned references receive a current locator binding only.\n"
        f"- The r2 locator input SHA-256 is `{locator_audit['r2_input_sha256']}`; its projection rule and excluded-field/null counts are recorded in `manifest.json`.\n"
        f"- Approved r2 locator hash refreshes: {refresh_note}. All shard source hashes must match live bytes.\n"
        "- This package is a candidate record, not semantic or route acceptance.\n"
    ).encode("utf-8")


def preflight_frozen_inputs(
    *,
    repo_root: Path,
    locator_ledger: Path = Path(FROZEN_LOCATOR_INPUT),
    shard_paths: Sequence[Path] = tuple(Path(path) for path, _ in FROZEN_SHARD_INPUTS),
) -> dict[str, Any]:
    """Read-only validation of the locked source/review inputs; writes no package."""
    repo_root = repo_root.resolve(strict=True)
    paths = [locator_ledger, *shard_paths]
    raw_inputs = [read_external_regular(path, "frozen census preflight input") for path in paths]
    _check_frozen_input_lock(locator_ledger, shard_paths, raw_inputs, allow_synthetic_inputs=False)
    registry = BoundFileRegistry.create(repo_root)
    locators, refreshes = read_locator_roster(raw_inputs[0], registry)
    projection = locator_projection_audit(raw_inputs[0])
    shards, shard_snapshots = _load_shard_rows(shard_paths, repo_root=repo_root, frozen_schema=True)
    for path, raw in shard_snapshots.items():
        if sha256_bytes(raw) != sha256_bytes(raw_inputs[paths.index(path)]):
            raise CensusError(f"preflight shard changed while loading: {path}")
    normalized = [
        _normalize_row(index, shards[index], locators[index - 1], registry)
        for index in range(1, COUNT + 1)
    ]
    bound_snapshots = registry.snapshots()
    if len(bound_snapshots) != len(registry.by_path):
        raise CensusError("preflight bound source/spec/test snapshot count changed")
    for path, raw in zip(paths, raw_inputs, strict=True):
        if _stable_input_hash(path, "preflight input recheck") != sha256_bytes(raw):
            raise CensusError(f"preflight input drift: {path}")
    return {
        "status": "read_only_preflight_not_accepted",
        "accepted": False,
        "row_count": len(normalized),
        "inventory_index_min": normalized[0]["inventory_index"],
        "inventory_index_max": normalized[-1]["inventory_index"],
        "source_class_counts": _row_class_counts(normalized),
        "default_route_expectation_counts": _route_expectation_counts(normalized, "default"),
        "explicit_mir_route_expectation_counts": _route_expectation_counts(normalized, "explicit_mir"),
        "locator_projection": projection,
        "locator_refreshes": refreshes,
        "bound_repo_file_count": len(bound_snapshots),
        "all_status_not_run": all(row["status"] == "not_run" for row in normalized),
        "all_unaccepted": all(row["accepted"] is False and row["accepted_as_route_pass"] is False for row in normalized),
        "outputs_written": False,
        "execution_tools_invoked": False,
    }


def _package_file_hashes(package_dir: Path) -> dict[str, str]:
    return {
        name: sha256_file(package_dir / name)
        for name in ("README.md", "census.jsonl", "summary.json")
    }


def _verify_input_snapshots(
    external_hashes: Mapping[Path, str],
    repo_root: Path,
    bound_hashes: Mapping[str, str],
) -> None:
    for path, expected in external_hashes.items():
        current = _stable_input_hash(path, "input snapshot recheck")
        if current != expected:
            raise CensusError(f"input hash drift: {path}")
    registry = BoundFileRegistry.create(repo_root)
    for relative, expected in bound_hashes.items():
        _, current = registry.read(relative, "post-merge snapshot recheck")
        if current != expected:
            raise CensusError(f"source/spec/test file drift: {relative}")


def _check_frozen_input_lock(
    locator_ledger: Path,
    shard_paths: Sequence[Path],
    raw_inputs: Sequence[bytes],
    *,
    allow_synthetic_inputs: bool,
) -> str:
    if allow_synthetic_inputs:
        return "synthetic_test_only"
    if len(shard_paths) != len(FROZEN_SHARD_INPUTS) or len(raw_inputs) < 10:
        raise CensusError("canonical frozen input lock requires one locator and exactly nine shards")
    expected_paths = [Path(FROZEN_LOCATOR_INPUT), *(Path(path) for path, _ in FROZEN_SHARD_INPUTS)]
    actual_paths = [locator_ledger, *shard_paths]
    expected_hashes = [FROZEN_LOCATOR_SHA256, *(digest for _, digest in FROZEN_SHARD_INPUTS)]
    for offset, (actual, expected_path, expected_hash, raw) in enumerate(
        zip(actual_paths, expected_paths, expected_hashes, raw_inputs, strict=True)
    ):
        try:
            actual_resolved = actual.resolve(strict=True)
        except OSError as error:
            raise CensusError(f"canonical input {offset} cannot be resolved: {error}") from error
        if actual_resolved != expected_path:
            raise CensusError(f"canonical input {offset} path differs from the frozen input lock")
        if sha256_bytes(raw) != expected_hash:
            raise CensusError(f"canonical input {offset} SHA-256 differs from the frozen input lock")
    return "canonical_frozen"


def build_candidate_package(
    *,
    locator_ledger: Path,
    shard_paths: Sequence[Path],
    repo_root: Path,
    output_dir: Path,
    args_record: Mapping[str, Any] | None = None,
    allow_synthetic_inputs: bool = False,
) -> dict[str, Any]:
    """Validate and atomically write an unaccepted candidate package.

    This function is exercised only with temporary synthetic inputs in the
    accompanying Python-only tests. It has no Mimi/Cargo/test/build runner.
    """
    repo_root = repo_root.resolve(strict=True)
    if output_dir.exists() or output_dir.is_symlink():
        raise CensusError(f"output directory already exists: {output_dir}")
    if len(shard_paths) != 9:
        raise CensusError("exactly nine full-review shards are required")

    input_paths = [locator_ledger, *shard_paths, Path(__file__).resolve()]
    input_inodes: dict[tuple[int, int], Path] = {}
    external_hashes: dict[Path, str] = {}
    external_bytes: dict[Path, bytes] = {}
    input_records: list[dict[str, str]] = []
    for offset, path in enumerate(input_paths):
        role = "r2_current_locator_projection" if offset == 0 else "full_review_shard" if offset <= 9 else "merger_script"
        raw = read_external_regular(path, role)
        info = path.stat()
        inode = (info.st_dev, info.st_ino)
        if inode in input_inodes:
            raise CensusError(f"duplicate/aliased input file: {path} and {input_inodes[inode]}")
        input_inodes[inode] = path
        digest = sha256_bytes(raw)
        external_hashes[path] = digest
        external_bytes[path] = raw
        input_records.append(_manifest_input(path, role, digest))

    registry = BoundFileRegistry.create(repo_root)
    input_mode = _check_frozen_input_lock(
        locator_ledger,
        shard_paths,
        [external_bytes[path] for path in input_paths[:10]],
        allow_synthetic_inputs=allow_synthetic_inputs,
    )
    locator_rows, locator_refreshes = read_locator_roster(
        external_bytes[locator_ledger], registry
    )
    locator_audit = locator_projection_audit(external_bytes[locator_ledger])
    shards_by_index, shard_snapshots = _load_shard_rows(
        shard_paths,
        repo_root=repo_root,
        frozen_schema=(input_mode == "canonical_frozen"),
    )
    for path, raw in shard_snapshots.items():
        if sha256_bytes(raw) != external_hashes[path]:
            raise CensusError(f"shard changed between input snapshot and parse: {path}")

    merged_rows = [
        _normalize_row(index, shards_by_index[index], locator_rows[index - 1], registry)
        for index in range(1, COUNT + 1)
    ]
    bound_hashes = registry.snapshots()
    bound_records = [registry.by_path[path] for path in sorted(registry.by_path)]
    review_schema_counts = dict(sorted(Counter(
        row.review_schema if isinstance(row, ParsedShardRow) else "synthetic_legacy_row"
        for row in shards_by_index.values()
    ).items()))

    summary = _build_summary(merged_rows)
    census_bytes = _serialize_jsonl(merged_rows)
    summary_bytes = _canonical_json_bytes(summary)
    readme_bytes = _build_readme(locator_audit, locator_refreshes)

    output_dir = output_dir.absolute()
    output_dir.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=f".{output_dir.name}.candidate-", dir=output_dir.parent))
    published = False
    try:
        (staging / "census.jsonl").write_bytes(census_bytes)
        (staging / "summary.json").write_bytes(summary_bytes)
        (staging / "README.md").write_bytes(readme_bytes)
        outputs = {
            "README.md": sha256_bytes(readme_bytes),
            "census.jsonl": sha256_bytes(census_bytes),
            "summary.json": sha256_bytes(summary_bytes),
        }
        manifest = {
            "schema": "actor-census-full-independent-manifest-v1",
            "status": "candidate_only_not_accepted",
            "accepted": False,
            "accepted_as_route_pass": False,
            "input_mode": input_mode,
            "created_utc": _now_utc(),
            "row_count": COUNT,
            "shard_ranges": [list(value) for value in SHARD_RANGES],
            "full_review_schema_counts": review_schema_counts,
            "locator_projection": locator_audit,
            "locator_refreshes": locator_refreshes,
            "source_class_vocabulary": sorted(SOURCE_CLASSES),
            "route_axes": {
                "default": "independent candidate expectation where present; otherwise not_separately_adjudicated; not_run; never inferred from source class",
                "explicit_mir": "independent candidate expectation where present; otherwise not_separately_adjudicated; not_run; never inferred from source class",
            },
            "inputs": input_records,
            "bound_repo_files": bound_records,
            "merger": {
                "path": "scripts/merge_actor_census_full_independent.py",
                "sha256": external_hashes[Path(__file__).resolve()],
            },
            "python": {
                "implementation": platform.python_implementation(),
                "version": platform.python_version(),
                "executable": sys.executable,
            },
            "arguments": dict(args_record or {}),
            "outputs": outputs,
        }
        manifest_bytes = _canonical_json_bytes(manifest)
        (staging / "manifest.json").write_bytes(manifest_bytes)
        sums = {
            "README.md": sha256_bytes(readme_bytes),
            "census.jsonl": sha256_bytes(census_bytes),
            "summary.json": sha256_bytes(summary_bytes),
            "manifest.json": sha256_bytes(manifest_bytes),
        }
        sums_bytes = "".join(f"{digest}  {name}\n" for name, digest in sorted(sums.items())).encode("ascii")
        (staging / "SHA256SUMS").write_bytes(sums_bytes)

        _verify_input_snapshots(external_hashes, repo_root, bound_hashes)
        validate_candidate_package(
            staging,
            repo_root=repo_root,
            validate_live_bindings=True,
            allow_synthetic_inputs=allow_synthetic_inputs,
        )
        if output_dir.exists() or output_dir.is_symlink():
            raise CensusError(f"output directory appeared during merge: {output_dir}")
        os.rename(staging, output_dir)
        published = True
        validate_candidate_package(
            output_dir,
            repo_root=repo_root,
            validate_live_bindings=True,
            allow_synthetic_inputs=allow_synthetic_inputs,
        )
        _verify_input_snapshots(external_hashes, repo_root, bound_hashes)
        return manifest
    except Exception:
        if published and output_dir.exists() and not output_dir.is_symlink():
            shutil.rmtree(output_dir)
        elif staging.exists() and not staging.is_symlink():
            shutil.rmtree(staging)
        raise


def _read_sha256sums(path: Path) -> dict[str, str]:
    try:
        text = path.read_text(encoding="ascii")
    except (OSError, UnicodeDecodeError) as error:
        raise CensusError(f"invalid SHA256SUMS: {error}") from error
    result: dict[str, str] = {}
    for line_number, line in enumerate(text.splitlines(), 1):
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match:
            raise CensusError(f"SHA256SUMS:{line_number}: malformed entry")
        digest, name = match.groups()
        if name in result:
            raise CensusError(f"SHA256SUMS: duplicate filename {name}")
        result[name] = digest
    if set(result) != {"README.md", "census.jsonl", "summary.json", "manifest.json"}:
        raise CensusError("SHA256SUMS must cover exact package file set")
    return result


def _read_bound_manifest_inputs(
    manifest: Mapping[str, Any],
    *,
    allow_synthetic_inputs: bool,
) -> tuple[list[Path], list[bytes]]:
    input_mode = manifest.get("input_mode")
    if input_mode not in {"canonical_frozen", "synthetic_test_only"}:
        raise CensusError("manifest input_mode is invalid")
    if input_mode == "synthetic_test_only" and not allow_synthetic_inputs:
        raise CensusError("synthetic-test package validation requires explicit allow_synthetic_inputs=True")
    items = manifest.get("inputs")
    expected_roles = ["r2_current_locator_projection"] + ["full_review_shard"] * 9 + ["merger_script"]
    if not isinstance(items, list) or len(items) != len(expected_roles):
        raise CensusError("manifest inputs must bind one r2 locator, nine shards, and the merger")
    paths: list[Path] = []
    raw_inputs: list[bytes] = []
    seen_inodes: set[tuple[int, int]] = set()
    seen_paths: set[str] = set()
    for offset, (item, expected_role) in enumerate(zip(items, expected_roles, strict=True)):
        if not isinstance(item, dict) or set(item) != {"role", "path", "sha256"}:
            raise CensusError(f"manifest input {offset} has invalid schema")
        if item["role"] != expected_role:
            raise CensusError(f"manifest input {offset} has unexpected role")
        path_text = item["path"]
        if not isinstance(path_text, str) or not Path(path_text).is_absolute():
            raise CensusError(f"manifest input {offset} path must be absolute")
        path = Path(path_text)
        try:
            resolved = path.resolve(strict=True)
        except OSError as error:
            raise CensusError(f"manifest input {offset} path cannot be resolved: {error}") from error
        if str(resolved) != path_text:
            raise CensusError(f"manifest input {offset} path is noncanonical or traverses a symlink")
        if path_text in seen_paths:
            raise CensusError(f"manifest input path is duplicated: {path_text}")
        raw = read_external_regular(path, f"manifest input {offset}")
        info = path.stat()
        inode = (info.st_dev, info.st_ino)
        if inode in seen_inodes:
            raise CensusError(f"manifest input path aliases another input: {path_text}")
        seen_inodes.add(inode)
        seen_paths.add(path_text)
        expected_sha = _strict_sha(item["sha256"], f"manifest input {offset} sha256")
        if sha256_bytes(raw) != expected_sha:
            raise CensusError(f"manifest input hash drift: {path_text}")
        paths.append(path)
        raw_inputs.append(raw)
    merger = manifest.get("merger")
    if not isinstance(merger, dict) or set(merger) != {"path", "sha256"}:
        raise CensusError("manifest merger record has invalid schema")
    if merger["path"] != "scripts/merge_actor_census_full_independent.py":
        raise CensusError("manifest merger path is not the strict full-census merger")
    _strict_sha(merger["sha256"], "manifest merger sha256")
    if merger["sha256"] != items[-1]["sha256"]:
        raise CensusError("manifest merger hash does not match its bound input")
    if input_mode == "canonical_frozen":
        expected = [(FROZEN_LOCATOR_INPUT, FROZEN_LOCATOR_SHA256), *FROZEN_SHARD_INPUTS]
        for offset, (item, (expected_path, expected_sha)) in enumerate(
            zip(items[:10], expected, strict=True)
        ):
            if item["path"] != expected_path or item["sha256"] != expected_sha:
                raise CensusError(f"manifest canonical frozen input {offset} differs from lock")
    python_info = manifest.get("python")
    if not isinstance(python_info, dict) or set(python_info) != {"implementation", "version", "executable"}:
        raise CensusError("manifest Python runtime record has invalid schema")
    if not all(isinstance(python_info[key], str) and python_info[key] for key in python_info):
        raise CensusError("manifest Python runtime fields must be nonempty strings")
    if not isinstance(manifest.get("arguments"), dict):
        raise CensusError("manifest arguments must be an object")
    return paths, raw_inputs


def validate_candidate_package(
    package_dir: Path,
    *,
    repo_root: Path,
    validate_live_bindings: bool = True,
    allow_synthetic_inputs: bool = False,
) -> dict[str, Any]:
    if package_dir.is_symlink() or not package_dir.is_dir():
        raise CensusError(f"candidate package is not a non-symlink directory: {package_dir}")
    expected_files = {"README.md", "census.jsonl", "summary.json", "manifest.json", "SHA256SUMS"}
    actual_files = {entry.name for entry in package_dir.iterdir()}
    if actual_files != expected_files:
        raise CensusError(f"package file set mismatch: unknown={sorted(actual_files-expected_files)}, missing={sorted(expected_files-actual_files)}")
    for name in expected_files:
        if (package_dir / name).is_symlink() or not (package_dir / name).is_file():
            raise CensusError(f"package member is not a regular non-symlink file: {name}")
    sums = _read_sha256sums(package_dir / "SHA256SUMS")
    for name, expected in sums.items():
        if sha256_file(package_dir / name) != expected:
            raise CensusError(f"package checksum mismatch: {name}")

    manifest = strict_json_loads((package_dir / "manifest.json").read_bytes(), "manifest.json")
    if not isinstance(manifest, dict):
        raise CensusError("manifest.json must be an object")
    manifest_keys = {
        "schema", "status", "accepted", "accepted_as_route_pass", "input_mode", "created_utc", "row_count",
        "shard_ranges", "full_review_schema_counts", "locator_projection", "locator_refreshes", "source_class_vocabulary", "route_axes", "inputs", "bound_repo_files",
        "merger", "python", "arguments", "outputs",
    }
    if set(manifest) != manifest_keys:
        raise CensusError("manifest.json contains unknown or missing fields")
    if manifest["schema"] != "actor-census-full-independent-manifest-v1" or manifest["status"] != "candidate_only_not_accepted":
        raise CensusError("manifest has unsupported schema/status")
    if manifest["accepted"] is not False or manifest["accepted_as_route_pass"] is not False:
        raise CensusError("manifest acceptance flags must remain false")
    if not isinstance(manifest["created_utc"], str):
        raise CensusError("manifest created_utc must be a string")
    try:
        created = datetime.fromisoformat(manifest["created_utc"])
    except ValueError as error:
        raise CensusError("manifest created_utc is not ISO-8601") from error
    if created.tzinfo is None:
        raise CensusError("manifest created_utc must include a timezone")
    if manifest["row_count"] != COUNT or manifest["shard_ranges"] != [list(value) for value in SHARD_RANGES]:
        raise CensusError("manifest roster/range declaration is not exact")
    if manifest["source_class_vocabulary"] != sorted(SOURCE_CLASSES):
        raise CensusError("manifest source class vocabulary is not exact")
    if manifest["route_axes"] != {
        "default": "independent candidate expectation where present; otherwise not_separately_adjudicated; not_run; never inferred from source class",
        "explicit_mir": "independent candidate expectation where present; otherwise not_separately_adjudicated; not_run; never inferred from source class",
    }:
        raise CensusError("manifest route-axis contract changed")
    if not isinstance(manifest["locator_refreshes"], list):
        raise CensusError("manifest locator_refreshes must be a list")
    input_paths, raw_inputs = _read_bound_manifest_inputs(
        manifest,
        allow_synthetic_inputs=allow_synthetic_inputs,
    )
    computed_locator_audit = locator_projection_audit(raw_inputs[0])
    if manifest["locator_projection"] != computed_locator_audit:
        raise CensusError("manifest locator projection audit does not match the bound r2 input")
    if validate_live_bindings:
        bound_records = manifest["bound_repo_files"]
        if not isinstance(bound_records, list):
            raise CensusError("manifest bound_repo_files must be a list")
        precheck_registry = BoundFileRegistry.create(repo_root)
        seen_bound_paths: set[str] = set()
        for item in bound_records:
            if not isinstance(item, dict) or set(item) != {"path", "sha256", "roles"}:
                raise CensusError("manifest bound file record has invalid schema")
            relative = canonical_repo_path(item["path"], "manifest bound file path")
            if relative in seen_bound_paths:
                raise CensusError(f"manifest bound file path is duplicated: {relative}")
            seen_bound_paths.add(relative)
            expected_hash = _strict_sha(item["sha256"], f"manifest bound file {relative}")
            roles = item["roles"]
            if (
                not isinstance(roles, list)
                or not roles
                or any(not isinstance(role, str) or not role for role in roles)
                or roles != sorted(set(roles))
            ):
                raise CensusError(f"manifest bound file roles are invalid: {relative}")
            _, current_hash = precheck_registry.read(relative, "manifest_live_binding_precheck")
            if current_hash != expected_hash:
                raise CensusError(f"live source/spec/test hash drift: {relative}")
    if not isinstance(manifest["outputs"], dict) or set(manifest["outputs"]) != {"README.md", "census.jsonl", "summary.json"}:
        raise CensusError("manifest outputs schema is invalid")
    for name, digest in manifest["outputs"].items():
        if _strict_sha(digest, f"manifest outputs {name}") != sha256_file(package_dir / name):
            raise CensusError(f"manifest output hash mismatch: {name}")

    census_rows = parse_jsonl((package_dir / "census.jsonl").read_bytes(), "census.jsonl")
    if len(census_rows) != COUNT:
        raise CensusError(f"census row count is {len(census_rows)}, expected {COUNT}")
    seen_paths: set[str] = set()
    seen_indices: set[int] = set()
    for row in census_rows:
        keys = {
            "schema", "inventory_index", "source_path", "source_sha256", "source_semantic_class",
            "direct_test_contracts", "direct_test_gaps", "conflicts_and_limits", "route_expectations",
            "status", "accepted", "accepted_as_route_pass", "bootstrap_or_runtime_observation_used",
            "mimi_or_cargo_or_test_or_build_executed",
        }
        if set(row) != keys:
            raise CensusError("merged row has unknown/missing fields")
        index = _strict_int(row["inventory_index"], "merged row inventory_index")
        path = canonical_repo_path(row["source_path"], f"merged row {index} source_path")
        digest = _strict_sha(row["source_sha256"], f"merged row {index} source_sha256")
        if index in seen_indices or path in seen_paths:
            raise CensusError(f"duplicate merged row index/path: {index} {path}")
        seen_indices.add(index)
        seen_paths.add(path)
        if row["schema"] != "actor-census-full-independent-merged-row-v1":
            raise CensusError(f"merged row {index}: unsupported schema")
        source_class = row["source_semantic_class"]
        if not isinstance(source_class, dict) or set(source_class) != {
            "value", "authority", "accepted", "semantic_basis", "normative_basis",
            "hash_provenance", "source_evidence_lines", "source_non_normative_comments",
        }:
            raise CensusError(f"merged row {index}: invalid source_semantic_class schema")
        if source_class["value"] not in SOURCE_CLASSES or source_class["accepted"] is not False:
            raise CensusError(f"merged row {index}: invalid/unaccepted source class")
        if source_class["authority"] != "full_independent_shard_candidate":
            raise CensusError(f"merged row {index}: wrong source-class provenance")
        if not isinstance(source_class["semantic_basis"], str) or not source_class["semantic_basis"].strip():
            raise CensusError(f"merged row {index}: empty source semantic basis")
        if source_class["hash_provenance"] is not None:
            provenance = source_class["hash_provenance"]
            legacy_provenance_keys = {
                "r2_locator_sha256_provenance_only", "r2_locator_drift_refreshed_to_live",
                "prior_semantic_snapshot_sha256_provenance_only", "prior_semantic_snapshot_differs_from_live",
                "source_hash_drift_during_review", "authority",
            }
            frozen_provenance_keys = legacy_provenance_keys | {
                "review_schema", "reported_source_hashes", "source_hash_flags",
            }
            if not isinstance(provenance, dict) or set(provenance) not in (legacy_provenance_keys, frozen_provenance_keys):
                raise CensusError(f"merged row {index}: invalid hash provenance schema")
            _strict_sha(provenance["r2_locator_sha256_provenance_only"], f"merged row {index}:r2_locator_sha256_provenance_only")
            prior_sha = provenance["prior_semantic_snapshot_sha256_provenance_only"]
            if prior_sha is not None:
                _strict_sha(prior_sha, f"merged row {index}:prior_semantic_snapshot_sha256_provenance_only")
            if provenance["authority"] != "hash provenance only; never selects or overrides source class":
                raise CensusError(f"merged row {index}: hash provenance authority changed")
            if provenance["source_hash_drift_during_review"] is not False:
                raise CensusError(f"merged row {index}: review-time drift cannot be accepted")
            if provenance["r2_locator_drift_refreshed_to_live"] is not (
                provenance["r2_locator_sha256_provenance_only"] != digest
            ):
                raise CensusError(f"merged row {index}: r2 drift provenance disagrees with source hash")
            prior_diff = None if prior_sha is None else prior_sha != digest
            if provenance["prior_semantic_snapshot_differs_from_live"] is not prior_diff:
                raise CensusError(f"merged row {index}: historic snapshot provenance disagrees with source hash")
            if set(provenance) == frozen_provenance_keys:
                if provenance["review_schema"] not in {schema_name for schema_name, _ in FROZEN_SOURCE_SCHEMA_KEYS}:
                    raise CensusError(f"merged row {index}: unknown source review schema provenance")
                if not isinstance(provenance["reported_source_hashes"], dict) or not isinstance(provenance["source_hash_flags"], dict):
                    raise CensusError(f"merged row {index}: malformed reported source hash provenance")
                for key, value in provenance["reported_source_hashes"].items():
                    _strict_sha(value, f"merged row {index}:reported source hash {key}")
        elif source_class["source_evidence_lines"] != []:
            raise CensusError(f"merged row {index}: basic schema cannot carry source evidence lines")
        if not isinstance(source_class["source_evidence_lines"], list):
            raise CensusError(f"merged row {index}: source_evidence_lines must be a list")
        for anchor in source_class["source_evidence_lines"]:
            if not isinstance(anchor, dict) or set(anchor) != {"line", "text"}:
                raise CensusError(f"merged row {index}: malformed source evidence anchor")
            _strict_int(anchor["line"], f"merged row {index}: anchor line")
            if not isinstance(anchor["text"], str):
                raise CensusError(f"merged row {index}: anchor text must be a string")
        if not isinstance(source_class["source_non_normative_comments"], list):
            raise CensusError(f"merged row {index}: non-normative source comments must be a list")
        for comment in source_class["source_non_normative_comments"]:
            if not isinstance(comment, dict) or set(comment) != {"line", "text", "status"}:
                raise CensusError(f"merged row {index}: malformed non-normative source comment")
            _strict_int(comment["line"], f"merged row {index}: non-normative comment line")
            if not isinstance(comment["text"], str) or comment["status"] != "source_comment_non_normative_not_used_as_authority":
                raise CensusError(f"merged row {index}: invalid non-normative source comment authority")
        if not isinstance(source_class["normative_basis"], list):
            raise CensusError(f"merged row {index}: normative_basis must be a list")
        for reference in source_class["normative_basis"]:
            if not isinstance(reference, dict) or set(reference) != {
                "reference", "current_file_bindings", "review_time_file_hash_present_in_shard", "authority"
            }:
                raise CensusError(f"merged row {index}: malformed normative basis reference")
            if not isinstance(reference["review_time_file_hash_present_in_shard"], bool) or reference["authority"] != "candidate_reference_only":
                raise CensusError(f"merged row {index}: normative reference authority/status changed")
            if not isinstance(reference["reference"], str) or not isinstance(reference["current_file_bindings"], list):
                raise CensusError(f"merged row {index}: malformed normative reference values")
            for binding in reference["current_file_bindings"]:
                if not isinstance(binding, dict) or set(binding) != {"path", "sha256_current_at_merge"}:
                    raise CensusError(f"merged row {index}: malformed normative file binding")
                canonical_repo_path(binding["path"], f"merged row {index}: normative file")
                _strict_sha(binding["sha256_current_at_merge"], f"merged row {index}: normative file hash")
        if not isinstance(row["direct_test_contracts"], list):
            raise CensusError(f"merged row {index}: direct_test_contracts must be a list")
        for contract in row["direct_test_contracts"]:
            legacy_contract_keys = {
                "locator_record", "bound_test_source_path", "reported_sha256", "sha256_current_at_merge",
                "hash_binding_status", "authority", "executed", "accepted",
            }
            canonical_contract_keys = legacy_contract_keys | {"reported_sha256_fields"}
            if not isinstance(contract, dict) or set(contract) not in (legacy_contract_keys, canonical_contract_keys):
                raise CensusError(f"merged row {index}: malformed direct-test contract output")
            if contract["reported_sha256"] is not None:
                _strict_sha(contract["reported_sha256"], f"merged row {index}: test reported hash")
            if set(contract) == canonical_contract_keys:
                if not isinstance(contract["reported_sha256_fields"], dict):
                    raise CensusError(f"merged row {index}: direct contract reported hashes must be an object")
                for key, value in contract["reported_sha256_fields"].items():
                    _strict_sha(value, f"merged row {index}: direct contract reported hash {key}")
            _strict_sha(contract["sha256_current_at_merge"], f"merged row {index}: test current hash")
            if contract["executed"] is not False or contract["accepted"] is not False:
                raise CensusError(f"merged row {index}: direct-test contract was executed/accepted")
            if contract["bound_test_source_path"] is None:
                if contract["hash_binding_status"] == "source_row_match":
                    if contract["reported_sha256"] != digest:
                        raise CensusError(f"merged row {index}: source check contract hash binding mismatch")
                elif set(contract) == legacy_contract_keys:
                    raise CensusError(f"merged row {index}: legacy source check contract hash binding mismatch")
                elif contract["hash_binding_status"] == "no_file_locator_in_contract":
                    if contract["sha256_current_at_merge"] != digest:
                        raise CensusError(f"merged row {index}: unlocated source contract hash binding mismatch")
                else:
                    raise CensusError(f"merged row {index}: invalid source check contract status")
            else:
                canonical_repo_path(contract["bound_test_source_path"], f"merged row {index}: direct-test path")
                if contract["hash_binding_status"] == "match" and contract["reported_sha256"] != contract["sha256_current_at_merge"]:
                    raise CensusError(f"merged row {index}: direct-test file hash binding mismatch")
                if contract["hash_binding_status"] not in {"match", "no_review_hash_pin_locator_only"}:
                    raise CensusError(f"merged row {index}: invalid direct-test hash binding status")
        routes = row["route_expectations"]
        if not isinstance(routes, dict) or set(routes) != {"default", "explicit_mir", "source_class_used_as_route_oracle"}:
            raise CensusError(f"merged row {index}: invalid route axis schema")
        if routes["source_class_used_as_route_oracle"] is not False:
            raise CensusError(f"merged row {index}: route derived from source class")
        for axis in ("default", "explicit_mir"):
            route = routes[axis]
            if not isinstance(route, dict):
                raise CensusError(f"merged row {index}: invalid {axis} route schema")
            minimal_route_keys = {"expectation", "status", "accepted_as_route_pass", "basis"}
            adjudicated_candidate_keys = {
                "expectation", "expected_result", "exact_command", "status", "accepted_as_route_pass", "normative_basis"
            }
            if set(route) not in (minimal_route_keys, adjudicated_candidate_keys):
                raise CensusError(f"merged row {index}: invalid {axis} route schema")
            allowed_expectations = {
                "not_separately_adjudicated", "blocked_unresolved", "source_error_preempts_route",
                "valid_source_route_unassessed", "candidate_route_contract_recorded",
            }
            if route["expectation"] not in allowed_expectations or route["status"] != "not_run" or route["accepted_as_route_pass"] is not False:
                raise CensusError(f"merged row {index}: route axis was promoted or run")
            if set(route) == minimal_route_keys:
                if not isinstance(route["basis"], str) or not route["basis"]:
                    raise CensusError(f"merged row {index}: route basis must be a string")
            else:
                if not isinstance(route["expected_result"], str) or not isinstance(route["exact_command"], str):
                    raise CensusError(f"merged row {index}: route result/command must be strings")
                if axis == "default" and "--mir" in route["exact_command"].split():
                    raise CensusError(f"merged row {index}: default route command contains --mir")
                if axis == "explicit_mir" and "--mir" not in route["exact_command"].split():
                    raise CensusError(f"merged row {index}: explicit MIR route command is missing --mir")
                if row["source_path"] not in route["exact_command"]:
                    raise CensusError(f"merged row {index}: route command is not source-bound")
                if not isinstance(route["normative_basis"], list):
                    raise CensusError(f"merged row {index}: route normative_basis must be a list")
        if row["status"] != "not_run" or row["accepted"] is not False or row["accepted_as_route_pass"] is not False:
            raise CensusError(f"merged row {index}: status/acceptance flags changed")
        if row["bootstrap_or_runtime_observation_used"] is not False or row["mimi_or_cargo_or_test_or_build_executed"] is not False:
            raise CensusError(f"merged row {index}: observation/execution flags changed")
    if seen_indices != set(range(1, COUNT + 1)):
        raise CensusError("merged rows do not form exact 1..1187 roster")

    summary = strict_json_loads((package_dir / "summary.json").read_bytes(), "summary.json")
    if summary != _build_summary(census_rows):
        raise CensusError("summary does not exactly match census rows and their separate route axes")
    if (package_dir / "README.md").read_bytes() != _build_readme(
        computed_locator_audit, manifest["locator_refreshes"]
    ):
        raise CensusError("README does not match the bound locator projection and candidate contract")

    registry = BoundFileRegistry.create(repo_root)
    locators, actual_refreshes = read_locator_roster(raw_inputs[0], registry)
    if manifest["locator_refreshes"] != actual_refreshes:
        raise CensusError("manifest locator refreshes do not match live r2-to-source hash comparison")
    shard_paths = input_paths[1:10]
    shard_rows, shard_snapshots = _load_shard_rows(
        shard_paths,
        repo_root=repo_root,
        frozen_schema=(manifest["input_mode"] == "canonical_frozen"),
    )
    actual_schema_counts = dict(sorted(Counter(
        row.review_schema if isinstance(row, ParsedShardRow) else "synthetic_legacy_row"
        for row in shard_rows.values()
    ).items()))
    if manifest["full_review_schema_counts"] != actual_schema_counts:
        raise CensusError("manifest full-review schema counts do not match bound shards")
    for shard_path, snapshot in shard_snapshots.items():
        raw_index = shard_paths.index(shard_path) + 1
        if snapshot != raw_inputs[raw_index]:
            raise CensusError(f"bound shard bytes changed while validating: {shard_path}")
    rebuilt_rows = [
        _normalize_row(index, shard_rows[index], locators[index - 1], registry)
        for index in range(1, COUNT + 1)
    ]
    if census_rows != rebuilt_rows:
        raise CensusError("census rows do not exactly match the bound independent shards and live hashes")

    bound_hashes = registry.snapshots()
    expected_bound_records = [registry.by_path[path] for path in sorted(registry.by_path)]
    if manifest["bound_repo_files"] != expected_bound_records:
        raise CensusError("manifest bound repo files do not exactly match referenced source/spec/test files")

    for input_path, raw in zip(input_paths, raw_inputs, strict=True):
        if read_external_regular(input_path, "post-validation input recheck") != raw:
            raise CensusError(f"input bytes changed while validating: {input_path}")
    if validate_live_bindings:
        post_registry = BoundFileRegistry.create(repo_root)
        for relative, expected in bound_hashes.items():
            _, current = post_registry.read(relative, "post-validation live binding")
            if current != expected:
                raise CensusError(f"live source/spec/test hash drift: {relative}")
    return manifest


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--locator-ledger", required=True, type=Path)
    parser.add_argument("--shard", action="append", required=True, type=Path, help="repeat exactly nine times in roster order")
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        manifest = build_candidate_package(
            locator_ledger=args.locator_ledger,
            shard_paths=args.shard,
            repo_root=args.repo_root,
            output_dir=args.output_dir,
            args_record={
                "locator_ledger": str(args.locator_ledger),
                "shards": [str(path) for path in args.shard],
                "repo_root": str(args.repo_root.resolve()),
                "output_dir": str(args.output_dir.absolute()),
            },
        )
    except (CensusError, OSError) as error:
        print(f"full census merge rejected: {error}", file=sys.stderr)
        return 2
    print(
        f"candidate package written: {args.output_dir} rows={manifest['row_count']} "
        "accepted=false not_run=true"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
