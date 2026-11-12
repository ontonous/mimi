#!/usr/bin/env python3
"""Render default-route Rust authorities from independent, source-bound claims.

This renderer deliberately does not accept or read a route census report. The
caller supplies a schema-v1 JSON document with one entry for every current
Mimi source path. Class, diagnostic, route-reason, and failure expectations
come only from that document. The renderer reads source files only to confirm
the current denominator and SHA-256 identities, then emits an external bundle
containing direct Rust tests, schema-v4 expectations, and an input/source-hash
manifest.

Example input shape (all current Mimi paths must be present):

    {
      "schema_version": 1,
      "inputs": {
        "tests/real_world/example.mimi": {
          "class": "required_compile",
          "reason": "example must compile through the default canonical route",
          "source_sha256": "<current lowercase SHA-256>"
        },
        "tests/real_world/rejected.mimi": {
          "class": "expected_build_rejection",
          "reason": "the declared native capability is intentionally unavailable",
          "source_sha256": "<current lowercase SHA-256>",
          "expected_exit_code": 1,
          "expected_route_disposition": "rejected",
          "expected_route_classification_status": "observed",
          "expected_route_reason": "<exact single-line route reason>",
          "expected_failure_class": "capability",
          "expected_diagnostic_codes": ["MIR-RECEIPT-001"],
          "required_output_substrings": [
            "MIR-RECEIPT-001",
            "default Canonical MIR route rejected: <exact single-line route reason>"
          ]
        },
        "tests/real_world/source-error.mimi": {
          "class": "expected_source_diagnostic",
          "reason": "the exact source diagnostic is independently expected",
          "source_sha256": "<current lowercase SHA-256>",
          "expected_stderr": "error[E0000]: ...\\n",
          "expected_exit_code": 1,
          "expected_diagnostic_lines": ["error[E0000]: ..."],
          "expected_diagnostic_codes": ["E0000"],
          "expected_stdout_empty": true,
          "expected_output_absent": true,
          "expected_output_path": "target/route-census-source-diagnostic-<derived>.out"
        },
        "tests/real_world/pending.mimi": {
          "class": "unmapped",
          "reason": "independent semantic authorization is pending",
          "source_sha256": "<current lowercase SHA-256>"
        }
      }
    }

Only the three direct-authority classes above generate tests. Other recognized
expectation classes, plus ``unresolved``, remain row-level review decisions
with a blocked ``authority_status`` and are omitted from the schema-v4
``inputs`` object, so an authority gap is not folded into a schema class or
represented as an accepted expectation.

An optional ``--fixture-adjudication-jsonl`` overlays the explicitly pinned
19-row independent fixture adjudication. Its classes and direct-contract text
are retained in review rows. Those rows stay blocked until the richer direct
contracts can be represented exactly; no class is inferred from route output.

The output directory must be new and outside the repository. Rendering stages
all files beside that directory and publishes them with one rename. This tool
does not invoke Mimi, Cargo, or tests.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import sys
import tempfile
from typing import Any, Mapping, Sequence


EXPECTED_CURRENT_DENOMINATOR = 1187
INPUT_SCHEMA_VERSION = 1
OUTPUT_EXPECTATIONS_SCHEMA_VERSION = 4
EXPECTED_AUTHORITY_PATH = "tests/route-census/authorities.rs"
RUST_SOURCE_FILENAME = "authorities.rs"
EXPECTATIONS_FILENAME = "route-census-expectations.json"
MANIFEST_FILENAME = "route-census-generation-manifest.json"
FIXTURE_ADJUDICATION_SHA256 = "e09682a7d12dae54062b62485d8ae265ff44bdbd30a3f3921a1cc6b7d2746cd9"
FIXTURE_ADJUDICATION_REPORT_ID = "fixture-blocker-independent-adjudication-20261111"
FIXTURE_ADJUDICATION_ROW_COUNT = 19
DIRECT_CLASSES = frozenset({
    "required_compile",
    "expected_build_rejection",
    "expected_source_diagnostic",
})
UNRESOLVED_CLASS = "unresolved"
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
ROUTE_REJECTION_PREFIXES = (
    "default Canonical MIR route rejected: ",
    "canonical MIR build rejected: ",
)


class RendererError(ValueError):
    """Raised when the independent manifest cannot authorize a safe render."""


def _sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _reject_duplicate_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise RendererError(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def _reject_constant(value: str) -> None:
    raise RendererError(f"non-finite JSON number is not allowed: {value}")


def _load_module(path: Path, module_name: str) -> Any:
    if not path.is_file() or path.is_symlink():
        raise RendererError(f"required source module is missing or not regular: {path}")
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RendererError(f"cannot load source module: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _load_inputs(path: Path) -> tuple[dict[str, Any], bytes]:
    try:
        raw = path.read_bytes()
        document = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_pairs,
            parse_constant=_reject_constant,
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RendererError(f"cannot read valid UTF-8 JSON expectations: {error}") from error
    if (
        not isinstance(document, dict)
        or not isinstance(document.get("schema_version"), int)
        or isinstance(document.get("schema_version"), bool)
        or document.get("schema_version") != INPUT_SCHEMA_VERSION
        or not isinstance(document.get("inputs"), dict)
    ):
        raise RendererError(
            f"independent expectations must be an object with schema_version: {INPUT_SCHEMA_VERSION} and an inputs object"
        )
    return document, raw


def _normalize_input_path(census: Any, value: Any) -> str:
    try:
        normalized = census._repository_relative_path(value, label="expectation path")
    except (TypeError, ValueError) as error:
        raise RendererError(str(error)) from error
    path = PurePosixPath(normalized)
    if path.as_posix() != normalized or path.suffix != ".mimi":
        raise RendererError(f"expectation path must be a normalized .mimi path: {value!r}")
    return normalized


def _validate_inventory(root: Path, census: Any, generator: Any) -> list[dict[str, Any]]:
    sources = census.enumerate_sources(root)
    if len(sources) != EXPECTED_CURRENT_DENOMINATOR:
        raise RendererError(
            f"current Mimi denominator is {len(sources)}, expected exactly {EXPECTED_CURRENT_DENOMINATOR}"
        )
    paths = [str(source.get("path", "")) for source in sources]
    if len(set(paths)) != EXPECTED_CURRENT_DENOMINATOR:
        raise RendererError("current Mimi denominator contains duplicate paths")
    for source in sources:
        path = str(source.get("path", ""))
        try:
            normalized = generator._normalized_repo_path(path, label="inventory path")
        except (TypeError, ValueError) as error:
            raise RendererError(str(error)) from error
        if normalized != path or not path.endswith(".mimi"):
            raise RendererError(f"current inventory path is not a normalized Mimi path: {path!r}")
        if (
            source.get("present") is not True
            or source.get("presence_kind") != "regular_file"
            or source.get("buildable") is not True
            or source.get("target_kind") not in {"executable", "shared_library_probe"}
            or not isinstance(source.get("sha256"), str)
            or SHA256_RE.fullmatch(str(source.get("sha256"))) is None
        ):
            raise RendererError(f"current candidate is missing, symlinked, unbuildable, or unhashed: {path}")
        if generator._source_file_sha256(root, path) != source["sha256"]:
            raise RendererError(f"source changed while inventory was being read: {path}")
    return sources


def _validate_exact_input_coverage(
    document: Mapping[str, Any], sources: Sequence[Mapping[str, Any]], census: Any
) -> dict[str, Mapping[str, Any]]:
    raw_inputs = document.get("inputs")
    if not isinstance(raw_inputs, Mapping):
        raise RendererError("independent expectations must contain an inputs object")
    inputs: dict[str, Mapping[str, Any]] = {}
    for raw_path, raw_expectation in raw_inputs.items():
        path = _normalize_input_path(census, raw_path)
        if path in inputs:
            raise RendererError(f"duplicate normalized expectation path: {path!r}")
        if not isinstance(raw_expectation, Mapping):
            raise RendererError(f"expectation {path!r} must be an object")
        inputs[path] = raw_expectation
    inventory_paths = {str(source["path"]) for source in sources}
    missing = sorted(inventory_paths - set(inputs))
    extra = sorted(set(inputs) - inventory_paths)
    if missing or extra:
        detail = []
        if missing:
            detail.append(f"missing {len(missing)} paths (first: {missing[:5]!r})")
        if extra:
            detail.append(f"extra {len(extra)} paths (first: {extra[:5]!r})")
        raise RendererError("independent expectation keys must exactly equal the current denominator: " + "; ".join(detail))
    source_by_path = {str(source["path"]): source for source in sources}
    for path, expectation in inputs.items():
        expectation_class = expectation.get("class")
        if not isinstance(expectation_class, str):
            raise RendererError(f"expectation {path!r} class must be a string")
        recognized = set(getattr(census, "EXPECTATION_CLASSES", set()))
        if expectation_class not in recognized and expectation_class != UNRESOLVED_CLASS:
            raise RendererError(
                f"expectation {path!r} class {expectation_class!r} is not a recognized schema-v4 class"
            )
        source_sha = expectation.get("source_sha256")
        if not isinstance(source_sha, str) or SHA256_RE.fullmatch(source_sha) is None:
            raise RendererError(f"expectation {path!r} must include its current lowercase source_sha256")
        if source_sha != source_by_path[path].get("sha256"):
            raise RendererError(f"expectation {path!r} source_sha256 does not match the current source")
        reason = expectation.get("reason")
        if not isinstance(reason, str) or not reason.strip():
            raise RendererError(f"expectation {path!r} must include a non-empty reason")
    return inputs


def _load_fixture_adjudication(
    path: Path, sources: Sequence[Mapping[str, Any]], census: Any
) -> tuple[dict[str, Any], dict[str, Mapping[str, Any]], bytes]:
    """Load the pinned independent 19-row fixture decision file, never _review."""
    path = Path(os.path.abspath(path))
    try:
        metadata = path.lstat()
        raw = path.read_bytes()
    except OSError as error:
        raise RendererError(f"fixture adjudication JSONL is unavailable: {path}") from error
    if not stat.S_ISREG(metadata.st_mode) or path.is_symlink():
        raise RendererError("fixture adjudication input must be a regular non-symlink file")
    digest = _sha256(raw)
    if digest != FIXTURE_ADJUDICATION_SHA256:
        raise RendererError(
            "fixture adjudication bytes do not match the independently supplied pinned SHA-256"
        )
    try:
        records = [
            json.loads(
                line.decode("utf-8"),
                object_pairs_hook=_reject_duplicate_pairs,
                parse_constant=_reject_constant,
            )
            for line in raw.splitlines()
            if line.strip()
        ]
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RendererError(f"fixture adjudication JSONL is malformed: {error}") from error
    if len(records) != FIXTURE_ADJUDICATION_ROW_COUNT + 1:
        raise RendererError("fixture adjudication must contain one metadata row and exactly 19 decision rows")
    meta = records[0]
    if (
        not isinstance(meta, Mapping)
        or meta.get("record_type") != "report_meta"
        or meta.get("report_id") != FIXTURE_ADJUDICATION_REPORT_ID
        or meta.get("selected_rows") != FIXTURE_ADJUDICATION_ROW_COUNT
        or meta.get("strict_authority") is not False
        or not isinstance(meta.get("denominator"), Mapping)
        or meta["denominator"].get("rows") != EXPECTED_CURRENT_DENOMINATOR
    ):
        raise RendererError("fixture adjudication metadata does not match the reviewed 19-row independent report")
    source_by_path = {str(source["path"]): source for source in sources}
    adjudicated: dict[str, Mapping[str, Any]] = {}
    sequences: set[int] = set()
    observed_class_counts: dict[str, int] = {}
    for row in records[1:]:
        if not isinstance(row, Mapping) or row.get("record_type") != "fixture_adjudication":
            raise RendererError("fixture adjudication contains a non-decision row after its metadata")
        path_value = row.get("path")
        path_key = _normalize_input_path(census, path_value)
        if path_key in adjudicated:
            raise RendererError(f"fixture adjudication duplicates source path {path_key!r}")
        if not path_key.startswith("tests/fixtures/") or path_key.startswith("tests/fixtures/v02_app/"):
            raise RendererError(f"fixture adjudication path is outside its explicit scope: {path_key}")
        if path_key not in source_by_path:
            raise RendererError(f"fixture adjudication path is outside the current denominator: {path_key}")
        current_sha = str(source_by_path[path_key].get("sha256"))
        if (
            row.get("source_sha256_current") != current_sha
            or row.get("source_sha256_denominator") != current_sha
        ):
            raise RendererError(f"fixture adjudication source hash is stale for {path_key}")
        expectation_class = row.get("class")
        if (
            not isinstance(expectation_class, str)
            or (
                expectation_class not in census.EXPECTATION_CLASSES
                and expectation_class != UNRESOLVED_CLASS
            )
        ):
            raise RendererError(f"fixture adjudication class is not recognized for {path_key}")
        direct_contract = row.get("direct_contract")
        if not isinstance(direct_contract, str) or not direct_contract.strip():
            raise RendererError(f"fixture adjudication has no explicit direct_contract for {path_key}")
        normative_basis = row.get("normative_basis")
        if not isinstance(normative_basis, list) or not normative_basis or any(
            not isinstance(item, Mapping)
            or not isinstance(item.get("basis"), str)
            or not isinstance(item.get("path"), str)
            or not isinstance(item.get("lines"), str)
            for item in normative_basis
        ):
            raise RendererError(f"fixture adjudication normative basis is incomplete for {path_key}")
        sequence = row.get("sequence")
        if not isinstance(sequence, int) or isinstance(sequence, bool) or sequence < 1:
            raise RendererError(f"fixture adjudication sequence is invalid for {path_key}")
        if sequence in sequences:
            raise RendererError(f"fixture adjudication repeats sequence number {sequence}")
        sequences.add(sequence)
        unresolved_reason = row.get("unresolved_reason")
        if unresolved_reason is not None and not isinstance(unresolved_reason, str):
            raise RendererError(f"fixture adjudication unresolved_reason is malformed for {path_key}")
        adjudicated[path_key] = row
        observed_class_counts[expectation_class] = observed_class_counts.get(expectation_class, 0) + 1
    if sequences != set(range(1, FIXTURE_ADJUDICATION_ROW_COUNT + 1)):
        raise RendererError("fixture adjudication sequence numbers must cover 1 through 19")
    if len(adjudicated) != FIXTURE_ADJUDICATION_ROW_COUNT:
        raise RendererError("fixture adjudication does not cover exactly 19 unique paths")
    if meta.get("class_counts") != observed_class_counts or meta.get("classes") != observed_class_counts:
        raise RendererError("fixture adjudication metadata class counts do not match its explicit rows")
    return dict(meta), adjudicated, raw


def _merge_fixture_adjudication(
    document: Mapping[str, Any],
    adjudicated: Mapping[str, Mapping[str, Any]],
) -> tuple[dict[str, Any], dict[str, str | None]]:
    """Overlay only the explicit 19 decisions and retain any prior class for review."""
    raw_inputs = document.get("inputs")
    if not isinstance(raw_inputs, Mapping):
        raise RendererError("independent expectations must contain an inputs object")
    merged = dict(document)
    merged_inputs: dict[str, Any] = {
        key: dict(value) if isinstance(value, Mapping) else value
        for key, value in raw_inputs.items()
    }
    prior_classes: dict[str, str | None] = {}
    for path, row in adjudicated.items():
        previous = merged_inputs.get(path)
        if previous is not None and not isinstance(previous, Mapping):
            raise RendererError(f"base expectation for adjudicated path {path!r} is not an object")
        previous = dict(previous) if isinstance(previous, Mapping) else {}
        previous_sha = previous.get("source_sha256")
        authoritative_sha = row["source_sha256_current"]
        if previous_sha is not None and previous_sha != authoritative_sha:
            raise RendererError(f"base expectation source hash conflicts with explicit adjudication for {path}")
        previous_class = previous.get("class")
        prior_classes[path] = previous_class if isinstance(previous_class, str) else None
        previous.update({
            "class": row["class"],
            "reason": row["direct_contract"],
            "source_sha256": authoritative_sha,
        })
        merged_inputs[path] = previous
    merged["inputs"] = merged_inputs
    return merged, prior_classes


def _expectation_test_id(generator: Any, index: int, path: str, used: set[str]) -> str:
    test_id = generator._test_id(index, path)
    if test_id in used:
        raise RendererError(f"duplicate generated direct-test id: {test_id}")
    used.add(test_id)
    return test_id


def _validate_required_compile(
    *, census: Any, generator: Any, source: Mapping[str, Any], test_id: str
) -> tuple[str, int]:
    path = str(source["path"])
    output_path = f"target/route-census-authorities-{int(source['index']):04d}.out"
    rust_test = generator._render_required_compile_test(source, test_id, output_path)
    assertion_line = generator._line_number_for_literal(
        rust_test, "canonical route disposition: canonical ("
    )
    if not census._required_compile_test_authority_is_valid(
        test_id=test_id,
        source_path=path,
        target_kind=str(source["target_kind"]),
        source_text=rust_test,
        assertion_line=assertion_line,
    ):
        raise RendererError(f"{path}: canonical direct test failed the current static recognizer")
    return rust_test, assertion_line


def _validate_expected_rejection(
    *, census: Any, generator: Any, source: Mapping[str, Any], test_id: str, expectation: Mapping[str, Any]
) -> tuple[str, int, list[str], list[str]]:
    path = str(source["path"])
    expected_exit_code = expectation.get("expected_exit_code")
    route_disposition = expectation.get("expected_route_disposition")
    route_status = expectation.get("expected_route_classification_status")
    route_reason = expectation.get("expected_route_reason")
    failure_class = expectation.get("expected_failure_class")
    codes = expectation.get("expected_diagnostic_codes")
    required = expectation.get("required_output_substrings")
    if (
        not isinstance(expected_exit_code, int)
        or isinstance(expected_exit_code, bool)
        or expected_exit_code <= 0
    ):
        raise RendererError(f"{path}: expected_build_rejection requires a positive expected_exit_code")
    if route_disposition != "rejected" or route_status != "observed":
        raise RendererError(f"{path}: expected_build_rejection must explicitly declare rejected/observed route fields")
    if (
        not isinstance(route_reason, str)
        or not route_reason
        or route_reason != route_reason.strip()
        or "\n" in route_reason
        or "\r" in route_reason
    ):
        raise RendererError(f"{path}: expected_route_reason must be exact, trimmed, and single-line")
    if failure_class not in {"capability", "library_target"}:
        raise RendererError(f"{path}: expected_failure_class must be capability or library_target")
    if (
        not isinstance(codes, list)
        or not codes
        or any(not isinstance(code, str) or not census.DIAGNOSTIC_CODE_FULL_RE.fullmatch(code) for code in codes)
    ):
        raise RendererError(f"{path}: expected_diagnostic_codes must be a non-empty list of recognized codes")
    normalized_codes = [code.upper() for code in codes]
    if len(set(normalized_codes)) != len(normalized_codes):
        raise RendererError(f"{path}: expected_diagnostic_codes contains duplicates")
    if not isinstance(required, list) or not required or any(not isinstance(item, str) or not item for item in required):
        raise RendererError(f"{path}: required_output_substrings must be a non-empty list of exact text")
    if len(set(required)) != len(required):
        raise RendererError(f"{path}: required_output_substrings contains duplicates")
    route_literals = [
        item for item in required
        if any(item.startswith(prefix) for prefix in ROUTE_REJECTION_PREFIXES)
    ]
    if len(route_literals) != 1 or not any(
        route_literals[0] == prefix + route_reason for prefix in ROUTE_REJECTION_PREFIXES
    ):
        raise RendererError(
            f"{path}: required_output_substrings must contain exactly one route-rejection line with the exact expected_route_reason"
        )
    output_path = f"target/route-census-authorities-{int(source['index']):04d}.out"
    rust_test, assertion_line = generator._render_expected_rejection_test(
        source,
        test_id,
        output_path,
        route_reason,
        normalized_codes,
        required,
    )
    if not census._expected_rejection_test_authority_is_valid(
        test_id=test_id,
        source_path=path,
        target_kind=str(source["target_kind"]),
        source_text=rust_test,
        assertion_line=assertion_line,
        expected_diagnostic_codes=sorted(normalized_codes),
        required_output_substrings=required,
        expected_route_reason=route_reason,
    ):
        raise RendererError(f"{path}: rejection direct test failed the current static recognizer")
    return rust_test, assertion_line, sorted(normalized_codes), list(required)


def _validate_source_diagnostic(
    *, census: Any, generator: Any, source: Mapping[str, Any], test_id: str,
    expectation: Mapping[str, Any], root: Path
) -> tuple[str, int, list[str], list[str], str]:
    path = str(source["path"])
    expected_stderr = expectation.get("expected_stderr")
    expected_exit_code = expectation.get("expected_exit_code")
    expected_lines = expectation.get("expected_diagnostic_lines")
    expected_codes = expectation.get("expected_diagnostic_codes")
    output_absent = expectation.get("expected_output_absent")
    stdout_empty = expectation.get("expected_stdout_empty")
    expected_output_path = expectation.get("expected_output_path")
    source_sha = str(source["sha256"])
    derived_output_path = generator._source_diagnostic_authority_output_path(path, source_sha)
    if not isinstance(expected_stderr, str) or not expected_stderr:
        raise RendererError(f"{path}: expected_source_diagnostic requires exact expected_stderr text")
    if (
        not isinstance(expected_exit_code, int)
        or isinstance(expected_exit_code, bool)
        or expected_exit_code <= 0
    ):
        raise RendererError(f"{path}: expected_source_diagnostic requires a positive expected_exit_code")
    if stdout_empty is not True or output_absent is not True:
        raise RendererError(
            f"{path}: expected_source_diagnostic must explicitly require empty stdout and an absent output artifact"
        )
    if expected_output_path != derived_output_path:
        raise RendererError(
            f"{path}: expected_output_path must exactly equal the deterministic source-bound path {derived_output_path!r}"
        )
    if not census._source_diagnostic_text_is_well_formed(
        expected_stderr, source_path=path, root=root
    ):
        raise RendererError(f"{path}: expected_stderr is not a complete plain-text diagnostic for this exact source")
    actual_lines = census._source_diagnostic_lines(expected_stderr)
    actual_codes = census._diagnostic_codes(expected_stderr)
    if (
        not isinstance(expected_lines, list)
        or any(not isinstance(line, str) for line in expected_lines)
        or expected_lines != actual_lines
    ):
        raise RendererError(f"{path}: expected_diagnostic_lines must exactly match expected_stderr")
    if (
        not isinstance(expected_codes, list)
        or any(not isinstance(code, str) for code in expected_codes)
        or [code.upper() for code in expected_codes] != actual_codes
    ):
        raise RendererError(f"{path}: expected_diagnostic_codes must exactly match expected_stderr")
    rust_test, assertion_line = census._render_expected_source_diagnostic_test(
        test_id=test_id,
        source_path=path,
        source_sha256=source_sha,
        expected_exit_code=expected_exit_code,
        target_kind=str(source["target_kind"]),
        output_path=derived_output_path,
        expected_diagnostic_text=expected_stderr,
        root=root,
    )
    if not census._expected_source_diagnostic_test_authority_is_valid(
        test_id=test_id,
        source_path=path,
        source_sha256=source_sha,
        expected_exit_code=expected_exit_code,
        target_kind=str(source["target_kind"]),
        output_path=derived_output_path,
        expected_diagnostic_text=expected_stderr,
        expected_diagnostic_codes=actual_codes,
        expected_diagnostic_lines=actual_lines,
        source_text=rust_test,
        assertion_line=assertion_line,
        root=root,
    ):
        raise RendererError(f"{path}: source-diagnostic direct test failed the current static recognizer")
    return rust_test, assertion_line, actual_codes, actual_lines, derived_output_path


def _json_bytes(document: Any) -> bytes:
    return (json.dumps(document, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def _write_staged_file(directory: Path, name: str, raw: bytes) -> None:
    path = directory / name
    with path.open("xb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def render(
    *, root: Path, expectations_path: Path, output_dir: Path,
    fixture_adjudication_path: Path | None = None,
) -> dict[str, Any]:
    root = root.resolve(strict=True)
    if not root.is_dir():
        raise RendererError(f"repository root is not a directory: {root}")
    expectations_path = Path(os.path.abspath(expectations_path))
    try:
        input_metadata = expectations_path.lstat()
    except OSError as error:
        raise RendererError(f"independent expectations input is unavailable: {expectations_path}") from error
    if not stat.S_ISREG(input_metadata.st_mode) or expectations_path.is_symlink():
        raise RendererError("independent expectations input must be a regular non-symlink file")
    expectations_path = expectations_path.resolve(strict=True)
    output_dir = Path(os.path.abspath(output_dir))
    if output_dir.exists() or output_dir.is_symlink():
        raise RendererError(f"refusing to overwrite output directory: {output_dir}")
    if not output_dir.parent.is_dir() or output_dir.parent.is_symlink():
        raise RendererError(f"output parent must be an existing regular directory: {output_dir.parent}")
    output_parent = output_dir.parent.resolve(strict=True)
    output_dir = output_parent / output_dir.name
    try:
        output_dir.relative_to(root)
    except ValueError:
        pass
    else:
        raise RendererError("output directory must be outside the repository")

    generator_path = root / "scripts" / "generate_route_census_authority.py"
    census_path = root / "scripts" / "full_route_census.py"
    generator_initial_sha = _sha256(generator_path.read_bytes())
    census_initial_sha = _sha256(census_path.read_bytes())
    generator = _load_module(generator_path, "mimi_generate_route_census_authority_for_independent_renderer")
    census = _load_module(census_path, "mimi_full_route_census_for_independent_renderer")
    generator._validate_optional_cargo_target(root)
    wrapper_path = root / EXPECTED_AUTHORITY_PATH
    wrapper_sha = _sha256(wrapper_path.read_bytes())
    sources = _validate_inventory(root, census, generator)
    input_document, input_raw = _load_inputs(expectations_path)
    fixture_meta: dict[str, Any] | None = None
    fixture_rows: dict[str, Mapping[str, Any]] = {}
    fixture_raw: bytes | None = None
    prior_classes: dict[str, str | None] = {}
    fixture_path: Path | None = None
    if fixture_adjudication_path is not None:
        fixture_path = Path(os.path.abspath(fixture_adjudication_path))
        fixture_meta, fixture_rows, fixture_raw = _load_fixture_adjudication(
            fixture_path, sources, census
        )
        input_document, prior_classes = _merge_fixture_adjudication(
            input_document, fixture_rows
        )
    inputs = _validate_exact_input_coverage(input_document, sources, census)
    effective_input_raw = _json_bytes(input_document)

    source_by_path = {str(source["path"]): dict(source) for source in sources}
    source_paths = [str(source["path"]) for source in sources]
    source_hashes = {path: str(source_by_path[path]["sha256"]) for path in source_paths}
    generated_inputs: dict[str, dict[str, Any]] = {}
    manifest_rows: list[dict[str, Any]] = []
    test_blocks: list[str] = []
    used_test_ids: set[str] = set()
    source_diag_output_paths: set[str] = set()

    for index, path in enumerate(source_paths, 1):
        source = source_by_path[path]
        source["index"] = index
        expectation = inputs[path]
        expectation_class = str(expectation["class"])
        reason = str(expectation["reason"])
        source_sha = source_hashes[path]
        if path in fixture_rows:
            adjudication = fixture_rows[path]
            authority_status = (
                "semantic_unresolved"
                if expectation_class == UNRESOLVED_CLASS
                else "missing_exact_contract"
            )
            manifest_rows.append({
                "path": path,
                "adjudicated_class": expectation_class,
                "authority_status": authority_status,
                "status": (
                    "semantic_unresolved"
                    if authority_status == "semantic_unresolved"
                    else "blocked_missing_exact_contract"
                ),
                "reason": reason,
                "source_sha256": source_sha,
                "test_id": None,
                "authority_generated": False,
                "direct_contract": adjudication["direct_contract"],
                "normative_basis": adjudication["normative_basis"],
                "unresolved_reason": adjudication.get("unresolved_reason"),
                "source_sha256_current": adjudication["source_sha256_current"],
                "source_sha256_denominator": adjudication["source_sha256_denominator"],
                "adjudication": {
                    "report_id": FIXTURE_ADJUDICATION_REPORT_ID,
                    "report_sha256": FIXTURE_ADJUDICATION_SHA256,
                    "sequence": adjudication["sequence"],
                    "input_class_before_adjudication": prior_classes.get(path),
                },
            })
            continue
        if expectation_class not in DIRECT_CLASSES:
            if expectation_class == UNRESOLVED_CLASS:
                authority_status = "semantic_unresolved"
                row_status = "blocking_unresolved"
            elif expectation_class == "unmapped":
                authority_status = "missing_exact_contract"
                row_status = "blocking_unmapped"
            else:
                authority_status = "missing_exact_contract"
                row_status = "blocking_without_direct_contract"
            manifest_rows.append({
                "path": path,
                "adjudicated_class": expectation_class,
                "authority_status": authority_status,
                "status": row_status,
                "reason": reason,
                "source_sha256": source_sha,
                "test_id": None,
                "authority_generated": False,
            })
            continue

        test_id = _expectation_test_id(generator, index, path, used_test_ids)
        output_expectation: dict[str, Any] = {
            "class": expectation_class,
            "reason": reason,
            "source_sha256": source_sha,
            "authority": None,
        }
        if expectation_class == "required_compile":
            rust_test, assertion_line = _validate_required_compile(
                census=census, generator=generator, source=source, test_id=test_id
            )
            assertion_literal = "canonical route disposition: canonical ("
            output_expectation["authority"] = {
                "test_id": test_id,
                "assertion": {
                    "path": EXPECTED_AUTHORITY_PATH,
                    "line": assertion_line,
                    "sha256": "",
                },
            }
            manifest_row = {
                "path": path,
                "adjudicated_class": expectation_class,
                "authority_status": "generated_not_executed",
                "status": "generated_not_executed",
                "reason": reason,
                "source_sha256": source_sha,
                "test_id": test_id,
                "assertion_line": assertion_line,
                "authority_generated": True,
                "direct_contract": "exact_default_build_canonical_markers_no_fallback",
            }
        elif expectation_class == "expected_build_rejection":
            rust_test, assertion_line, codes, required = _validate_expected_rejection(
                census=census,
                generator=generator,
                source=source,
                test_id=test_id,
                expectation=expectation,
            )
            assertion_literal = next(
                item for item in required
                if any(item.startswith(prefix) for prefix in ROUTE_REJECTION_PREFIXES)
            )
            output_expectation["authority"] = {
                "test_id": test_id,
                "assertion": {
                    "path": EXPECTED_AUTHORITY_PATH,
                    "line": assertion_line,
                    "sha256": "",
                },
            }
            for field in (
                "expected_exit_code",
                "expected_route_disposition",
                "expected_route_classification_status",
                "expected_route_reason",
                "expected_failure_class",
            ):
                output_expectation[field] = expectation[field]
            output_expectation["expected_diagnostic_codes"] = codes
            output_expectation["required_output_substrings"] = required
            manifest_row = {
                "path": path,
                "adjudicated_class": expectation_class,
                "authority_status": "generated_not_executed",
                "status": "generated_not_executed",
                "reason": reason,
                "source_sha256": source_sha,
                "test_id": test_id,
                "assertion_line": assertion_line,
                "authority_generated": True,
                "direct_contract": "exact_route_reason_diagnostic_codes_and_required_output_text",
                "independent_expected_values": {
                    "expected_exit_code": expectation["expected_exit_code"],
                    "expected_failure_class": expectation["expected_failure_class"],
                    "expected_route_reason": expectation["expected_route_reason"],
                    "expected_diagnostic_codes": codes,
                    "required_output_substrings": required,
                },
            }
        else:
            rust_test, assertion_line, codes, lines, output_path = _validate_source_diagnostic(
                census=census,
                generator=generator,
                source=source,
                test_id=test_id,
                expectation=expectation,
                root=root,
            )
            if output_path in source_diag_output_paths:
                raise RendererError(f"{path}: deterministic source-diagnostic output path was reused")
            source_diag_output_paths.add(output_path)
            assertion_literal = "output.stderr.as_slice()"
            output_expectation["authority"] = {
                "test_id": test_id,
                "assertion": {
                    "path": EXPECTED_AUTHORITY_PATH,
                    "line": assertion_line,
                    "sha256": "",
                },
            }
            output_expectation.update({
                "expected_exit_code": expectation["expected_exit_code"],
                "expected_diagnostic_text": expectation["expected_stderr"],
                "expected_diagnostic_lines": lines,
                "expected_diagnostic_codes": codes,
                "expected_output_path": output_path,
                "expected_stdout_empty": True,
                "expected_output_absent": True,
            })
            manifest_row = {
                "path": path,
                "adjudicated_class": expectation_class,
                "authority_status": "generated_not_executed",
                "status": "generated_not_executed",
                "reason": reason,
                "source_sha256": source_sha,
                "test_id": test_id,
                "assertion_line": assertion_line,
                "authority_generated": True,
                "direct_contract": "exact_stderr_exit_lines_codes_empty_stdout_and_output_absence",
                "independent_expected_values": {
                    "expected_exit_code": expectation["expected_exit_code"],
                    "expected_stderr_sha256": _sha256(expectation["expected_stderr"].encode("utf-8")),
                    "expected_diagnostic_lines": lines,
                    "expected_diagnostic_codes": codes,
                    "expected_stdout_empty": True,
                    "expected_output_absent": True,
                    "expected_output_path": output_path,
                },
            }
        # Every direct expectation is attached to the output Rust source digest
        # below, after all test blocks have been rendered.
        if assertion_literal not in rust_test:
            raise RendererError(f"{path}: generated test lost its cited assertion literal")
        test_blocks.append(rust_test.rstrip())
        generated_inputs[path] = output_expectation
        manifest_rows.append(manifest_row)

    rust_source = (
        "// Generated by scripts/render_independent_route_authorities.py.\n"
        "// Every mapped test derives from the caller's independent expectations manifest.\n"
        "// This bundle has not been executed and is not itself a passing census report.\n\n"
        + generator.AUTHORITY_MIMI_HELPER_SOURCE
        + "\n"
        + ("\n\n".join(test_blocks) + "\n" if test_blocks else "")
    )
    rust_raw = rust_source.encode("utf-8")
    rust_sha = _sha256(rust_raw)
    for expectation in generated_inputs.values():
        authority = expectation.get("authority")
        if isinstance(authority, dict):
            authority["assertion"]["sha256"] = rust_sha
    expectations_document = {
        "schema_version": OUTPUT_EXPECTATIONS_SCHEMA_VERSION,
        "inputs": generated_inputs,
    }
    expectations_raw = _json_bytes(expectations_document)

    # Revalidate all identities after rendering, immediately before staging.
    if _sha256(expectations_path.read_bytes()) != _sha256(input_raw):
        raise RendererError("independent expectations input changed while rendering")
    if fixture_path is not None and fixture_raw is not None:
        if _sha256(fixture_path.read_bytes()) != _sha256(fixture_raw):
            raise RendererError("fixture adjudication input changed while rendering")
    if _sha256(generator_path.read_bytes()) != generator_initial_sha:
        raise RendererError("authority generator changed while rendering")
    if _sha256(census_path.read_bytes()) != census_initial_sha:
        raise RendererError("route census collector changed while rendering")
    if _sha256(wrapper_path.read_bytes()) != wrapper_sha:
        raise RendererError("registered route authority wrapper changed while rendering")
    for path, expected_hash in source_hashes.items():
        if generator._source_file_sha256(root, path) != expected_hash:
            raise RendererError(f"current Mimi source changed while rendering: {path}")

    input_sha = _sha256(input_raw)
    inventory_rows = [
        {"path": path, "source_sha256": source_hashes[path], "target_kind": source_by_path[path]["target_kind"]}
        for path in source_paths
    ]
    inventory_sha = _sha256(_json_bytes(inventory_rows))
    class_counts: dict[str, int] = {}
    for row in manifest_rows:
        class_name = str(row["adjudicated_class"])
        class_counts[class_name] = class_counts.get(class_name, 0) + 1
    blocked_count = sum(row.get("authority_generated") is not True for row in manifest_rows)
    unmapped_count = sum(row.get("adjudicated_class") == "unmapped" for row in manifest_rows)
    unresolved_count = sum(row.get("adjudicated_class") == UNRESOLVED_CLASS for row in manifest_rows)
    no_contract_count = blocked_count - unmapped_count - unresolved_count
    generated_count = sum(row.get("authority_generated") is True for row in manifest_rows)
    manifest: dict[str, Any] = {
        "schema_version": 1,
        "status": "generated_not_executed",
        "generation_mode": "independent_manifest",
        "authorization_status": "not_executed",
        "accepted": False,
        "strict_census_ready": False,
        "expectation_coverage_complete": blocked_count == 0,
        "denominator": {
            "repository_root": str(root),
            "candidate_count": len(sources),
            "required_current_count": EXPECTED_CURRENT_DENOMINATOR,
            "paths_sha256": _sha256("\0".join(source_paths).encode("utf-8")),
            "inventory_sha256": inventory_sha,
            "source_hashes_included": True,
        },
        "independent_expectations": {
            "path": str(expectations_path),
            "sha256": input_sha,
            "bytes": len(input_raw),
            "schema_version": INPUT_SCHEMA_VERSION,
            "effective_merged_sha256": _sha256(effective_input_raw),
            "effective_merged_bytes": len(effective_input_raw),
        },
        "renderer": {
            "path": "scripts/render_independent_route_authorities.py",
            "sha256": _sha256(Path(__file__).resolve(strict=True).read_bytes()),
        },
        "recognizers": {
            "generator_path": "scripts/generate_route_census_authority.py",
            "generator_sha256": _sha256(generator_path.read_bytes()),
            "collector_path": "scripts/full_route_census.py",
            "collector_sha256": _sha256(census_path.read_bytes()),
        },
        "authority_target": {
            "path": EXPECTED_AUTHORITY_PATH,
            "name": generator.EXPECTED_TEST_NAME,
            "feature": generator.EXPECTED_FEATURE_NAME,
            "wrapper_sha256": wrapper_sha,
        },
        "outcomes": {
            "counts": dict(sorted(class_counts.items())),
            "direct_test_count": generated_count,
            "blocked_row_count": blocked_count,
            "unmapped_blocker_count": unmapped_count,
            "semantic_unresolved_count": unresolved_count,
            "missing_direct_contract_count": no_contract_count,
            "expectation_inputs_include_blockers": False,
            "all_direct_tests_static_recognizer_valid": True,
            "tests_executed": False,
        },
        "artifacts": {
            RUST_SOURCE_FILENAME: {"sha256": rust_sha, "bytes": len(rust_raw)},
            EXPECTATIONS_FILENAME: {"sha256": _sha256(expectations_raw), "bytes": len(expectations_raw)},
        },
        "rows": manifest_rows,
        "execution_contract": (
            "The declared classes and expected values came from the independent input manifest. "
            "Generated direct tests passed source-level static recognizers only; no Mimi, Cargo, or test command was run. "
            "A subsequent route census must execute and independently verify every generated direct test. "
            "Unmapped, unresolved, and other non-direct-contract rows remain blocked review decisions and are never accepted."
        ),
    }
    if fixture_path is not None and fixture_raw is not None and fixture_meta is not None:
        fixture_class_counts: dict[str, int] = {}
        for row in fixture_rows.values():
            class_name = str(row["class"])
            fixture_class_counts[class_name] = fixture_class_counts.get(class_name, 0) + 1
        manifest["independent_adjudication"] = {
            "path": str(fixture_path),
            "sha256": _sha256(fixture_raw),
            "bytes": len(fixture_raw),
            "report_id": FIXTURE_ADJUDICATION_REPORT_ID,
            "row_count": len(fixture_rows),
            "class_counts": dict(sorted(fixture_class_counts.items())),
            "denominator_path": fixture_meta["denominator"]["path"],
            "denominator_sha256": fixture_meta["denominator"]["sha256"],
            "decision_method": fixture_meta["decision_method"],
            "strict_authority_disclaimer": fixture_meta["strict_authority_disclaimer"],
            "strict_authority": False,
            "note": (
                "Rows are preserved as independent review decisions; their richer direct contracts are not represented by schema-v4 inputs until exact authority contracts are available."
            ),
        }
    manifest_raw = _json_bytes(manifest)

    if output_dir.exists() or output_dir.is_symlink():
        raise RendererError(f"output directory appeared while rendering: {output_dir}")
    stage = Path(tempfile.mkdtemp(prefix=f".{output_dir.name}.staging-", dir=str(output_parent)))
    try:
        _write_staged_file(stage, RUST_SOURCE_FILENAME, rust_raw)
        _write_staged_file(stage, EXPECTATIONS_FILENAME, expectations_raw)
        _write_staged_file(stage, MANIFEST_FILENAME, manifest_raw)
        if _sha256((stage / RUST_SOURCE_FILENAME).read_bytes()) != rust_sha:
            raise RendererError("staged Rust authority source hash changed before publication")
        if _sha256((stage / EXPECTATIONS_FILENAME).read_bytes()) != _sha256(expectations_raw):
            raise RendererError("staged expectations hash changed before publication")
        if _sha256((stage / MANIFEST_FILENAME).read_bytes()) != _sha256(manifest_raw):
            raise RendererError("staged generation manifest hash changed before publication")
        _fsync_directory(stage)
        if output_dir.exists() or output_dir.is_symlink():
            raise RendererError(f"output directory appeared before publication: {output_dir}")
        os.rename(stage, output_dir)
        _fsync_directory(output_parent)
    except BaseException:
        if stage.exists():
            shutil.rmtree(stage)
        raise
    return manifest


def _self_check(root: Path) -> None:
    """Run only the current static source recognizers on synthetic snippets."""
    root = root.resolve(strict=True)
    generator = _load_module(root / "scripts" / "generate_route_census_authority.py", "mimi_authority_generator_self_check")
    census = _load_module(root / "scripts" / "full_route_census.py", "mimi_route_census_self_check")
    source = {
        "path": "tests/real_world/static-recognizer-probe.mimi",
        "target_kind": "executable",
        "index": 1,
    }
    canonical_id = "route_census_self_check_canonical"
    canonical = generator._render_required_compile_test(
        source, canonical_id, "target/route-census-authorities-0001.out"
    )
    canonical_line = generator._line_number_for_literal(
        canonical, "canonical route disposition: canonical ("
    )
    if not census._required_compile_test_authority_is_valid(
        test_id=canonical_id,
        source_path=source["path"],
        target_kind=source["target_kind"],
        source_text=canonical,
        assertion_line=canonical_line,
    ):
        raise RendererError("canonical self-check failed")
    rejection_id = "route_census_self_check_rejection"
    route_reason = "self-check route reason"
    codes = ["MIR-RECEIPT-001"]
    required = [
        "MIR-RECEIPT-001",
        f"default Canonical MIR route rejected: {route_reason}",
    ]
    rejection, rejection_line = generator._render_expected_rejection_test(
        source,
        rejection_id,
        "target/route-census-authorities-0001.out",
        route_reason,
        codes,
        required,
    )
    if not census._expected_rejection_test_authority_is_valid(
        test_id=rejection_id,
        source_path=source["path"],
        target_kind=source["target_kind"],
        source_text=rejection,
        assertion_line=rejection_line,
        expected_diagnostic_codes=codes,
        required_output_substrings=required,
        expected_route_reason=route_reason,
    ):
        raise RendererError("rejection self-check failed")

    source_candidate = census.enumerate_sources(root)[0]
    source_path = str(source_candidate["path"])
    source_sha = str(source_candidate["sha256"])
    absolute_source = str((root / source_path).resolve())
    source_diagnostic = (
        f"{absolute_source} has 1 type error(s):\n"
        f"error[E0001] {absolute_source}:1:1 static recognizer self-check\n"
        "error: type checking failed\n"
    )
    source_diagnostic_id = "route_census_self_check_source_diagnostic"
    source_output_path = generator._source_diagnostic_authority_output_path(
        source_path, source_sha
    )
    source_lines = census._source_diagnostic_lines(source_diagnostic)
    source_codes = census._diagnostic_codes(source_diagnostic)
    source_test, source_line = census._render_expected_source_diagnostic_test(
        test_id=source_diagnostic_id,
        source_path=source_path,
        source_sha256=source_sha,
        expected_exit_code=1,
        target_kind=str(source_candidate["target_kind"]),
        output_path=source_output_path,
        expected_diagnostic_text=source_diagnostic,
        root=root,
    )
    if not census._expected_source_diagnostic_test_authority_is_valid(
        test_id=source_diagnostic_id,
        source_path=source_path,
        source_sha256=source_sha,
        expected_exit_code=1,
        target_kind=str(source_candidate["target_kind"]),
        output_path=source_output_path,
        expected_diagnostic_text=source_diagnostic,
        expected_diagnostic_codes=source_codes,
        expected_diagnostic_lines=source_lines,
        source_text=source_test,
        assertion_line=source_line,
        root=root,
    ):
        raise RendererError("source-diagnostic self-check failed")


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="Mimi repository root (default: this script's repository)",
    )
    parser.add_argument(
        "--expectations",
        type=Path,
        help="independent schema-v1 JSON keyed by the exact current Mimi paths",
    )
    parser.add_argument(
        "--fixture-adjudication-jsonl",
        type=Path,
        help=(
            "optional pinned independent 19-row fixture adjudication JSONL; its row classes override matching base inputs but remain blocked review rows unless exact direct contracts are supported"
        ),
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        help="new external directory for authorities.rs, schema-v4 expectations, and manifest",
    )
    parser.add_argument(
        "--self-check",
        action="store_true",
        help="exercise only the static Rust authority recognizers; do not read Mimi observations or write output",
    )
    args = parser.parse_args(argv)
    if args.self_check:
        if (
            args.expectations is not None
            or args.output_dir is not None
            or args.fixture_adjudication_jsonl is not None
        ):
            parser.error(
                "--self-check cannot be combined with --expectations, --fixture-adjudication-jsonl, or --output-dir"
            )
    elif args.expectations is None or args.output_dir is None:
        parser.error("--expectations and --output-dir are required unless --self-check is used")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.self_check:
            _self_check(args.root)
            print("independent route authority static recognizers: valid")
            return 0
        manifest = render(
            root=args.root,
            expectations_path=args.expectations,
            output_dir=args.output_dir,
            fixture_adjudication_path=args.fixture_adjudication_jsonl,
        )
    except (RendererError, OSError, ValueError) as error:
        print(f"independent route authority render failed: {error}", file=sys.stderr)
        return 2
    print(
        f"rendered {manifest['outcomes']['direct_test_count']} direct tests and "
        f"{manifest['outcomes']['blocked_row_count']} blocked review rows to {Path(args.output_dir).absolute()}"
    )
    print("status: generated_not_executed; accepted: false")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
