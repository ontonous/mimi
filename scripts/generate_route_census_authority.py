#!/usr/bin/env python3
"""Generate source-bound schema-v4 route expectations and direct Rust authorities.

The generator is intentionally fail-closed.  It consumes only a current,
full-execution evidence-collection report whose entire candidate inventory is
still present with the same hashes. Normal generation requires completed
route contracts. Explicit ``--bootstrap-report`` mode may instead consume raw
observations to generate reproducible direct tests for canonical builds and
contract-allowed route rejections while preserving every other candidate as a
source-bound blocker. Bootstrap artifacts are never themselves a passing
census report.

The Rust output is staged as
``tests/route-census/authorities.rs`` for the separately registered,
feature-gated ``route-census-authorities`` Cargo test target.  No generated
output is written until the full report and every row have passed validation.
"""

from __future__ import annotations

import argparse
import base64
from collections import Counter
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
import tomllib
import unicodedata
from typing import Any, Mapping, Sequence


REQUIRED_CENSUS_SCHEMA_VERSION = "0.1.11-full-route-census-v7"
EXPECTED_TEST_NAME = "route-census-authorities"
EXPECTED_FEATURE_NAME = "route-census-authorities"
EXPECTED_TEST_PATH = "tests/route-census/authorities.rs"
EXPECTED_WRAPPER_SOURCE = (
    "// The collector supplies the exact generated bundle source only after it has\n"
    "// validated the generation manifest, expectations, source hashes, and target.\n"
    "// Direct Cargo use with this feature and no validated bundle fails at compile\n"
    "// time because the environment variable is intentionally absent.\n"
    'include!(env!("MIMI_ROUTE_CENSUS_AUTHORITY_SOURCE"));\n'
)
AUTHORITY_SOURCE_ENV = "MIMI_ROUTE_CENSUS_AUTHORITY_SOURCE"
AUTHORITY_MIMI_PATH_ENV = "MIMI_ROUTE_CENSUS_MIMI"
AUTHORITY_MIMI_SHA256_ENV = "MIMI_ROUTE_CENSUS_MIMI_SHA256"
AUTHORITY_MIMI_CENSUS_ENV = "MIMI_ROUTE_CENSUS"
AUTHORITY_MIMI_MARKER = "MIMI_ROUTE_CENSUS_AUTHORITY_BINARY"
AUTHORITY_MIMI_HELPER_SOURCE = (
    "fn route_census_mimi_path() -> ::std::path::PathBuf {\n"
    f"    let census_mode = ::std::env::var({json.dumps(AUTHORITY_MIMI_CENSUS_ENV)}).expect(\"collector must enable strict route census authority mode\");\n"
    "    ::std::assert_eq!(census_mode, \"1\", \"route census authority mode must be exact\");\n"
    f"    let path = ::std::env::var({json.dumps(AUTHORITY_MIMI_PATH_ENV)}).expect(\"collector must provide validated Mimi path\");\n"
    f"    let sha256 = ::std::env::var({json.dumps(AUTHORITY_MIMI_SHA256_ENV)}).expect(\"collector must provide validated Mimi SHA-256\");\n"
    "    ::std::assert!(::std::path::Path::new(&path).is_absolute(), \"validated Mimi path must be absolute\");\n"
    "    ::std::assert_eq!(::std::path::Path::new(&path).canonicalize().expect(\"validated Mimi path must exist\"), ::std::path::PathBuf::from(&path), \"validated Mimi path must be canonical and symlink-free\");\n"
    "    ::std::assert!(sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)), \"validated Mimi SHA-256 must be lowercase hexadecimal\");\n"
    f"    ::std::println!(\"{AUTHORITY_MIMI_MARKER} path={{}} sha256={{}}\", path, sha256);\n"
    "    ::std::path::PathBuf::from(path)\n"
    "}\n"
)
EXPECTATIONS_FILENAME = "route-census-expectations.json"
GENERATION_MANIFEST_FILENAME = "route-census-generation-manifest.json"
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
IDENTIFIER_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")
CANONICAL_DIGEST_RE = re.compile(r"[0-9a-f]{64}\Z")
ROUTE_REJECTION_PREFIXES = (
    "default Canonical MIR route rejected: ",
    "canonical MIR build rejected: ",
)
CANONICAL_MIR_ROUTE_CODES = frozenset({
    "MIR-RECEIPT-001",
    "MIR-RECEIPT-MANIFEST-001",
    "MIR-MATERIALIZATION-001",
    "MIR-COVERAGE-001",
    "MIR-FFI-RECEIPT-001",
    "MIR-FFI-RECEIPT-MANIFEST-001",
    "MIR-FFI-DECLARATION-001",
})
SOURCE_DIAGNOSTIC_RE = re.compile(
    r"(?i)^[ \t]*error(?:\[[A-Z][A-Z0-9_-]*\])?(?:[ \t]+|[ \t]*:[ \t]+)"
    r"(?P<location>.*?\.mimi:\d+:\d+(?:-(?:\d+:)?\d+)?)(?:[ \t]+|$)"
)
SOURCE_DIAGNOSTIC_DETECT_RE = re.compile(
    r"(?i)error[^\r\n]*?\.mimi:\d+:\d+(?:-(?:\d+:)?\d+)?"
)
SOURCE_DIAGNOSTIC_CODE_RE = re.compile(
    r"(?i)^[ \t]*error[ \t]*\[[ \t]*(?P<code>E\d{4}|MIR-[A-Z0-9_-]+-\d{3,})[ \t]*\]"
)
SOURCE_DIAGNOSTIC_FORBIDDEN_MARKERS = (
    "canonical route disposition:",
    "default canonical mir route rejected:",
    "canonical mir build rejected:",
    "mimi_route_census",
    "fallback",
    "legacy",
    "compatibility",
)
SOURCE_DIAGNOSTIC_SUMMARY_RE = re.compile(
    r"(?i)[ \t]*error:[ \t]*type checking failed[ \t]*\Z"
)


class GenerationError(ValueError):
    """Raised when the census cannot safely authorize all current candidates."""


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _reject_duplicate_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise GenerationError(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def _reject_constant(value: str) -> None:
    raise GenerationError(f"non-finite JSON number is not allowed: {value}")


def _load_census_module(root: Path) -> Any:
    path = root / "scripts" / "full_route_census.py"
    if not path.is_file() or path.is_symlink():
        raise GenerationError(f"current route census collector is missing or not regular: {path}")
    spec = importlib.util.spec_from_file_location("mimi_full_route_census_for_authority", path)
    if spec is None or spec.loader is None:
        raise GenerationError(f"cannot load current route census collector: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _normalized_repo_path(value: Any, *, label: str) -> str:
    if not isinstance(value, str) or not value or "\\" in value or "\x00" in value:
        raise GenerationError(f"{label} must be a non-empty POSIX repository-relative path")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise GenerationError(f"{label} is not a normalized repository-relative path: {value!r}")
    normalized = path.as_posix()
    if normalized != value:
        raise GenerationError(f"{label} is not normalized: {value!r}")
    return normalized


def _source_file_sha256(root: Path, relative: str) -> str:
    path = root / relative
    try:
        metadata = path.lstat()
        resolved = path.resolve(strict=True)
        resolved.relative_to(root.resolve(strict=True))
        if not stat.S_ISREG(metadata.st_mode) or path.is_symlink():
            raise GenerationError(f"candidate is not a regular non-symlink file: {relative}")
        return _sha256(path.read_bytes())
    except (OSError, ValueError) as error:
        if isinstance(error, GenerationError):
            raise
        raise GenerationError(f"candidate is unavailable beneath repository root: {relative}") from error


def _validate_optional_cargo_target(root: Path) -> None:
    cargo_toml = root / "Cargo.toml"
    try:
        with cargo_toml.open("rb") as stream:
            manifest = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise GenerationError(f"cannot read valid Cargo.toml: {error}") from error

    package = manifest.get("package")
    if not isinstance(package, Mapping) or package.get("name") != "mimi":
        raise GenerationError("route census authority target must belong to the Mimi package")
    features = manifest.get("features")
    if (
        not isinstance(features, Mapping)
        or EXPECTED_FEATURE_NAME not in features
        or features.get(EXPECTED_FEATURE_NAME) != []
    ):
        raise GenerationError(
            f"Cargo.toml must define the empty optional feature {EXPECTED_FEATURE_NAME!r} before authority generation"
        )
    tests = manifest.get("test")
    if not isinstance(tests, list):
        raise GenerationError("Cargo.toml must declare the feature-gated route census integration test target")
    matching = [
        item for item in tests
        if isinstance(item, Mapping)
        and item.get("name") == EXPECTED_TEST_NAME
        and item.get("path") == EXPECTED_TEST_PATH
    ]
    colliding = [
        item for item in tests
        if isinstance(item, Mapping)
        and (item.get("name") == EXPECTED_TEST_NAME or item.get("path") == EXPECTED_TEST_PATH)
    ]
    if len(matching) != 1 or len(colliding) != 1:
        raise GenerationError(
            "Cargo.toml must declare exactly one unambiguous [[test]] named "
            f"{EXPECTED_TEST_NAME!r} at {EXPECTED_TEST_PATH!r}"
        )
    target = matching[0]
    required = target.get("required-features")
    default_features = features.get("default", [])
    if not isinstance(default_features, list) or not all(
        isinstance(item, str) for item in default_features
    ):
        raise GenerationError("Cargo default feature list is malformed")
    pending_features = list(default_features)
    visited_features: set[str] = set()
    while pending_features:
        enabled = pending_features.pop()
        if enabled == EXPECTED_FEATURE_NAME:
            raise GenerationError(
                "route census authority feature must not be activated directly or transitively by default features"
            )
        if enabled in visited_features:
            continue
        visited_features.add(enabled)
        nested = features.get(enabled)
        if nested is None:
            continue
        if not isinstance(nested, list) or not all(isinstance(item, str) for item in nested):
            raise GenerationError(f"Cargo feature {enabled!r} is malformed")
        for reference in nested:
            referenced_feature = (
                reference.removeprefix("dep:").split("/", 1)[0].removesuffix("?")
            )
            if referenced_feature == EXPECTED_FEATURE_NAME:
                raise GenerationError(
                    "route census authority feature must not be activated directly or transitively by default features"
                )
            if referenced_feature in features:
                pending_features.append(referenced_feature)
    if (
        not isinstance(required, list)
        or required != [EXPECTED_FEATURE_NAME]
        or target.get("harness", True) is not True
        or target.get("test", True) is not True
    ):
        raise GenerationError(
            "route census authority target must be an ordinary test harness gated by "
            f"required-features = [{EXPECTED_FEATURE_NAME!r}]"
        )
    target_source = root / EXPECTED_TEST_PATH
    if not target_source.is_file() or target_source.is_symlink():
        raise GenerationError(
            f"registered Cargo authority target source must exist as a regular file: {EXPECTED_TEST_PATH}"
        )
    try:
        target_source_text = target_source.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise GenerationError("registered Cargo authority target source is unreadable") from error
    if target_source_text != EXPECTED_WRAPPER_SOURCE:
        raise GenerationError(
            "registered Cargo authority target source must be the immutable generated-source include wrapper"
        )


def _read_report(path: Path) -> tuple[dict[str, Any], bytes]:
    try:
        raw = path.read_bytes()
        document = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_pairs,
            parse_constant=_reject_constant,
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise GenerationError(f"cannot parse census report as strict UTF-8 JSON: {error}") from error
    if not isinstance(document, dict):
        raise GenerationError("census report must be a JSON object")
    return document, raw


def _validate_report_identity(
    report: Mapping[str, Any],
    report_path: Path,
    root: Path,
    census: Any,
    *,
    allow_unstable_snapshot: bool = False,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], dict[str, Any], Path]:
    if census.SCHEMA_VERSION != REQUIRED_CENSUS_SCHEMA_VERSION:
        raise GenerationError(
            f"current collector must implement census schema {REQUIRED_CENSUS_SCHEMA_VERSION}"
        )
    if report.get("schema_version") != REQUIRED_CENSUS_SCHEMA_VERSION:
        raise GenerationError(
            f"census report schema_version must be {REQUIRED_CENSUS_SCHEMA_VERSION}"
        )
    if report.get("evidence_kind") != "route-census":
        raise GenerationError("input is not a route-census report")
    if report.get("collection_mode") != "full_execution":
        raise GenerationError("route authority generation requires full_execution evidence")
    gate = report.get("gate")
    if (
        not isinstance(gate, Mapping)
        or gate.get("mode") != "evidence_collection_only"
        or gate.get("requested") is not False
    ):
        raise GenerationError("input report must be an evidence-collection run, not inventory-only or strict-check")
    inventory = report.get("input_inventory")
    if not isinstance(inventory, Mapping):
        raise GenerationError("census report is missing input_inventory")
    reported_root = inventory.get("root")
    if not isinstance(reported_root, str) or Path(reported_root).resolve() != root:
        raise GenerationError("census input_inventory.root does not match the supplied repository root")

    collector = report.get("collector")
    collector_path = root / "scripts" / "full_route_census.py"
    try:
        reported_collector_path = Path(str(collector.get("path"))).resolve()
        current_collector_sha = census.sha256_file(collector_path)
    except (AttributeError, OSError, TypeError) as error:
        raise GenerationError("census report does not identify a readable current collector") from error
    if (
        not isinstance(collector, Mapping)
        or reported_collector_path != collector_path.resolve()
        or collector.get("sha256") != current_collector_sha
    ):
        raise GenerationError("census collector path/hash is stale or does not match the current collector")

    tracked_paths, tracked_inventory = census.tracked_mimi_paths(root)
    if tracked_inventory.get("status") != "available":
        raise GenerationError("cannot prove the current Git-indexed Mimi inventory")
    sources, exclusions, current_scope = census.enumerate_source_scope(root, tracked_paths)
    source_scope = report.get("source_scope")
    if (
        current_scope.get("exactly_once") is not True
        or source_scope != current_scope
        or inventory.get("source_scope") != current_scope
        or inventory.get("scope_exclusions") != exclusions
        or inventory.get("inputs") != sources
        or inventory.get("count") != len(sources)
    ):
        raise GenerationError("census candidate/source-scope ledger is not an exact current inventory")
    # The current source inventory is the denominator authority. The report's
    # frozen count and manifest are checked against it above/below; a historical
    # constant would reject legitimate current inventories and can go stale.
    current_candidate_count = len(sources)
    manifest = report.get("manifest")
    manifest_sha = census._manifest_digest(sources)
    if (
        not isinstance(manifest, Mapping)
        or manifest.get("denominator") != current_candidate_count
        or manifest.get("sha256") != manifest_sha
        or manifest.get("source_scope_exactly_once") is not True
        or inventory.get("manifest_sha256") != manifest_sha
    ):
        raise GenerationError("census manifest digest/denominator does not match current inventory")
    snapshot_claimed_stable = report.get("source_snapshot_stable") is True
    if not snapshot_claimed_stable and not allow_unstable_snapshot:
        raise GenerationError("full execution report did not prove a stable source snapshot")
    start_snapshot = report.get("source_snapshot")
    end_snapshot = report.get("source_snapshot_end")
    current_snapshot = census.source_snapshot(root, sources)
    if (
        not isinstance(start_snapshot, Mapping)
        or not isinstance(end_snapshot, Mapping)
        or current_snapshot.get("snapshot_id") != end_snapshot.get("snapshot_id")
        or current_snapshot.get("files") != end_snapshot.get("files")
        or (
            snapshot_claimed_stable
            and start_snapshot.get("snapshot_id") != end_snapshot.get("snapshot_id")
        )
    ):
        raise GenerationError("census compiler/source snapshot is stale relative to the current checkout")

    return sources, exclusions, current_scope, current_snapshot


def _revalidate_current_inputs(
    *,
    report_path: Path,
    report_raw: bytes,
    root: Path,
    census: Any,
    sources: Sequence[Mapping[str, Any]],
    exclusions: Sequence[Mapping[str, Any]],
    source_scope: Mapping[str, Any],
    source_snapshot: Mapping[str, Any],
    collector_sha256: str,
    mimi_path: Path,
    mimi_sha256: str,
) -> None:
    """Recheck mutable identities immediately before publishing artifacts."""
    try:
        if report_path.read_bytes() != report_raw:
            raise GenerationError("census report changed while authority generation was in progress")
        current_collector_sha = census.sha256_file(root / "scripts" / "full_route_census.py")
        current_mimi_sha = census.sha256_file(mimi_path)
    except OSError as error:
        raise GenerationError("current report, collector or compiler identity became unavailable") from error
    if current_collector_sha != collector_sha256:
        raise GenerationError("current collector changed while authority generation was in progress")
    if current_mimi_sha != mimi_sha256:
        raise GenerationError("reported Mimi compiler changed while authority generation was in progress")
    tracked_paths, tracked_inventory = census.tracked_mimi_paths(root)
    if tracked_inventory.get("status") != "available":
        raise GenerationError("cannot revalidate the current Git-indexed Mimi inventory")
    current_sources, current_exclusions, current_scope = census.enumerate_source_scope(root, tracked_paths)
    if (
        list(sources) != current_sources
        or list(exclusions) != current_exclusions
        or source_scope != current_scope
        or source_scope.get("exactly_once") is not True
    ):
        raise GenerationError("candidate inventory or source scope changed while authority generation was in progress")
    current_snapshot = census.source_snapshot(root, current_sources)
    if current_snapshot != source_snapshot:
        raise GenerationError("candidate source hashes changed while authority generation was in progress")


def _read_capture(
    capture: Any,
    *,
    logs_root: Path,
    label: str,
) -> bytes:
    if not isinstance(capture, Mapping):
        raise GenerationError(f"{label} capture is missing")
    encoded = capture.get("base64")
    digest = capture.get("sha256")
    byte_count = capture.get("bytes")
    sidecar = capture.get("path")
    if (
        not isinstance(encoded, str)
        or not isinstance(digest, str)
        or SHA256_RE.fullmatch(digest) is None
        or not isinstance(byte_count, int)
        or isinstance(byte_count, bool)
        or byte_count < 0
        or not isinstance(sidecar, str)
        or Path(sidecar).name != sidecar
        or sidecar in {"", ".", ".."}
    ):
        raise GenerationError(f"{label} capture metadata is incomplete or malformed")
    try:
        payload = base64.b64decode(encoded, validate=True)
    except (ValueError, base64.binascii.Error) as error:
        raise GenerationError(f"{label} inline base64 is invalid") from error
    if len(payload) != byte_count or _sha256(payload) != digest:
        raise GenerationError(f"{label} inline byte count or SHA-256 does not match")
    sidecar_path = logs_root / sidecar
    try:
        metadata = sidecar_path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or sidecar_path.is_symlink():
            raise GenerationError(f"{label} sidecar is not a regular non-symlink file")
        raw_sidecar = sidecar_path.read_bytes()
    except OSError as error:
        raise GenerationError(f"{label} sidecar is not available") from error
    if raw_sidecar != payload:
        raise GenerationError(f"{label} sidecar bytes do not match its inline capture")
    return payload


def _source_diagnostic_authority_output_path(source_path: str, source_sha256: str) -> str:
    identity = f"{source_path}\0{source_sha256}".encode("utf-8")
    return f"target/route-census-source-diagnostic-{_sha256(identity)[:20]}.out"


def _source_diagnostic_from_raw_logs(
    stderr: bytes, *, source_path: str, root: Path
) -> dict[str, Any]:
    """Derive the complete source diagnostic set directly from captured stderr."""
    try:
        text = stderr.decode("utf-8")
    except UnicodeDecodeError as error:
        raise GenerationError(f"{source_path}: source diagnostic stderr is not complete UTF-8") from error
    if not text or "\x00" in text or "\r" in text or "\x1b" in text:
        raise GenerationError(f"{source_path}: source diagnostic stderr is not deterministic plain text")
    lowered = text.lower()
    if any(marker in lowered for marker in SOURCE_DIAGNOSTIC_FORBIDDEN_MARKERS):
        raise GenerationError(f"{source_path}: source diagnostics contain route or fallback telemetry")
    absolute_source = str((root / source_path).resolve())
    lines = text.splitlines()
    diagnostic_lines = [line for line in lines if SOURCE_DIAGNOSTIC_RE.search(line)]
    summary_lines = [line for line in lines if SOURCE_DIAGNOSTIC_SUMMARY_RE.fullmatch(line)]
    nonempty_lines = [line for line in lines if line.strip()]
    if (
        not diagnostic_lines
        or len(summary_lines) > 1
        or len(nonempty_lines) != len(diagnostic_lines) + len(summary_lines)
        or any(line not in diagnostic_lines and line not in summary_lines for line in nonempty_lines)
        or any(absolute_source not in line for line in diagnostic_lines)
    ):
        raise GenerationError(
            f"{source_path}: raw stderr contains an unknown, extra or unbound diagnostic"
        )
    codes = sorted({
        match.group("code").upper()
        for line in diagnostic_lines
        if (match := SOURCE_DIAGNOSTIC_CODE_RE.match(line)) is not None
    })
    return {"text": text, "lines": diagnostic_lines, "codes": codes}


def _record_output_path(
    record: Mapping[str, Any],
    source: Mapping[str, Any],
    root: Path,
    reported_mimi: Any,
) -> str:
    command = record.get("command")
    if not isinstance(command, list) or not all(isinstance(value, str) for value in command):
        raise GenerationError(f"{source['path']}: exact observed build command is unavailable")
    prefix = [str(reported_mimi), "build"]
    if source.get("target_kind") == "shared_library_probe":
        prefix.append("--shared")
    prefix.append(str(root / str(source["path"])))
    if command[:len(prefix)] != prefix:
        raise GenerationError(f"{source['path']}: recorded command is not the exact default build")
    output_args = command[len(prefix) :]
    if len(output_args) == 2 and output_args[0] in {"-o", "--output"}:
        output_value = output_args[1]
    elif len(output_args) == 1 and output_args[0].startswith("--output="):
        output_value = output_args[0].split("=", 1)[1]
    else:
        raise GenerationError(f"{source['path']}: recorded build has no unique parsed -o/--output argument")
    if not Path(output_value).is_absolute():
        raise GenerationError(f"{source['path']}: recorded output path is incomplete")
    normalized_output = str(Path(output_value))
    if normalized_output != output_value:
        raise GenerationError(f"{source['path']}: recorded output path is not normalized")
    return output_value


def _rejection_output_absence_evidence(
    record: Mapping[str, Any],
    source: Mapping[str, Any],
    index: int,
    output_path: str,
    *,
    evidence_field: str = "rejection_artifact_evidence",
) -> Mapping[str, Any]:
    evidence = record.get(evidence_field)
    if not isinstance(evidence, Mapping):
        evidence_label = (
            "output artifact evidence"
            if evidence_field == "rejection_artifact_evidence"
            else "source-diagnostic output artifact evidence"
        )
        raise GenerationError(
            f"{source['path']}: report lacks explicit before/after {evidence_label}"
        )
    scratch = evidence.get("scratch")
    if not isinstance(scratch, Mapping):
        raise GenerationError(f"{source['path']}: rejection evidence lacks a fresh isolated run directory")
    if (
        not isinstance(evidence.get("schema_version"), int)
        or isinstance(evidence.get("schema_version"), bool)
        or evidence.get("schema_version") != 2
        or set(evidence) != {
            "schema_version", "scratch", "output_path", "command_output_path",
            "before", "after", "mimi_executable_identity",
        }
        or set(scratch) != {
            "path", "input_index", "existed_before", "created_exclusively", "output_absent_before_build"
        }
    ):
        raise GenerationError(f"{source['path']}: rejection artifact evidence schema is incomplete or unsupported")
    scratch_path = scratch.get("path")
    if (
        not isinstance(scratch_path, str)
        or not Path(scratch_path).is_absolute()
        or str(Path(scratch_path)) != scratch_path
        or not isinstance(scratch.get("input_index"), int)
        or isinstance(scratch.get("input_index"), bool)
        or scratch.get("input_index") != index
        or scratch.get("existed_before") is not False
        or scratch.get("created_exclusively") is not True
        or scratch.get("output_absent_before_build") is not True
    ):
        raise GenerationError(
            f"{source['path']}: scratch directory is not proven fresh and unique to this input"
        )
    expected_directory = str(Path(output_path).parent)
    if (
        evidence.get("output_path") != output_path
        or evidence.get("command_output_path") != output_path
        or scratch_path != expected_directory
        or Path(output_path).name != (
            "out.so" if source.get("target_kind") == "shared_library_probe" else "out"
        )
        or Path(scratch_path).name != f"input-{index:06d}"
    ):
        raise GenerationError(
            f"{source['path']}: output artifact evidence is not bound to an isolated exact build destination"
        )
    expected_observation = {"path": output_path, "exists": False, "sha256": None}
    if evidence.get("before") != expected_observation or evidence.get("after") != expected_observation:
        raise GenerationError(
            f"{source['path']}: output artifact evidence does not prove exact-path absence before and after the build"
        )
    if evidence.get("mimi_executable_identity") != record.get("mimi_executable_identity"):
        raise GenerationError(
            f"{source['path']}: rejection output absence is not bound to the row's observed Mimi executable identity"
        )
    return evidence


def _ensure_observation_fields(
    record: Mapping[str, Any],
    route: Mapping[str, Any],
    failure: Mapping[str, Any],
    diagnostic_codes: list[str],
    *,
    output_present: bool,
    source_sha: str,
) -> Mapping[str, Any]:
    expectation = record.get("expectation")
    if not isinstance(expectation, Mapping):
        raise GenerationError(f"{record.get('path')}: expectation observation is missing")
    if (
        expectation.get("explicit_expectation") is not True
        or expectation.get("class") == "unmapped"
        or expectation.get("matched") is not True
    ):
        raise GenerationError(f"{record.get('path')}: report row is not explicitly mapped")
    actuals = {
        "actual_exit_code": record.get("exit_code"),
        "actual_route": route.get("disposition"),
        "actual_route_classification_status": route.get("classification_status"),
        "actual_route_reason": route.get("reason"),
        "actual_failure_class": failure.get("class"),
        "actual_diagnostic_codes": diagnostic_codes,
        "actual_output_artifact_present": output_present,
        "source_sha256": source_sha,
        "source_hash_status": "matched",
        "matched": True,
    }
    for key, expected in actuals.items():
        if expectation.get(key) != expected:
            raise GenerationError(
                f"{record.get('path')}: report expectation observation {key} is missing or inconsistent"
            )
    if expectation.get("authority_valid") is not True:
        raise GenerationError(f"{record.get('path')}: existing report row has no valid active test authority")
    if expectation.get("test_contract_valid") is not True:
        raise GenerationError(f"{record.get('path')}: existing direct test contract is not valid")
    if expectation.get("cargo_target_valid") is not True:
        raise GenerationError(f"{record.get('path')}: existing Cargo target authority is not valid")
    if expectation.get("test_execution_valid") is not True:
        raise GenerationError(f"{record.get('path')}: existing exact authority test was not proven executed")
    return expectation


def _render_required_compile_test(
    source: Mapping[str, Any], test_id: str, output_path: str
) -> str:
    relative = str(source["path"])
    _rust_path_literal(relative)
    _rust_path_literal(output_path)
    join_chain = "".join(
        f".join({json.dumps(part, ensure_ascii=False)})"
        for part in Path(relative).parts
    )
    target_flag = '        .arg("--shared")\n' if source["target_kind"] == "shared_library_probe" else ""
    return (
        "#[test]\n"
        f"fn {test_id}() {{\n"
        "    let fixture = ::std::path::Path::new(::std::env!(\"CARGO_MANIFEST_DIR\"))"
        f"{join_chain};\n"
        "    let output = ::std::process::Command::new(route_census_mimi_path())\n"
        "        .current_dir(::std::env!(\"CARGO_MANIFEST_DIR\"))\n"
        "        .arg(\"build\")\n"
        f"{target_flag}"
        "        .arg(&fixture)\n"
        "        .arg(\"-o\")\n"
        f"        .arg({json.dumps(output_path, ensure_ascii=False)})\n"
        "        .env(\"MIMI_ROUTE_CENSUS\", \"1\")\n"
        "        .output()\n"
        "        .expect(\"validated Mimi default build must start\");\n"
        "    ::std::assert!(output.status.success());\n"
        "    let stderr = ::std::string::String::from_utf8_lossy(&output.stderr);\n"
        '    ::std::assert!(stderr.contains("canonical route disposition: canonical ("));\n'
        '    ::std::assert!(stderr.contains("MIMI_ROUTE_CENSUS route-admission consumer=build mode=default "));\n'
        '    ::std::assert!(stderr.contains("MIMI_ROUTE_CENSUS canonical-native-build consumer=build "));\n'
        '    ::std::assert!(stderr.contains("mir_digest="));\n'
        '    ::std::assert!(stderr.contains("receipt_identity_digest="));\n'
        '    ::std::assert!(!stderr.contains("canonical route disposition: legacy"));\n'
        '    ::std::assert!(!stderr.contains("canonical route disposition: compatibility"));\n'
        '    ::std::assert!(!stderr.contains("fallback"));\n'
        '    ::std::assert!(!stderr.contains("compatibility"));\n'
        "}\n"
    )


def _rust_path_literal(value: str) -> str:
    if any(char in value for char in ('"', "\\", "\n", "\r", "\x00")):
        raise GenerationError(f"Rust authority path contains unsupported characters: {value!r}")
    return json.dumps(value, ensure_ascii=False)


def _render_expected_rejection_test(
    source: Mapping[str, Any],
    test_id: str,
    output_path: str,
    expected_route_reason: str,
    diagnostic_codes: Sequence[str],
    required_output_substrings: Sequence[str],
) -> tuple[str, int]:
    relative = str(source["path"])
    _rust_path_literal(relative)
    _rust_path_literal(output_path)
    join_chain = "".join(
        f".join({json.dumps(part, ensure_ascii=False)})"
        for part in Path(relative).parts
    )
    for value in (*diagnostic_codes, *required_output_substrings):
        _rust_path_literal(value)
    if "\n" in expected_route_reason or "\r" in expected_route_reason:
        raise GenerationError(f"{relative}: multiline route rejection reasons cannot be authorized")
    target_flag = '        .arg("--shared")\n' if source["target_kind"] == "shared_library_probe" else ""
    lines = [
        "#[test]",
        f"fn {test_id}() {{",
        "    let fixture = ::std::path::Path::new(::std::env!(\"CARGO_MANIFEST_DIR\"))" + join_chain + ";",
        "    let output = ::std::process::Command::new(route_census_mimi_path())",
        "        .current_dir(::std::env!(\"CARGO_MANIFEST_DIR\"))",
        '        .arg("build")',
    ]
    if target_flag:
        lines.extend(target_flag.rstrip("\n").splitlines())
    lines.extend([
        "        .arg(&fixture)",
        '        .arg("-o")',
        f"        .arg({json.dumps(output_path, ensure_ascii=False)})",
        '        .env("MIMI_ROUTE_CENSUS", "1")',
        "        .output()",
        '        .expect("validated Mimi default build must start");',
        "    ::std::assert!(!output.status.success());",
        "    let stderr = ::std::string::String::from_utf8_lossy(&output.stderr);",
    ])
    line_number = len(lines) + 1
    assertions = list(dict.fromkeys([
        *(str(code).upper() for code in diagnostic_codes),
        *required_output_substrings,
        f"default Canonical MIR route rejected: {expected_route_reason}",
        f"canonical MIR build rejected: {expected_route_reason}",
    ]))
    # The census recognizer accepts either exact route line spelling. Include
    # only the observed spelling; it independently checks this exact reason.
    observed_route_literals = [
        item for item in required_output_substrings
        if item.startswith(ROUTE_REJECTION_PREFIXES)
    ]
    if len(observed_route_literals) != 1:
        raise GenerationError(f"{relative}: expected exactly one observed route-rejection line")
    selected_route_literal = observed_route_literals[0]
    assertions = list(dict.fromkeys([
        *(str(code).upper() for code in diagnostic_codes),
        *required_output_substrings,
        selected_route_literal,
    ]))
    for literal in assertions:
        lines.append(f"    ::std::assert!(stderr.contains({json.dumps(literal, ensure_ascii=False)}));")
    lines.append("}")
    return "\n".join(lines) + "\n", line_number


def _line_number_for_literal(test: str, literal: str) -> int:
    for number, line in enumerate(test.splitlines(), 1):
        if literal in line and "assert!" in line:
            return number
    raise GenerationError(f"generated authority test lost assertion literal {literal!r}")


def _test_id(index: int, source_path: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9]+", "_", source_path).strip("_").lower()
    slug = slug[:72].strip("_") or "input"
    value = f"route_census_{index:04d}_{slug}"
    if IDENTIFIER_RE.fullmatch(value) is None:
        raise GenerationError(f"could not derive a Rust test identifier for {source_path!r}")
    return value


def _validate_row(
    *,
    index: int,
    source: Mapping[str, Any],
    record: Mapping[str, Any],
    root: Path,
    logs_root: Path,
    reported_mimi: Any,
    reported_mimi_sha256: Any,
    census: Any,
    cargo_authority: Any,
) -> tuple[dict[str, Any], str, dict[str, Any]]:
    path = _normalized_repo_path(source.get("path"), label="candidate path")
    if record.get("path") != path or source.get("route_candidate") is not True:
        raise GenerationError(f"{path}: report row is not an exact route candidate")
    for key, value in source.items():
        if record.get(key) != value:
            raise GenerationError(f"{path}: record field {key!r} differs from its frozen candidate inventory")
    if (
        not isinstance(record.get("input_index"), int)
        or isinstance(record.get("input_index"), bool)
        or record.get("input_index") != index
    ):
        raise GenerationError(f"{path}: record input_index is missing, duplicated or out of order")
    if record.get("run_status") != "completed":
        raise GenerationError(f"{path}: build did not complete")
    mimi_identity = record.get("mimi_executable_identity")
    observations = (
        mimi_identity.get("before"), mimi_identity.get("after")
    ) if isinstance(mimi_identity, Mapping) else ()
    if (
        not isinstance(reported_mimi_sha256, str)
        or SHA256_RE.fullmatch(reported_mimi_sha256) is None
        or not isinstance(mimi_identity, Mapping)
        or mimi_identity.get("path") != reported_mimi
        or mimi_identity.get("sha256_expected") != reported_mimi_sha256
        or mimi_identity.get("matches_expected") is not True
        or len(observations) != 2
        or any(
            not isinstance(observation, Mapping)
            or observation.get("path") != reported_mimi
            or observation.get("resolved_path") != reported_mimi
            or observation.get("sha256") != reported_mimi_sha256
            for observation in observations
        )
    ):
        raise GenerationError(
            f"{path}: candidate route observation does not bind the exact --mimi binary path and SHA-256"
        )
    if source.get("present") is not True or source.get("presence_kind") != "regular_file":
        raise GenerationError(f"{path}: candidate is missing, symlinked or not a regular file")
    if source.get("buildable") is not True or source.get("target_kind") not in {"executable", "shared_library_probe"}:
        raise GenerationError(f"{path}: candidate has no supported exact build target")
    source_sha = source.get("sha256")
    if not isinstance(source_sha, str) or SHA256_RE.fullmatch(source_sha) is None:
        raise GenerationError(f"{path}: source inventory does not carry a canonical SHA-256")
    current_sha = _source_file_sha256(root, path)
    if current_sha != source_sha:
        raise GenerationError(f"{path}: current source hash differs from the census inventory")
    if (
        record.get("sha256") != source_sha
        or record.get("source_hash_before_execution") != source_sha
        or record.get("source_hash_after_execution") != source_sha
        or record.get("source_stable_for_input") is not True
    ):
        raise GenerationError(f"{path}: source hash was not stable across the observed build")
    if record.get("target_kind") != source.get("target_kind"):
        raise GenerationError(f"{path}: build target kind differs from the frozen inventory")
    output_path = _record_output_path(record, source, root, reported_mimi)
    stdout = _read_capture(record.get("stdout_raw"), logs_root=logs_root, label=f"{path} stdout")
    stderr = _read_capture(record.get("stderr_raw"), logs_root=logs_root, label=f"{path} stderr")
    if record.get("stdout") != stdout.decode("utf-8", errors="replace"):
        raise GenerationError(f"{path}: rendered stdout differs from exact captured bytes")
    if record.get("stderr") != stderr.decode("utf-8", errors="replace"):
        raise GenerationError(f"{path}: rendered stderr differs from exact captured bytes")
    stdout_text = stdout.decode("utf-8", errors="replace")
    stderr_text = stderr.decode("utf-8", errors="replace")
    route = census._route_observation(
        stdout_text, stderr_text, record.get("exit_code"), record.get("run_status")
    )
    if record.get("route") != route:
        raise GenerationError(f"{path}: route observation cannot be reproduced from captured output")
    diagnostic_codes = census._diagnostic_codes(stderr_text + "\n" + stdout_text)
    row_expectation = record.get("expectation")
    row_expectation_class = (
        row_expectation.get("class") if isinstance(row_expectation, Mapping) else None
    )
    if (
        not diagnostic_codes
        and record.get("exit_code") != 0
        and row_expectation_class != "expected_source_diagnostic"
    ):
        raise GenerationError(f"{path}: nonzero build has no complete recognized diagnostics")
    failure = census._failure_classification(
        run_status=str(record.get("run_status")),
        exit_code=record.get("exit_code"),
        text=stderr_text + "\n" + stdout_text,
        target_kind=str(source.get("target_kind")),
        route=route,
    )
    if record.get("failure_classification") != failure:
        raise GenerationError(f"{path}: failure classification cannot be reproduced from captured output")
    function_observation = census._function_observations(
        stdout_text, stderr_text, record.get("dispatch_stats"), route
    )
    per_function = record.get("per_function")
    if not isinstance(per_function, Mapping):
        raise GenerationError(f"{path}: per-function route receipt is missing")
    for key in ("status", "named_roster_complete", "has_fallback", "consistency", "counts", "functions"):
        if per_function.get(key) != function_observation.get(key):
            raise GenerationError(f"{path}: per-function receipt field {key!r} is not reproducible")

    route_disposition = route.get("disposition")
    route_status = route.get("classification_status")
    candidate_classification = record.get("candidate_classification")
    output = record.get("output_artifact")
    output_present = output is not None
    expectation = _ensure_observation_fields(
        record,
        route,
        failure,
        diagnostic_codes,
        output_present=output_present,
        source_sha=source_sha,
    )
    expectation_class = expectation.get("class")
    if route_disposition == "canonical":
        if (
            route_status != "observed"
            or candidate_classification != "canonical"
            or expectation_class != "required_compile"
            or expectation.get("matched") is not True
            or not isinstance(record.get("exit_code"), int)
            or isinstance(record.get("exit_code"), bool)
            or record.get("exit_code") != 0
            or failure.get("class") != "none"
        ):
            raise GenerationError(f"{path}: canonical route is not explicitly and completely authorized")
        if (
            per_function.get("status") != "complete_named"
            or per_function.get("named_roster_complete") is not True
            or per_function.get("has_fallback") is not False
            or not isinstance(per_function.get("consistency"), Mapping)
            or not per_function["consistency"]
            or any(value is not True for value in per_function["consistency"].values())
        ):
            raise GenerationError(f"{path}: canonical route receipt/roster is incomplete or has fallback")
        digest = route.get("mir_digest")
        if not isinstance(digest, str) or CANONICAL_DIGEST_RE.fullmatch(digest) is None:
            raise GenerationError(f"{path}: canonical route has no exact MIR digest")
        if output is None or not isinstance(output, Mapping):
            raise GenerationError(f"{path}: canonical build output artifact evidence is missing")
        if (
            not isinstance(output.get("sha256"), str)
            or SHA256_RE.fullmatch(str(output.get("sha256"))) is None
            or not isinstance(output.get("bytes"), int)
            or isinstance(output.get("bytes"), bool)
            or output.get("bytes", 0) <= 0
        ):
            raise GenerationError(f"{path}: canonical build output artifact hash/size is incomplete")
        rust_test = _render_required_compile_test(
            source, _test_id(index, path), f"target/route-census-authorities-{index:04d}.out"
        )
        probe = census._required_compile_test_authority_is_valid(
            test_id=_test_id(index, path),
            source_path=path,
            target_kind=str(source["target_kind"]),
            source_text=rust_test,
            assertion_line=_line_number_for_literal(rust_test, "canonical route disposition: canonical ("),
        )
        if not probe:
            raise GenerationError(f"{path}: generated canonical test does not satisfy strict v4 recognizer")
        generated_expectation = {
            "class": "required_compile",
            "reason": "current source must build through the default canonical route with a complete receipt and no fallback",
            "source_sha256": source_sha,
            "authority": {
                "test_id": _test_id(index, path),
                "assertion": {
                    "path": EXPECTED_TEST_PATH,
                    "line": _line_number_for_literal(rust_test, "canonical route disposition: canonical ("),
                    "sha256": "",
                },
            },
        }
    elif expectation_class == "expected_source_diagnostic":
        if (
            route_disposition != "rejected"
            or route_status != "inferred_pre_route_rejection"
            or candidate_classification != "source_diagnostic"
            or not isinstance(record.get("exit_code"), int)
            or isinstance(record.get("exit_code"), bool)
            or record.get("exit_code", 0) <= 0
            or failure.get("class") != "source_diagnostic"
            or failure.get("status") != "classified"
            or stdout != b""
            or output is not None
            or route.get("observations") != []
            or route.get("route_marker_count") != 0
            or route.get("reason") != "compiler emitted a failure diagnostic before any route telemetry"
        ):
            raise GenerationError(
                f"{path}: source diagnostic must remain an inferred pre-route failure with no route marker"
            )
        raw_diagnostics = _source_diagnostic_from_raw_logs(
            stderr, source_path=path, root=root
        )
        expected_text = raw_diagnostics["text"]
        expected_lines = raw_diagnostics["lines"]
        expected_codes = raw_diagnostics["codes"]
        if (
            expectation.get("expected_diagnostic_text") != expected_text
            or expectation.get("expected_diagnostic_lines") != expected_lines
            or expectation.get("expected_diagnostic_codes") != expected_codes
            or expectation.get("expected_exit_code") != record.get("exit_code")
            or expectation.get("expected_output_path")
            != _source_diagnostic_authority_output_path(path, source_sha)
        ):
            raise GenerationError(
                f"{path}: typed source-diagnostic expectation does not exactly match raw stderr"
            )
        diagnostic_contract = expectation.get("diagnostic_contract")
        if not isinstance(diagnostic_contract, Mapping) or any(
            diagnostic_contract.get(key) is not True
            for key in (
                "valid",
                "diagnostic_codes_match_exactly",
                "diagnostic_lines_match_exactly",
                "diagnostic_text_matches_exactly",
                "stdout_empty",
                "unknown_or_extra_diagnostics_absent",
                "route_telemetry_absent",
                "fallback_markers_absent",
                "output_artifact_absent",
            )
        ):
            raise GenerationError(f"{path}: strict source-diagnostic contract evidence is incomplete")
        if (
            diagnostic_contract.get("actual_diagnostic_text") != expected_text
            or diagnostic_contract.get("actual_diagnostic_lines") != expected_lines
            or diagnostic_contract.get("actual_diagnostic_codes") != expected_codes
            or diagnostic_contract.get("expected_output_path")
            != expectation.get("expected_output_path")
        ):
            raise GenerationError(f"{path}: source-diagnostic report fields differ from raw stderr")
        if (
            per_function.get("named_roster_complete") is not False
            or per_function.get("has_fallback") is not False
            or per_function.get("functions") != []
            or per_function.get("emitter_summary") is not None
            or per_function.get("status") != "unavailable"
        ):
            raise GenerationError(
                f"{path}: source diagnostic must not manufacture a canonical function roster"
            )
        if not census._expected_source_diagnostic_execution_proof_is_valid(
            expectation,
            root=root,
            logs_root=logs_root,
            cargo_authority=cargo_authority,
        ):
            raise GenerationError(
                f"{path}: exact direct Cargo source-diagnostic authority test is not independently proven"
            )
        rejection_artifact_evidence = _rejection_output_absence_evidence(
            record,
            source,
            index,
            output_path,
            evidence_field="source_diagnostic_artifact_evidence",
        )
        if not census._rejection_artifact_evidence_valid(
            rejection_artifact_evidence,
            command=record.get("command"),
            input_index=index,
            target_kind=source.get("target_kind"),
            mimi_identity=record.get("mimi_executable_identity"),
        ):
            raise GenerationError(
                f"{path}: source-diagnostic build output path was not absent before and after the captured build"
            )
        test_id = _test_id(index, path)
        authority_output_path = expectation["expected_output_path"]
        rust_test, assertion_line = census._render_expected_source_diagnostic_test(
            test_id=test_id,
            source_path=path,
            source_sha256=source_sha,
            expected_exit_code=record["exit_code"],
            target_kind=str(source["target_kind"]),
            output_path=authority_output_path,
            expected_diagnostic_text=expected_text,
            root=root,
        )
        if not census._expected_source_diagnostic_test_authority_is_valid(
            test_id=test_id,
            source_path=path,
            source_sha256=source_sha,
            expected_exit_code=record["exit_code"],
            target_kind=str(source["target_kind"]),
            output_path=authority_output_path,
            expected_diagnostic_text=expected_text,
            expected_diagnostic_codes=expected_codes,
            expected_diagnostic_lines=expected_lines,
            source_text=rust_test,
            assertion_line=assertion_line,
            root=root,
        ):
            raise GenerationError(
                f"{path}: generated source-diagnostic test does not enforce exact build failure and output absence"
            )
        generated_expectation = {
            "class": "expected_source_diagnostic",
            "reason": "current source is source-hash-bound to its complete plain-text syntax/type diagnostics before route selection",
            "source_sha256": source_sha,
            "authority": {
                "test_id": test_id,
                "assertion": {
                    "path": EXPECTED_TEST_PATH,
                    "line": assertion_line,
                    "sha256": "",
                },
            },
            "expected_exit_code": record["exit_code"],
            "expected_diagnostic_codes": expected_codes,
            "expected_diagnostic_lines": expected_lines,
            "expected_diagnostic_text": expected_text,
            "expected_output_path": authority_output_path,
            "source_diagnostic_authority_evidence": {
                "raw_log_source": "complete_stderr_sidecar_and_inline_capture",
                "raw_stderr_sha256": _sha256(stderr),
                "exact_diagnostic_lines": expected_lines,
                "exact_diagnostic_codes": expected_codes,
                "direct_test_asserts_nonzero_mimi_exit": True,
                "direct_test_asserts_exact_stderr_and_empty_stdout": True,
                "direct_test_asserts_output_absent_before_and_after": True,
                "route_disposition": "inferred_pre_route_rejection",
                "observed_route_rejection": False,
            },
            "artifact_absence_evidence": {
                "source": "collector_observed_before_and_after_build",
                "path": rejection_artifact_evidence["output_path"],
                "run_directory": rejection_artifact_evidence["scratch"]["path"],
                "mimi_executable_identity": rejection_artifact_evidence["mimi_executable_identity"],
                "direct_test_output_path": authority_output_path,
                "direct_test_asserts_artifact_absence": True,
            },
        }
        route_status = "inferred_pre_route_rejection"
    elif route_disposition == "rejected":
        if (
            route_status != "observed"
            or candidate_classification != "intentional_reject"
            or expectation_class != "expected_build_rejection"
            or expectation.get("matched") is not True
            or record.get("exit_code") != expectation.get("expected_exit_code")
            or not isinstance(record.get("exit_code"), int)
            or isinstance(record.get("exit_code"), bool)
            or record.get("exit_code", 0) <= 0
            or failure.get("class") not in {"capability", "library_target"}
            or expectation.get("expected_failure_class") != failure.get("class")
            or expectation.get("expected_failure_class") not in {"capability", "library_target"}
            or expectation.get("expected_route_disposition") != "rejected"
            or expectation.get("expected_route_classification_status") != "observed"
            or expectation.get("expected_route_reason") != route.get("reason")
            or failure.get("status") != "classified"
            or per_function.get("has_fallback") is not False
        ):
            raise GenerationError(f"{path}: rejected route is inferred, unknown or not an allowed observed rejection")
        if output is not None or expectation.get("actual_output_artifact_present") is not False:
            raise GenerationError(
                f"{path}: report does not prove absent output artifact at the recorded build output path"
            )
        diagnostic_contract = expectation.get("diagnostic_contract")
        route_contract = expectation.get("route_contract")
        if not isinstance(diagnostic_contract, Mapping) or not isinstance(route_contract, Mapping):
            raise GenerationError(f"{path}: strict rejection route/diagnostic contracts are missing")
        if (
            diagnostic_contract.get("output_artifact_absent") is not True
            or diagnostic_contract.get("diagnostic_codes_match_exactly") is not True
            or diagnostic_contract.get("required_output_substrings_match") is not True
            or route_contract.get("reason_matches_exactly") is not True
        ):
            raise GenerationError(
                f"{path}: current report has no strict proof of exact rejection diagnostics and route reason"
            )
        rejection_artifact_evidence = _rejection_output_absence_evidence(
            record, source, index, output_path
        )
        if not census._rejection_artifact_evidence_valid(
            rejection_artifact_evidence,
            command=record.get("command"),
            input_index=index,
            target_kind=source.get("target_kind"),
            mimi_identity=record.get("mimi_executable_identity"),
        ):
            raise GenerationError(
                f"{path}: rejection output artifact evidence does not satisfy the current collector gate"
            )
        route_observations = route.get("observations")
        if not isinstance(route_observations, list) or len(route_observations) != 1:
            raise GenerationError(f"{path}: rejection route evidence is not a unique observed line")
        observed = route_observations[0]
        route_reason = route.get("reason")
        if (
            not isinstance(route_reason, str)
            or not route_reason
            or observed.get("reason") != route_reason
            or observed.get("disposition") != "rejected"
        ):
            raise GenerationError(f"{path}: rejection route reason does not match its single observed marker")
        observed_line = observed.get("line")
        plain_all = census._strip_ansi(stderr_text + "\n" + stdout_text)
        plain_stderr = census._strip_ansi(stderr_text)
        if not isinstance(observed_line, str):
            raise GenerationError(f"{path}: rejection route line is missing")
        plain_route_line = census._strip_ansi(observed_line)
        if (
            plain_route_line not in plain_all
            or plain_route_line not in plain_stderr
            or plain_route_line not in stderr_text
            or any(character in plain_route_line for character in "\r\n")
        ):
            raise GenerationError(
                f"{path}: rejection route line is not an exact captured stderr line"
            )
        if not any(plain_route_line.startswith(prefix) and plain_route_line[len(prefix):] == route_reason for prefix in ROUTE_REJECTION_PREFIXES):
            raise GenerationError(f"{path}: rejection route line does not carry the exact allowed route reason")
        if "\x1b" in observed_line:
            raise GenerationError(f"{path}: colored route marker cannot be bound by the strict direct-test parser")

        observed_diag_lines: list[str] = []
        for code in diagnostic_codes:
            matching_raw = [
                line for line in stderr_text.splitlines()
                if code in census._diagnostic_codes(line)
            ]
            matching = [census._strip_ansi(line) for line in matching_raw]
            if (
                not matching
                or any("\x1b" in line for line in matching_raw)
                or any(line not in stderr_text for line in matching)
            ):
                raise GenerationError(
                    f"{path}: diagnostic {code} lacks a complete plain-text stderr line"
                )
            observed_diag_lines.extend(matching)
        required_substrings = list(dict.fromkeys([*observed_diag_lines, plain_route_line]))
        # Refuse report-supplied expectations that omit any exact observed line.
        prior_codes = expectation.get("expected_diagnostic_codes")
        prior_substrings = expectation.get("required_output_substrings")
        normalized_prior_codes = (
            [str(item).upper() for item in prior_codes]
            if isinstance(prior_codes, list) and all(isinstance(item, str) for item in prior_codes)
            else []
        )
        if (
            not isinstance(prior_codes, list)
            or not normalized_prior_codes
            or len(set(normalized_prior_codes)) != len(normalized_prior_codes)
            or sorted(normalized_prior_codes) != diagnostic_codes
            or not isinstance(prior_substrings, list)
            or not set(required_substrings).issubset(set(prior_substrings))
            or any(not isinstance(item, str) or not item for item in prior_substrings)
            or any(item not in plain_stderr or item not in stderr_text for item in prior_substrings)
        ):
            raise GenerationError(
                f"{path}: report expectation does not bind every exact observed stderr diagnostic/output line"
            )
        required_substrings = list(dict.fromkeys([*prior_substrings, *required_substrings]))
        test_id = _test_id(index, path)
        rust_test, assertion_line = _render_expected_rejection_test(
            source,
            test_id,
            f"target/route-census-authorities-{index:04d}.out",
            route_reason,
            diagnostic_codes,
            required_substrings,
        )
        if not census._expected_rejection_test_authority_is_valid(
            test_id=test_id,
            source_path=path,
            target_kind=str(source["target_kind"]),
            source_text=rust_test,
            assertion_line=assertion_line,
            expected_diagnostic_codes=diagnostic_codes,
            required_output_substrings=required_substrings,
            expected_route_reason=route_reason,
        ):
            raise GenerationError(f"{path}: generated rejection test does not satisfy strict v4 recognizer")
        generated_expectation = {
            "class": "expected_build_rejection",
            "reason": "current source has an observed, classified default Canonical MIR rejection with exact diagnostics and no output artifact",
            "source_sha256": source_sha,
            "authority": {
                "test_id": test_id,
                "assertion": {"path": EXPECTED_TEST_PATH, "line": assertion_line, "sha256": ""},
            },
            "expected_exit_code": record["exit_code"],
            "expected_diagnostic_codes": diagnostic_codes,
            "required_output_substrings": required_substrings,
            "expected_route_disposition": "rejected",
            "expected_route_classification_status": "observed",
            "expected_route_reason": route_reason,
            "expected_failure_class": failure["class"],
            "artifact_absence_evidence": {
                "source": "collector_observed_before_and_after_build",
                "path": rejection_artifact_evidence["output_path"],
                "run_directory": rejection_artifact_evidence["scratch"]["path"],
                "mimi_executable_identity": rejection_artifact_evidence["mimi_executable_identity"],
                "direct_test_asserts_artifact_absence": False,
                "direct_test_asserts_route_reason_and_all_diagnostics": True,
            },
        }
    else:
        raise GenerationError(
            f"{path}: disallowed route disposition {route_disposition!r}; only observed canonical or authorized rejection rows are accepted"
        )

    generated = generated_expectation
    artifact_absence_row: Mapping[str, Any] | None = None
    if generated["class"] == "expected_build_rejection":
        artifact_absence_row = {
            "source": "collector_observed_before_and_after_build",
            "path": rejection_artifact_evidence["output_path"],
            "run_directory": rejection_artifact_evidence["scratch"]["path"],
            "before": rejection_artifact_evidence["before"],
            "after": rejection_artifact_evidence["after"],
            "mimi_executable_identity": rejection_artifact_evidence["mimi_executable_identity"],
            "direct_test_asserts_artifact_absence": False,
        }
    elif generated["class"] == "expected_source_diagnostic":
        artifact_absence_row = {
            "source": "collector_observed_before_and_after_build",
            "path": rejection_artifact_evidence["output_path"],
            "run_directory": rejection_artifact_evidence["scratch"]["path"],
            "before": rejection_artifact_evidence["before"],
            "after": rejection_artifact_evidence["after"],
            "mimi_executable_identity": rejection_artifact_evidence["mimi_executable_identity"],
            "direct_test_output_path": generated["expected_output_path"],
            "direct_test_asserts_artifact_absence": True,
        }
    row = {
        "path": path,
        "test_id": generated["authority"]["test_id"],
        "class": generated["class"],
        "candidate_classification": candidate_classification,
        "observed_route_rejection": (
            route_disposition == "rejected" and route_status == "observed"
        ),
        "source_sha256": source_sha,
        "route_disposition": route_disposition,
        "route_classification_status": route_status,
        "route_reason": route.get("reason") if route_status == "observed" else None,
        "failure_class": failure.get("class"),
        "exit_code": record.get("exit_code"),
        "output_artifact_present": output_present,
        "source_diagnostic_authority_evidence": generated.get(
            "source_diagnostic_authority_evidence"
        ),
        "artifact_absence_evidence": artifact_absence_row,
    }
    return generated, rust_test, row


def _bootstrap_blocked_row(
    *,
    index: int,
    source: Mapping[str, Any],
    reason: str,
    category: str,
    stdout: bytes | None = None,
    stderr: bytes | None = None,
    route: Mapping[str, Any] | None = None,
    failure: Mapping[str, Any] | None = None,
    record: Mapping[str, Any] | None = None,
    source_sha: str | None = None,
) -> tuple[dict[str, Any], None, dict[str, Any]]:
    """Keep an observation in the full denominator without granting authority."""
    path = str(source.get("path", ""))
    evidence: dict[str, Any] = {
        "category": category,
        "status": "blocking",
        "reason": reason,
    }
    if stdout is not None:
        evidence["stdout_sha256"] = _sha256(stdout)
    if stderr is not None:
        evidence["stderr_sha256"] = _sha256(stderr)
    if isinstance(record, Mapping):
        evidence["run_status"] = record.get("run_status")
        evidence["input_index"] = record.get("input_index")
        evidence["candidate_classification"] = record.get("candidate_classification")
        evidence["exit_code"] = record.get("exit_code")
    if isinstance(route, Mapping):
        evidence["route"] = {
            "disposition": route.get("disposition"),
            "classification_status": route.get("classification_status"),
            "reason": route.get("reason"),
            "route_marker_count": route.get("route_marker_count"),
        }
    if isinstance(failure, Mapping):
        evidence["failure_class"] = failure.get("class")
        evidence["failure_status"] = failure.get("status")
    if category == "source_diagnostic_pending_semantic_authorization":
        raw_diagnostic_lines = _source_diagnostic_lines_from_raw_streams(
            stdout, stderr, route=route
        )
        if stdout is not None:
            try:
                evidence["exact_stdout_text"] = stdout.decode("utf-8")
            except UnicodeDecodeError:
                pass
        if stderr is not None:
            try:
                evidence["exact_stderr_text"] = stderr.decode("utf-8")
            except UnicodeDecodeError:
                pass
        evidence["diagnostic_lines"] = [item["text"] for item in raw_diagnostic_lines]
        evidence["raw_diagnostic_lines"] = raw_diagnostic_lines
        evidence["semantic_authorization"] = {
            "status": "pending",
            "required_before_strict_acceptance": True,
            "route_or_roster_authority": False,
        }
    expectation = {
        "class": "unmapped",
        "reason": f"bootstrap observation remains blocking ({category}): {reason}",
        "source_sha256": source_sha,
        "bootstrap_observation": evidence,
    }
    row = {
        "path": path,
        "test_id": None,
        "class": "unmapped",
        "source_sha256": source_sha,
        "candidate_classification": (
            record.get("candidate_classification")
            if isinstance(record, Mapping)
            else "unobserved"
        ),
        "route_disposition": route.get("disposition") if isinstance(route, Mapping) else "unobserved",
        "route_classification_status": (
            route.get("classification_status") if isinstance(route, Mapping) else "unobserved"
        ),
        "route_reason": route.get("reason") if isinstance(route, Mapping) else None,
        "failure_class": failure.get("class") if isinstance(failure, Mapping) else "unknown",
        "exit_code": record.get("exit_code") if isinstance(record, Mapping) else None,
        "output_artifact_present": (
            record.get("output_artifact") is not None if isinstance(record, Mapping) else None
        ),
        "bootstrap_blocker": evidence,
    }
    return expectation, None, row


def _source_diagnostic_lines_from_raw_streams(
    stdout: bytes | None,
    stderr: bytes | None,
    *,
    route: Mapping[str, Any] | None = None,
) -> list[dict[str, str]]:
    """Return exact raw Mimi source diagnostics, excluding route-capability errors."""
    matches: list[dict[str, str]] = []
    observations = route.get("observations") if isinstance(route, Mapping) else None
    observed_route_rejection = False
    if (
        isinstance(route, Mapping)
        and route.get("disposition") == "rejected"
        and route.get("classification_status") == "observed"
        and isinstance(observations, list)
        and len(observations) == 1
        and isinstance(observations[0], Mapping)
        and observations[0].get("disposition") == "rejected"
    ):
        observation = observations[0]
        observed_line = observation.get("line")
        observed_reason = observation.get("reason")
        stderr_lines = (
            stderr.decode("utf-8", errors="replace").splitlines()
            if stderr is not None
            else []
        )
        observed_route_rejection = (
            isinstance(observed_line, str)
            and "\x1b" not in observed_line
            and observed_line in stderr_lines
            and isinstance(observed_reason, str)
            and observed_reason
            and route.get("reason") == observed_reason
            and any(
                observed_line.startswith(prefix)
                and observed_line[len(prefix):] == observed_reason
                for prefix in ROUTE_REJECTION_PREFIXES
            )
        )
    for stream_name, payload in (("stdout", stdout), ("stderr", stderr)):
        if payload is None:
            continue
        for raw_line in payload.splitlines(keepends=True):
            line_bytes = raw_line.rstrip(b"\r\n")
            line_text = line_bytes.decode("utf-8", errors="replace")
            detection_text = line_text
            while detection_text and (
                detection_text[0].isspace()
                or unicodedata.category(detection_text[0]) in {"Cc", "Cf"}
            ):
                detection_text = detection_text[1:]
            anchored_source_diagnostic = SOURCE_DIAGNOSTIC_RE.search(detection_text)
            unanchored_source_diagnostic = SOURCE_DIAGNOSTIC_DETECT_RE.search(line_text)
            if anchored_source_diagnostic or unanchored_source_diagnostic:
                code_match = SOURCE_DIAGNOSTIC_CODE_RE.match(detection_text)
                code = code_match.group("code").upper() if code_match is not None else None
                stripped_prefix = line_text[: len(line_text) - len(detection_text)]
                noncanonical_prefix = any(character not in " \t" for character in stripped_prefix)
                # MIR capability diagnostics can carry a Mimi source span while
                # describing route admission itself. Preserve those as rejection
                # evidence; a language diagnostic such as E0432 remains blocking
                # even when a forged route marker is also present.
                if (
                    observed_route_rejection
                    and code in CANONICAL_MIR_ROUTE_CODES
                    and anchored_source_diagnostic is not None
                    and not noncanonical_prefix
                ):
                    continue
                matches.append(
                    {
                        "stream": stream_name,
                        "text": line_text,
                        "bytes_base64": base64.b64encode(raw_line).decode("ascii"),
                        "bytes_sha256": _sha256(raw_line),
                    }
                )
    return matches


def _bootstrap_validate_row(
    *,
    index: int,
    source: Mapping[str, Any],
    record: Mapping[str, Any],
    root: Path,
    logs_root: Path,
    reported_mimi: str,
    reported_mimi_sha256: str,
    census: Any,
    source_snapshot_stable: bool,
) -> tuple[dict[str, Any], str | None, dict[str, Any]]:
    """Derive only canonical/rejection authorities from reproducible raw observations."""
    path = _normalized_repo_path(source.get("path"), label="candidate path")
    if record.get("path") != path or source.get("route_candidate") is not True:
        raise GenerationError(f"{path}: bootstrap rows must preserve the exact route-candidate path")
    for key, value in source.items():
        if record.get(key) != value:
            raise GenerationError(f"{path}: bootstrap record field {key!r} differs from its frozen inventory")
    if (
        not isinstance(record.get("input_index"), int)
        or isinstance(record.get("input_index"), bool)
        or record.get("input_index") != index
    ):
        raise GenerationError(f"{path}: bootstrap input_index is missing, duplicated or out of order")
    source_sha = source.get("sha256")
    if not isinstance(source_sha, str) or SHA256_RE.fullmatch(source_sha) is None:
        raise GenerationError(f"{path}: current source inventory lacks a canonical SHA-256")
    if _source_file_sha256(root, path) != source_sha:
        raise GenerationError(f"{path}: current source hash differs from the frozen full inventory")

    # A row may be unobserved, but its absence of observations remains explicit
    # and blocks strict acceptance; it is never synthesized into a passing row.
    try:
        stdout = _read_capture(record.get("stdout_raw"), logs_root=logs_root, label=f"{path} stdout")
        stderr = _read_capture(record.get("stderr_raw"), logs_root=logs_root, label=f"{path} stderr")
    except GenerationError:
        if record.get("run_status") != "completed":
            return _bootstrap_blocked_row(
                index=index,
                source=source,
                source_sha=source_sha,
                record=record,
                category="unobserved",
                reason="raw stdout/stderr captures are unavailable for a non-completed run",
            )
        raise
    if record.get("stdout") != stdout.decode("utf-8", errors="replace"):
        raise GenerationError(f"{path}: bootstrap stdout rendering differs from exact raw bytes")
    if record.get("stderr") != stderr.decode("utf-8", errors="replace"):
        raise GenerationError(f"{path}: bootstrap stderr rendering differs from exact raw bytes")
    stdout_text = stdout.decode("utf-8", errors="replace")
    stderr_text = stderr.decode("utf-8", errors="replace")
    route = census._route_observation(
        stdout_text, stderr_text, record.get("exit_code"), record.get("run_status")
    )
    if record.get("route") != route:
        raise GenerationError(f"{path}: bootstrap route cannot be reproduced from raw output")
    failure = census._failure_classification(
        run_status=str(record.get("run_status")),
        exit_code=record.get("exit_code"),
        text=stderr_text + "\n" + stdout_text,
        target_kind=str(source.get("target_kind")),
        route=route,
    )
    if record.get("failure_classification") != failure:
        raise GenerationError(f"{path}: bootstrap failure class cannot be reproduced from raw output")
    function_observation = census._function_observations(
        stdout_text, stderr_text, record.get("dispatch_stats"), route
    )
    per_function = record.get("per_function")
    if not isinstance(per_function, Mapping):
        raise GenerationError(f"{path}: bootstrap per-function observation is missing")
    for key in ("status", "named_roster_complete", "has_fallback", "consistency", "counts", "functions"):
        if per_function.get(key) != function_observation.get(key):
            raise GenerationError(f"{path}: bootstrap per-function field {key!r} is not reproducible")

    if record.get("run_status") != "completed":
        return _bootstrap_blocked_row(
            index=index,
            source=source,
            source_sha=source_sha,
            record=record,
            stdout=stdout,
            stderr=stderr,
            route=route,
            failure=failure,
            category="unobserved",
            reason="build did not complete, so no route authority can be inferred",
        )

    mimi_identity = record.get("mimi_executable_identity")
    identity_observations = (
        mimi_identity.get("before"), mimi_identity.get("after")
    ) if isinstance(mimi_identity, Mapping) else ()
    mimi_stable = (
        isinstance(mimi_identity, Mapping)
        and mimi_identity.get("path") == reported_mimi
        and mimi_identity.get("sha256_expected") == reported_mimi_sha256
        and mimi_identity.get("matches_expected") is True
        and len(identity_observations) == 2
        and all(
            isinstance(item, Mapping)
            and item.get("path") == reported_mimi
            and item.get("resolved_path") == reported_mimi
            and item.get("sha256") == reported_mimi_sha256
            for item in identity_observations
        )
    )
    row_source_stable = (
        record.get("sha256") == source_sha
        and record.get("source_hash_before_execution") == source_sha
        and record.get("source_hash_after_execution") == source_sha
        and record.get("source_stable_for_input") is True
        and source.get("present") is True
        and source.get("presence_kind") == "regular_file"
        and source.get("buildable") is True
        and source.get("target_kind") in {"executable", "shared_library_probe"}
        and record.get("target_kind") == source.get("target_kind")
    )
    if not source_snapshot_stable or not row_source_stable or not mimi_stable:
        reasons = []
        if not source_snapshot_stable:
            reasons.append("report source snapshot is globally unstable")
        if not row_source_stable:
            reasons.append("candidate source was missing, unsupported, or changed during observation")
        if not mimi_stable:
            reasons.append("Mimi executable identity is not stable and source-bound")
        return _bootstrap_blocked_row(
            index=index,
            source=source,
            source_sha=source_sha,
            record=record,
            stdout=stdout,
            stderr=stderr,
            route=route,
            failure=failure,
            category="unstable",
            reason="; ".join(reasons),
        )

    try:
        output_path = _record_output_path(record, source, root, reported_mimi)
    except GenerationError as error:
        return _bootstrap_blocked_row(
            index=index,
            source=source,
            source_sha=source_sha,
            record=record,
            stdout=stdout,
            stderr=stderr,
            route=route,
            failure=failure,
            category="unknown_command",
            reason=str(error),
        )

    route_disposition = route.get("disposition")
    route_status = route.get("classification_status")
    candidate_classification = record.get("candidate_classification")
    output = record.get("output_artifact")
    diagnostic_codes = census._diagnostic_codes(stderr_text + "\n" + stdout_text)
    raw_source_diagnostic_lines = _source_diagnostic_lines_from_raw_streams(
        stdout, stderr, route=route
    )
    if raw_source_diagnostic_lines:
        return _bootstrap_blocked_row(
            index=index,
            source=source,
            source_sha=source_sha,
            record=record,
            stdout=stdout,
            stderr=stderr,
            route=route,
            failure=failure,
            category="source_diagnostic_pending_semantic_authorization",
            reason=(
                "raw Mimi source-diagnostic line is present in captured process output; "
                "bootstrap cannot grant route-rejection or semantic authority"
            ),
        )

    test_id = _test_id(index, path)
    if (
        route_disposition == "canonical"
        and route_status == "observed"
        and candidate_classification == "canonical"
        and record.get("exit_code") == 0
        and failure.get("class") == "none"
        and per_function.get("status") == "complete_named"
        and per_function.get("named_roster_complete") is True
        and per_function.get("has_fallback") is False
        and isinstance(per_function.get("consistency"), Mapping)
        and bool(per_function.get("consistency"))
        and all(value is True for value in per_function["consistency"].values())
        and isinstance(route.get("mir_digest"), str)
        and CANONICAL_DIGEST_RE.fullmatch(str(route.get("mir_digest"))) is not None
        and isinstance(output, Mapping)
        and isinstance(output.get("sha256"), str)
        and SHA256_RE.fullmatch(str(output.get("sha256"))) is not None
        and isinstance(output.get("bytes"), int)
        and not isinstance(output.get("bytes"), bool)
        and output.get("bytes", 0) > 0
    ):
        rust_test = _render_required_compile_test(
            source,
            test_id,
            f"target/route-census-authorities-{index:04d}.out",
        )
        assertion_line = _line_number_for_literal(
            rust_test, "canonical route disposition: canonical ("
        )
        if not census._required_compile_test_authority_is_valid(
            test_id=test_id,
            source_path=path,
            target_kind=str(source["target_kind"]),
            source_text=rust_test,
            assertion_line=assertion_line,
        ):
            raise GenerationError(f"{path}: bootstrap canonical test fails the strict source recognizer")
        expectation = {
            "class": "required_compile",
            "reason": "bootstrap observed a stable exact-source default canonical build; strict census must execute this assertion",
            "source_sha256": source_sha,
            "authority": {
                "test_id": test_id,
                "assertion": {"path": EXPECTED_TEST_PATH, "line": assertion_line, "sha256": ""},
            },
        }
        row = {
            "path": path,
            "test_id": test_id,
            "class": "required_compile",
            "source_sha256": source_sha,
            "candidate_classification": "canonical",
            "route_disposition": "canonical",
            "route_classification_status": "observed",
            "route_reason": route.get("reason"),
            "failure_class": "none",
            "exit_code": 0,
            "output_artifact_present": True,
            "observation": {
                "stdout_sha256": _sha256(stdout),
                "stderr_sha256": _sha256(stderr),
                "source_stable_for_input": True,
                "mimi_executable_sha256": reported_mimi_sha256,
                "output_path": output_path,
                "output_artifact": dict(output),
            },
        }
        return expectation, rust_test, row

    if (
        route_disposition == "rejected"
        and route_status == "observed"
        and candidate_classification == "intentional_reject"
        and isinstance(record.get("exit_code"), int)
        and not isinstance(record.get("exit_code"), bool)
        and record.get("exit_code", 0) > 0
        and failure.get("class") in {"capability", "library_target"}
        and failure.get("status") == "classified"
        and per_function.get("has_fallback") is False
        and output is None
        and bool(diagnostic_codes)
    ):
        try:
            rejection_evidence = _rejection_output_absence_evidence(
                record, source, index, output_path
            )
        except GenerationError as error:
            return _bootstrap_blocked_row(
                index=index,
                source=source,
                source_sha=source_sha,
                record=record,
                stdout=stdout,
                stderr=stderr,
                route=route,
                failure=failure,
                category="rejection_artifact_unproven",
                reason=str(error),
            )
        if not census._rejection_artifact_evidence_valid(
            rejection_evidence,
            command=record.get("command"),
            input_index=index,
            target_kind=source.get("target_kind"),
            mimi_identity=mimi_identity,
        ):
            return _bootstrap_blocked_row(
                index=index,
                source=source,
                source_sha=source_sha,
                record=record,
                stdout=stdout,
                stderr=stderr,
                route=route,
                failure=failure,
                category="rejection_artifact_unproven",
                reason="exact output path absence evidence does not satisfy the collector contract",
            )
        observations = route.get("observations")
        if not isinstance(observations, list) or len(observations) != 1:
            return _bootstrap_blocked_row(
                index=index,
                source=source,
                source_sha=source_sha,
                record=record,
                stdout=stdout,
                stderr=stderr,
                route=route,
                failure=failure,
                category="unknown_route_observation",
                reason="observed rejection does not have exactly one raw route marker",
            )
        observed_line = observations[0].get("line")
        route_reason = route.get("reason")
        plain_route_line = census._strip_ansi(observed_line) if isinstance(observed_line, str) else None
        if (
            not isinstance(route_reason, str)
            or not route_reason
            or observations[0].get("reason") != route_reason
            or observations[0].get("disposition") != "rejected"
            or not isinstance(observed_line, str)
            or "\x1b" in observed_line
            or plain_route_line not in stderr_text.splitlines()
            or not any(
                str(plain_route_line).startswith(prefix)
                and str(plain_route_line)[len(prefix):] == route_reason
                for prefix in ROUTE_REJECTION_PREFIXES
            )
        ):
            return _bootstrap_blocked_row(
                index=index,
                source=source,
                source_sha=source_sha,
                record=record,
                stdout=stdout,
                stderr=stderr,
                route=route,
                failure=failure,
                category="unknown_route_observation",
                reason="rejection route line is not one exact plain-text stderr observation",
            )
        observed_diag_lines: list[str] = []
        for code in diagnostic_codes:
            matching = [
                line for line in stderr_text.splitlines()
                if code in census._diagnostic_codes(line)
            ]
            if not matching:
                return _bootstrap_blocked_row(
                    index=index,
                    source=source,
                    source_sha=source_sha,
                    record=record,
                    stdout=stdout,
                    stderr=stderr,
                    route=route,
                    failure=failure,
                    category="rejection_diagnostic_unbound",
                    reason=f"diagnostic code {code} is absent from exact stderr lines",
                )
            observed_diag_lines.extend(matching)
        required_substrings = list(dict.fromkeys([*observed_diag_lines, str(plain_route_line)]))
        rust_test, assertion_line = _render_expected_rejection_test(
            source,
            test_id,
            f"target/route-census-authorities-{index:04d}.out",
            route_reason,
            diagnostic_codes,
            required_substrings,
        )
        if not census._expected_rejection_test_authority_is_valid(
            test_id=test_id,
            source_path=path,
            target_kind=str(source["target_kind"]),
            source_text=rust_test,
            assertion_line=assertion_line,
            expected_diagnostic_codes=diagnostic_codes,
            required_output_substrings=required_substrings,
            expected_route_reason=route_reason,
        ):
            raise GenerationError(f"{path}: bootstrap rejection test fails the strict source recognizer")
        expectation = {
            "class": "expected_build_rejection",
            "reason": "bootstrap observed an exact allowed default-route rejection; strict census must execute this assertion",
            "source_sha256": source_sha,
            "authority": {
                "test_id": test_id,
                "assertion": {"path": EXPECTED_TEST_PATH, "line": assertion_line, "sha256": ""},
            },
            "expected_exit_code": record["exit_code"],
            "expected_diagnostic_codes": diagnostic_codes,
            "required_output_substrings": required_substrings,
            "expected_route_disposition": "rejected",
            "expected_route_classification_status": "observed",
            "expected_route_reason": route_reason,
            "expected_failure_class": failure["class"],
            "artifact_absence_evidence": {
                "source": "collector_observed_before_and_after_build",
                "path": rejection_evidence["output_path"],
                "run_directory": rejection_evidence["scratch"]["path"],
                "before": rejection_evidence["before"],
                "after": rejection_evidence["after"],
                "mimi_executable_identity": mimi_identity,
                "direct_test_asserts_artifact_absence": False,
                "direct_test_asserts_route_reason_and_all_diagnostics": True,
            },
        }
        row = {
            "path": path,
            "test_id": test_id,
            "class": "expected_build_rejection",
            "source_sha256": source_sha,
            "candidate_classification": "intentional_reject",
            "route_disposition": "rejected",
            "route_classification_status": "observed",
            "route_reason": route_reason,
            "failure_class": failure["class"],
            "exit_code": record["exit_code"],
            "output_artifact_present": False,
            "artifact_absence_evidence": {
                "source": "collector_observed_before_and_after_build",
                "path": rejection_evidence["output_path"],
                "run_directory": rejection_evidence["scratch"]["path"],
                "before": rejection_evidence["before"],
                "after": rejection_evidence["after"],
                "mimi_executable_identity": mimi_identity,
            },
            "observation": {
                "stdout_sha256": _sha256(stdout),
                "stderr_sha256": _sha256(stderr),
                "source_stable_for_input": True,
                "mimi_executable_sha256": reported_mimi_sha256,
            },
        }
        return expectation, rust_test, row

    if route_disposition == "compatibility" or per_function.get("has_fallback") is True:
        category = "compatibility_or_fallback"
        reason = "observed compatibility route or fallback is not a canonical authority"
    elif route_disposition in {None, "unobserved"} or route_status == "unobserved":
        category = "unobserved"
        reason = "raw observation does not establish a route"
    elif candidate_classification in {None, "unknown", "unmapped"} or failure.get("class") == "unknown":
        category = "unknown_or_unmapped"
        reason = "observation has no recognized canonical or allowed rejection authority"
    else:
        category = "not_authorizable"
        reason = "observation does not satisfy a complete canonical or allowed-rejection contract"
    return _bootstrap_blocked_row(
        index=index,
        source=source,
        source_sha=source_sha,
        record=record,
        stdout=stdout,
        stderr=stderr,
        route=route,
        failure=failure,
        category=category,
        reason=reason,
    )


def generate(
    report_path: Path,
    root: Path,
    output_dir: Path,
    *,
    bootstrap_report: bool = False,
) -> dict[str, Any]:
    root = root.resolve(strict=True)
    report_path = report_path.resolve(strict=True)
    output_dir = Path(os.path.abspath(output_dir))
    if not root.is_dir():
        raise GenerationError(f"repository root is not a directory: {root}")
    if output_dir.exists():
        raise GenerationError(f"refusing to overwrite generated output directory: {output_dir}")
    if not output_dir.parent.is_dir():
        raise GenerationError(f"output parent directory does not exist: {output_dir.parent}")

    _validate_optional_cargo_target(root)
    census = _load_census_module(root)
    report, report_raw = _read_report(report_path)
    sources, _exclusions, source_scope, current_snapshot = _validate_report_identity(
        report,
        report_path,
        root,
        census,
        allow_unstable_snapshot=bootstrap_report,
    )
    records = report.get("records")
    if not isinstance(records, list) or len(records) != len(sources):
        raise GenerationError("full execution report records do not exactly cover the candidate denominator")
    source_paths = [str(item.get("path")) for item in sources]
    record_paths = [str(item.get("path")) if isinstance(item, Mapping) else "" for item in records]
    if len(set(source_paths)) != len(source_paths) or Counter(record_paths) != Counter(source_paths):
        raise GenerationError("full execution report has duplicate, missing or extra candidate records")
    if bootstrap_report and record_paths != source_paths:
        raise GenerationError("bootstrap report candidate records do not preserve exact inventory order")
    artifacts = report.get("artifacts")
    artifacts_root = artifacts.get("root") if isinstance(artifacts, Mapping) else None
    if not isinstance(artifacts_root, str):
        raise GenerationError("full execution report does not identify raw artifact storage")
    artifact_root = Path(artifacts_root)
    if not artifact_root.is_absolute():
        artifact_root = report_path.parent / artifact_root
    try:
        artifact_root = artifact_root.resolve(strict=True)
    except OSError as error:
        raise GenerationError("full execution report raw artifact storage is unavailable") from error
    if not artifact_root.is_dir() or artifact_root.is_symlink():
        raise GenerationError("full execution report raw artifact storage is not a regular directory")
    logs_root = artifact_root / "logs"
    if not logs_root.is_dir() or logs_root.is_symlink():
        raise GenerationError("full execution report raw build logs directory is unavailable")
    toolchain = report.get("toolchain")
    mimi = toolchain.get("mimi") if isinstance(toolchain, Mapping) else None
    reported_mimi = mimi.get("path") if isinstance(mimi, Mapping) else None
    reported_mimi_sha = mimi.get("sha256") if isinstance(mimi, Mapping) else None
    if (
        not isinstance(reported_mimi, str)
        or not Path(reported_mimi).is_absolute()
        or not isinstance(reported_mimi_sha, str)
        or SHA256_RE.fullmatch(reported_mimi_sha) is None
    ):
        raise GenerationError("full execution report does not identify its absolute Mimi binary")
    try:
        mimi_path = Path(reported_mimi).resolve(strict=True)
        mimi_metadata = mimi_path.lstat()
        current_mimi_sha = census.sha256_file(mimi_path)
    except OSError as error:
        raise GenerationError("reported Mimi compiler binary is no longer available") from error
    if (
        not stat.S_ISREG(mimi_metadata.st_mode)
        or not os.access(mimi_path, os.X_OK)
        or current_mimi_sha != reported_mimi_sha
    ):
        raise GenerationError("reported Mimi compiler binary is not the same current executable")
    reported_mimi = str(mimi_path)

    expectation_inputs: dict[str, Any] = {}
    test_blocks: list[str] = []
    row_manifest: list[dict[str, Any]] = []
    source_by_path = {str(item["path"]): item for item in sources}
    record_by_path = {str(item["path"]): item for item in records}
    rejection_run_directories: set[str] = set()
    source_diagnostic_output_paths: set[str] = set()
    generated_test_ids: set[str] = set()
    for index, path in enumerate(source_paths, 1):
        source = source_by_path[path]
        record = record_by_path[path]
        test_id = _test_id(index, path)
        if test_id in generated_test_ids:
            raise GenerationError(f"duplicate generated authority test id: {test_id}")
        generated_test_ids.add(test_id)
        if bootstrap_report:
            expectation, rust_test, row = _bootstrap_validate_row(
                index=index,
                source=source,
                record=record,
                root=root,
                logs_root=logs_root,
                reported_mimi=reported_mimi,
                reported_mimi_sha256=reported_mimi_sha,
                census=census,
                source_snapshot_stable=report.get("source_snapshot_stable") is True,
            )
        else:
            expectation, rust_test, row = _validate_row(
                index=index,
                source=source,
                record=record,
                root=root,
                logs_root=logs_root,
                reported_mimi=reported_mimi,
                reported_mimi_sha256=reported_mimi_sha,
                census=census,
                cargo_authority=report.get("cargo_test_toolchain_authority"),
            )
        if expectation.get("class") == "expected_source_diagnostic":
            output_path = expectation.get("expected_output_path")
            if not isinstance(output_path, str) or output_path in source_diagnostic_output_paths:
                raise GenerationError(f"{path}: source-diagnostic authority output path is missing or reused")
            source_diagnostic_output_paths.add(output_path)
        artifact_absence = row.get("artifact_absence_evidence")
        if isinstance(artifact_absence, Mapping):
            run_directory = artifact_absence.get("run_directory")
            if not isinstance(run_directory, str) or run_directory in rejection_run_directories:
                raise GenerationError(f"{path}: absent-output evidence directory is missing or reused across inputs")
            rejection_run_directories.add(run_directory)
        if rust_test is not None:
            test_blocks.append(rust_test.rstrip())
        row_manifest.append(row)
        expectation_inputs[path] = expectation

    bootstrap_blockers = [row for row in row_manifest if row.get("class") == "unmapped"]
    if bootstrap_report and bootstrap_blockers:
        test_blocks.append(
            "#[test]\n"
            "fn route_census_bootstrap_bundle_is_not_a_strict_authority() {\n"
            "    panic!(\"bootstrap observation bundle has unresolved candidates and cannot pass strict census\");\n"
            "}"
        )

    rust_source = (
        "// Generated by scripts/generate_route_census_authority.py.\n"
        "// One direct source-bound default-build authority per current route candidate.\n"
        "// Enable only with --features route-census-authorities.\n\n"
        + AUTHORITY_MIMI_HELPER_SOURCE
        + "\n"
        + ("\n\n".join(test_blocks) + "\n" if test_blocks else "")
    )
    rust_bytes = rust_source.encode("utf-8")
    rust_sha = _sha256(rust_bytes)
    for expectation in expectation_inputs.values():
        authority = expectation.get("authority")
        if isinstance(authority, Mapping):
            authority["assertion"]["sha256"] = rust_sha
    expectation_document = {"schema_version": 4, "inputs": expectation_inputs}
    expectation_bytes = (
        json.dumps(expectation_document, ensure_ascii=False, sort_keys=True, indent=2) + "\n"
    ).encode("utf-8")
    expectation_sha = _sha256(expectation_bytes)
    generator_path = Path(__file__).resolve(strict=True)
    generator_sha = _sha256(generator_path.read_bytes())
    generation_manifest = {
        "schema_version": 1,
        "status": "generated_not_executed",
        "generation_mode": "bootstrap_observation" if bootstrap_report else "strict_authority",
        "generator": {
            "path": "scripts/generate_route_census_authority.py",
            "sha256": generator_sha,
        },
        "report": {
            "path": str(report_path),
            "sha256": _sha256(report_raw),
            "collector_sha256": report["collector"]["sha256"],
            "manifest_sha256": report["manifest"]["sha256"],
            "source_snapshot_id": current_snapshot["snapshot_id"],
            "candidate_count": len(sources),
            "source_scope_exactly_once": source_scope.get("exactly_once"),
        },
        "authority_target": {
            "name": EXPECTED_TEST_NAME,
            "feature": EXPECTED_FEATURE_NAME,
            "path": EXPECTED_TEST_PATH,
            "wrapper_sha256": _sha256((root / EXPECTED_TEST_PATH).read_bytes()),
        },
        "outcomes": {
            "canonical": sum(row["class"] == "required_compile" for row in row_manifest),
            "expected_build_rejection": sum(row["class"] == "expected_build_rejection" for row in row_manifest),
            "expected_source_diagnostic": sum(row["class"] == "expected_source_diagnostic" for row in row_manifest),
            **({"unmapped": len(bootstrap_blockers)} if bootstrap_report else {}),
        },
        "artifacts": {
            EXPECTED_TEST_PATH: {"sha256": rust_sha, "bytes": len(rust_bytes)},
            EXPECTATIONS_FILENAME: {"sha256": expectation_sha, "bytes": len(expectation_bytes)},
        },
        "rows": row_manifest,
        **(
            {
                "bootstrap_observation": {
                    "status": "observation_only",
                    "report_was_not_counted_as_strict_pass": True,
                    "candidate_denominator_preserved": len(row_manifest),
                    "pending_blocker_count": len(bootstrap_blockers),
                    "pending_semantic_authorization_paths": [
                        row["path"]
                        for row in bootstrap_blockers
                        if isinstance(row.get("bootstrap_blocker"), Mapping)
                        and row["bootstrap_blocker"].get("category")
                        == "source_diagnostic_pending_semantic_authorization"
                    ],
                    "blockers": [row["bootstrap_blocker"] for row in bootstrap_blockers],
                    "strict_census_execution_required": True,
                }
            }
            if bootstrap_report
            else {}
        ),
        "execution_contract": (
            ("Bootstrap observations are not a passing report. " if bootstrap_report else "")
            + "Generated authorities have not been executed. Re-run the full census with these expectations; "
            "that gate must compile/run each exact test and independently prove route receipts, rejection "
            "diagnostics, output-artifact absence, and source stability. For observed Canonical route rejections, artifact absence "
            "is separately observed by the collector before and after the exact build in that input's fresh "
            "scratch directory; the direct test binds the route reason, every diagnostic code, and all required "
            "output text, and does not claim to observe filesystem absence. Typed expected_source_diagnostic "
            "rows are separately counted, have only inferred pre-route source errors and no Canonical route/roster, "
            "bind the complete raw stderr text, and assert nonzero Mimi exit plus absence of their deterministic "
            "isolated authority output path before and after the exact build. Bootstrap does not assign this "
            "semantic authorization; unresolved source diagnostics remain source-bound unmapped blockers."
        ),
    }
    manifest_bytes = (
        json.dumps(generation_manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n"
    ).encode("utf-8")

    temporary: Path | None = None
    def revalidate_before_publish() -> None:
        if _source_file_sha256(root, "scripts/generate_route_census_authority.py") != generator_sha:
            raise GenerationError("authority generator changed while the bundle was being generated")
        _validate_optional_cargo_target(root)
        _revalidate_current_inputs(
            report_path=report_path,
            report_raw=report_raw,
            root=root,
            census=census,
            sources=sources,
            exclusions=_exclusions,
            source_scope=source_scope,
            source_snapshot=current_snapshot,
            collector_sha256=report["collector"]["sha256"],
            mimi_path=mimi_path,
            mimi_sha256=reported_mimi_sha,
        )

    try:
        revalidate_before_publish()
        temporary = Path(tempfile.mkdtemp(prefix=f".{output_dir.name}.staging-", dir=output_dir.parent))
        target_file = temporary / EXPECTED_TEST_PATH
        target_file.parent.mkdir(parents=True, exist_ok=True)
        target_file.write_bytes(rust_bytes)
        (temporary / EXPECTATIONS_FILENAME).write_bytes(expectation_bytes)
        (temporary / GENERATION_MANIFEST_FILENAME).write_bytes(manifest_bytes)
        # Refuse to publish a test file that the strict source recognizers do not accept.
        staged_text = target_file.read_text(encoding="utf-8")
        for row in row_manifest:
            test_id = row.get("test_id")
            if test_id is None:
                if (
                    not bootstrap_report
                    or row.get("class") != "unmapped"
                    or not isinstance(row.get("bootstrap_blocker"), Mapping)
                ):
                    raise GenerationError(
                        f"only a bootstrap blocker may omit a direct authority test: {row.get('path')}"
                    )
                continue
            span = census._rust_test_function_span(staged_text, test_id)
            if span is None:
                raise GenerationError(f"generated Rust test declaration is not discoverable: {test_id}")
        revalidate_before_publish()
        if output_dir.exists() or output_dir.is_symlink():
            raise GenerationError(f"refusing to overwrite generated output directory: {output_dir}")
        os.rename(temporary, output_dir)
        temporary = None
    except Exception:
        if temporary is not None:
            shutil.rmtree(temporary, ignore_errors=True)
        raise

    return generation_manifest


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    report_mode = parser.add_mutually_exclusive_group(required=True)
    report_mode.add_argument(
        "--report",
        type=Path,
        help="full_execution report with complete current source-bound route authorities already proven",
    )
    report_mode.add_argument(
        "--bootstrap-report",
        type=Path,
        help="raw full_execution observations; unresolved rows remain blocking and are never granted authority",
    )
    parser.add_argument("--root", required=True, type=Path, help="repository root used for that census")
    parser.add_argument("--output-dir", required=True, type=Path, help="new staging directory; existing paths are refused")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    report_path = args.bootstrap_report or args.report
    bootstrap = args.bootstrap_report is not None
    try:
        manifest = generate(
            report_path,
            args.root,
            args.output_dir,
            bootstrap_report=bootstrap,
        )
    except (GenerationError, OSError) as error:
        print(f"[route-authority-generator] REFUSED: {error}", file=sys.stderr)
        return 2
    print(
        "[route-authority-generator] generated "
        f"{manifest['report']['candidate_count']} source-bound expectations and direct Rust tests "
        f"under {args.output_dir}; status=generated_not_executed "
        f"mode={manifest['generation_mode']} blockers={manifest['outcomes']['unmapped']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
